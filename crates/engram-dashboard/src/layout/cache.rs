//! The layout cache: in memory (a small LRU in [`crate::AppState`]) and on
//! disk, keyed by the layout input's digest ([`super::LayoutInput::digest`]).
//!
//! **The dashboard's one declared write outside a store.** Every GET is
//! side-effect free *for stores* (they open read-only, nothing is created
//! under `.engramdb/` or the data dir), and that stays true: the disk cache
//! lives under the OS cache dir, `dirs::cache_dir()/engramdb/dashboard/layouts/`
//! (`engram_storage::paths::dashboard_layout_cache_dir`;
//! `ENGRAMDB_DASHBOARD_CACHE_DIR` overrides it, as `ENGRAMDB_DATA_DIR` does
//! the data dir, and the test harness points it at a temp dir), never under
//! a store, `.engramdb/` or the data dir. The directory is created 0700 and
//! refused (memory only, one warning) when it exists with group/other bits,
//! is a symlink or is not a directory; files are written 0600, temp file
//! then rename. It holds the encoded layouts only: positions, label spots,
//! scope names (`key` of each top scope) and the digest, no memory text.
//! `ENGRAMDB_DASHBOARD_LAYOUT_CACHE=0` turns both caches off (the browser
//! bench's cold figure).
//!
//! **Bounded.** At most [`MEM_ENTRIES`] layouts in memory and
//! [`DISK_BYTES`] on disk; past that the least recently used go (a disk hit
//! refreshes the file's mtime, eviction takes the oldest mtimes first).
//!
//! **Loss paths, all harmless for a cache and all declared here:** a file
//! that does not parse, carries another digest or another
//! [`super::LAYOUT_VERSION`] is ignored and overwritten by the next write;
//! two dashboards pruning at once may delete more than needed; a read that
//! races a prune or a rename misses and recomputes; a write that fails
//! (disk full, permissions) is logged at debug and the layout is still
//! served from memory; two requests for one uncached layout both compute it
//! (no single-flight: the second write replaces the first with the same
//! bytes); a temp file a crash left behind is removed by the next prune once
//! it is ten minutes old.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::Value;

/// One encoded layout: its JSON text, served as is. Kept as text rather
/// than a `serde_json::Value` because serde_json (built without
/// `float_roundtrip`) does not parse every double back to the same bits,
/// and the page needs the layout's exact numbers.
pub type Encoded = Arc<str>;

/// Layouts kept in memory.
pub const MEM_ENTRIES: usize = 32;
/// Bytes of layouts kept on disk.
pub const DISK_BYTES: u64 = 64 * 1024 * 1024;
/// A cache file larger than this is not read (none is written that large).
const MAX_FILE: u64 = 16 * 1024 * 1024;
/// A temp file older than this is a crash's leftover.
const STALE_TMP: Duration = Duration::from_secs(600);

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub struct LayoutCache {
    enabled: bool,
    mem: Mutex<Vec<(String, Encoded)>>,
    /// `None`: memory only (no cache dir, or one refused).
    disk: Option<PathBuf>,
}

impl LayoutCache {
    /// The cache the dashboard runs with: the environment decides whether
    /// it is on and where its directory is (module docs).
    pub fn from_env() -> Self {
        let enabled = !matches!(
            std::env::var("ENGRAMDB_DASHBOARD_LAYOUT_CACHE")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "0" | "false" | "no" | "off"
        );
        let dir = engramdb::storage::paths::dashboard_layout_cache_dir().ok();
        Self::new(dir, enabled)
    }

    /// A cache over `dir` (`None`: memory only).
    pub fn new(dir: Option<PathBuf>, enabled: bool) -> Self {
        let disk = if enabled {
            dir.and_then(|d| match prepare_dir(&d) {
                Ok(()) => Some(d),
                Err(why) => {
                    tracing::warn!(
                        "dashboard layout cache: not using {} ({why}); layouts are cached in memory only",
                        d.display()
                    );
                    None
                }
            })
        } else {
            None
        };
        Self {
            enabled,
            mem: Mutex::new(Vec::new()),
            disk,
        }
    }

    /// The disk cache's directory, when there is one.
    pub fn dir(&self) -> Option<&Path> {
        self.disk.as_deref()
    }

    /// A layout held in memory (no I/O: safe on the runtime).
    pub fn get_mem(&self, key: &str) -> Option<Encoded> {
        if !self.enabled {
            return None;
        }
        let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        let at = mem.iter().position(|(k, _)| k == key)?;
        let hit = mem.remove(at);
        let v = Arc::clone(&hit.1);
        mem.push(hit);
        Some(v)
    }

    fn put_mem(&self, key: &str, v: Encoded) {
        let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        mem.retain(|(k, _)| k != key);
        mem.push((key.to_string(), v));
        while mem.len() > MEM_ENTRIES {
            mem.remove(0);
        }
    }

    /// The layout for `key`: from memory, else from disk, else computed
    /// (and stored in both). Blocking: call it on the blocking pool.
    pub fn get_or_compute(&self, key: &str, compute: impl FnOnce() -> String) -> Encoded {
        if !self.enabled {
            return compute().into();
        }
        if let Some(v) = self.get_mem(key) {
            return v;
        }
        if let Some(v) = self.read_disk(key) {
            let v: Encoded = v.into();
            self.put_mem(key, Arc::clone(&v));
            return v;
        }
        let v: Encoded = compute().into();
        self.put_mem(key, Arc::clone(&v));
        if let Err(e) = self.write_disk(key, &v) {
            tracing::debug!("dashboard layout cache: write failed: {e}");
        }
        v
    }

    fn file_of(&self, key: &str) -> Option<PathBuf> {
        // Digests are hex; anything else never names a file.
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.disk.as_ref()?.join(format!("{key}.json")))
    }

    fn read_disk(&self, key: &str) -> Option<String> {
        let path = self.file_of(key)?;
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if !meta.file_type().is_file() || meta.len() > MAX_FILE {
            return None;
        }
        let text = std::fs::read_to_string(&path).ok()?;
        // Parsed only to check the header; the text itself is served.
        let v: Value = serde_json::from_str(&text).ok()?;
        let current = v.get("key").and_then(Value::as_str) == Some(key)
            && v.get("v").and_then(Value::as_u64) == Some(u64::from(super::LAYOUT_VERSION));
        if !current {
            return None;
        }
        // Refresh the LRU stamp; a failure only makes it evict sooner.
        if let Ok(f) = std::fs::File::options().append(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        Some(text)
    }

    fn write_disk(&self, key: &str, v: &str) -> std::io::Result<()> {
        let (Some(path), Some(dir)) = (self.file_of(key), self.disk.as_ref()) else {
            return Ok(());
        };
        let bytes = v.as_bytes();
        let tmp = dir.join(format!(
            ".{key}.{}.{}.tmp",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mut opts = std::fs::File::options();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let written = (|| {
            use std::io::Write;
            let mut f = opts.open(&tmp)?;
            f.write_all(bytes)?;
            std::fs::rename(&tmp, &path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written?;
        prune(dir, DISK_BYTES);
        Ok(())
    }
}

/// Create the cache directory 0700, or say why it cannot be used.
fn prepare_dir(dir: &Path) -> Result<(), String> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir).map_err(|e| e.to_string())?;
    let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("not a directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("group or other can access it".into());
        }
    }
    Ok(())
}

/// Delete the least recently used layouts until the directory holds at most
/// `cap` bytes (to three quarters of it, so a full cache does not prune on
/// every write), and stale temp files.
fn prune(dir: &Path, cap: u64) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".tmp") {
            if now.duration_since(mtime).unwrap_or_default() > STALE_TMP {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        if !name.ends_with(".json") {
            continue;
        }
        total += meta.len();
        files.push((mtime, meta.len(), path));
    }
    if total <= cap {
        return;
    }
    files.sort_by_key(|f| f.0);
    let target = cap / 4 * 3;
    for (_, len, path) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

#[cfg(test)]
pub(crate) fn prune_for_test(dir: &Path, cap: u64) {
    prune(dir, cap);
}
