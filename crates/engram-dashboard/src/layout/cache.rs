//! The layout cache: in memory (a small LRU in [`crate::AppState`]) and on
//! disk, keyed by the layout input's digest ([`super::LayoutInput::digest`]),
//! with at most one computation per key in flight, at most
//! [`MAX_COMPUTING`] at once and at most [`MAX_QUEUED`] more waiting.
//!
//! **The dashboard's one declared write outside a store.** Every GET is
//! side-effect free *for stores* (they open read-only, nothing is created
//! under `.engramdb/` or the data dir), and that stays true: the disk cache
//! lives under the OS cache dir, `dirs::cache_dir()/engramdb/dashboard/layouts/`
//! (`engram_storage::paths::dashboard_layout_cache_dir`;
//! `ENGRAMDB_DASHBOARD_CACHE_DIR` overrides it, as `ENGRAMDB_DATA_DIR` does
//! the data dir, and the test harness points it at a temp dir), never under
//! a store, `.engramdb/` or the data dir. The directory is created 0700 when
//! the dashboard starts (whether or not a layout is ever asked for) and
//! refused (memory only, one warning) when it exists with group/other bits,
//! with another owner, is a symlink or is not a directory; files are written
//! 0600, temp file then rename. A file is read through one handle opened
//! with `O_NOFOLLOW | O_NONBLOCK` (a planted symlink or FIFO is a miss, never
//! followed or waited on), checked on that handle to be a regular file under
//! [`MAX_FILE`], and its LRU stamp is refreshed on the same handle. It holds
//! the layouts only: positions, label spots, scope names (`key` of each top
//! scope) and the digest, no memory text. `ENGRAMDB_DASHBOARD_LAYOUT_CACHE=0`
//! turns both caches off (the browser bench's cold figure).
//!
//! **Format.** `EDLC`, the [`super::LAYOUT_VERSION`] (u32 LE), the 64-byte
//! hex digest, then [`super::Cached::to_bytes`]: scope-force's width-free
//! core as raw little-endian doubles (exact, unlike a JSON parse), the
//! overview as its encoded JSON text.
//!
//! **Bounded.** At most [`MEM_ENTRIES`] layouts in memory and
//! [`DISK_BYTES`] on disk; past that the least recently used go (a disk hit
//! refreshes the file's mtime, eviction takes the oldest mtimes first).
//!
//! **Bounded work.** A key being computed is computed once: later requests
//! for it wait on the same result ([`Flight::Wait`]). At most
//! [`MAX_COMPUTING`] keys are computed at once, so slow inputs cannot fill
//! the blocking pool, whose other users are the store probes; up to
//! [`MAX_QUEUED`] more keys wait their turn ([`Lead::permit`], first come
//! first served), and a request for a new key past that is refused
//! ([`Flight::Full`]: `graph.rs` answers 503 with `Retry-After`, which the
//! page retries). Every request waits for its layout however long it takes:
//! the page has no other way to lay one out. A computation that fails (a
//! number that is not finite, or a panic) reaches every waiter as an error
//! and is not cached, so the next request computes it again.
//!
//! **Loss paths, all harmless for a cache and all declared here:** a file
//! that does not parse, carries another digest or another version is
//! ignored and overwritten by the next write; files of an older format
//! (`*.json`, layout version 1) are deleted by the next prune; two dashboards
//! pruning at once may delete more than needed; a read that races a prune
//! or a rename misses and recomputes; a write that fails (disk full,
//! permissions) is logged at debug and the layout is still served from
//! memory; two dashboards computing one layout both write it (the second
//! rename replaces the first with the same bytes); a temp file a crash left
//! behind is removed by the next prune once it is ten minutes old.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

use super::Cached;

/// One encoded layout as the page gets it: JSON text, spliced into the
/// response as is (see [`super::Cached`] for why it stays text).
pub type Encoded = String;

/// Layouts kept in memory.
pub const MEM_ENTRIES: usize = 32;
/// Bytes of layouts kept on disk.
pub const DISK_BYTES: u64 = 64 * 1024 * 1024;
/// Layouts computed at once (module docs, "Bounded work").
pub const MAX_COMPUTING: usize = 2;
/// Layouts waiting for a computing slot (module docs, "Bounded work").
pub const MAX_QUEUED: usize = 16;
/// A cache file larger than this is not read (none is written that large).
const MAX_FILE: u64 = 16 * 1024 * 1024;
/// A temp file older than this is a crash's leftover.
const STALE_TMP: Duration = Duration::from_secs(600);
/// A cache file's first bytes.
const MAGIC: &[u8; 4] = b"EDLC";

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A computation's result as its waiters see it: `None` until it lands.
pub type Outcome = Result<Arc<Cached>, Arc<str>>;
type Slot = watch::Receiver<Option<Outcome>>;

pub struct LayoutCache {
    enabled: bool,
    mem: Mutex<Vec<(String, Arc<Cached>)>>,
    /// `None`: memory only (no cache dir, or one refused).
    disk: Option<PathBuf>,
    flights: Arc<Mutex<HashMap<String, Slot>>>,
    /// [`MAX_COMPUTING`] permits: one per computation running.
    permits: Arc<Semaphore>,
    /// [`MAX_COMPUTING`] + [`MAX_QUEUED`] permits: one per key led, running
    /// or waiting for a computing permit.
    admits: Arc<Semaphore>,
}

/// What a request for an uncached layout does ([`LayoutCache::begin`]).
pub enum Flight {
    /// Another request is computing it: wait for that one.
    Wait(Slot),
    /// Compute it: take a [`Lead::permit`], run it on the blocking pool,
    /// then [`LayoutCache::land`] it.
    Lead(Lead),
    /// [`MAX_COMPUTING`] layouts are being computed and [`MAX_QUEUED`] more
    /// are waiting: refuse (the caller answers 503).
    Full,
}

/// The right to compute one key. Dropped without landing (the computation
/// panicked), it clears the key so the next request computes it again, and
/// its waiters see the channel close.
pub struct Lead {
    key: String,
    tx: watch::Sender<Option<Outcome>>,
    flights: Arc<Mutex<HashMap<String, Slot>>>,
    permits: Arc<Semaphore>,
    _admit: OwnedSemaphorePermit,
}

impl Lead {
    /// A receiver of this computation's result.
    pub fn subscribe(&self) -> Slot {
        self.tx.subscribe()
    }

    /// Wait for one of the [`MAX_COMPUTING`] computing slots; hold it while
    /// computing.
    pub async fn permit(&self) -> OwnedSemaphorePermit {
        Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .expect("the layout semaphore is never closed")
    }
}

impl Drop for Lead {
    fn drop(&mut self) {
        let mut f = self.flights.lock().unwrap_or_else(|e| e.into_inner());
        if f.get(&self.key)
            .is_some_and(|rx| rx.same_channel(&self.tx.subscribe()))
        {
            f.remove(&self.key);
        }
    }
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
            flights: Arc::new(Mutex::new(HashMap::new())),
            permits: Arc::new(Semaphore::new(MAX_COMPUTING)),
            admits: Arc::new(Semaphore::new(MAX_COMPUTING + MAX_QUEUED)),
        }
    }

    /// The disk cache's directory, when there is one.
    pub fn dir(&self) -> Option<&Path> {
        self.disk.as_deref()
    }

    /// A layout held in memory (no I/O: safe on the runtime).
    pub fn get_mem(&self, key: &str) -> Option<Arc<Cached>> {
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

    fn put_mem(&self, key: &str, v: Arc<Cached>) {
        if !self.enabled {
            return;
        }
        let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        mem.retain(|(k, _)| k != key);
        mem.push((key.to_string(), v));
        while mem.len() > MEM_ENTRIES {
            mem.remove(0);
        }
    }

    /// Join the computation of `key` in flight, or lead a new one if the
    /// queue has room (module docs, "Bounded work").
    pub fn begin(&self, key: &str) -> Flight {
        let mut f = self.flights.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rx) = f.get(key) {
            return Flight::Wait(rx.clone());
        }
        let Ok(admit) = Arc::clone(&self.admits).try_acquire_owned() else {
            return Flight::Full;
        };
        let (tx, rx) = watch::channel(None);
        f.insert(key.to_string(), rx);
        Flight::Lead(Lead {
            key: key.to_string(),
            tx,
            flights: Arc::clone(&self.flights),
            permits: Arc::clone(&self.permits),
            _admit: admit,
        })
    }

    /// Hand a led computation's result (or its error) to everyone waiting
    /// on it.
    pub fn land(&self, lead: Lead, v: Outcome) {
        // Out of the map first, so a request arriving now reads the memory
        // cache (filled before this) rather than a finished flight.
        self.flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&lead.key);
        let _ = lead.tx.send(Some(v));
    }

    /// The layout for `key`: from memory, else from disk, else computed
    /// (and stored in both; a failed computation is stored nowhere).
    /// Blocking: call it on the blocking pool.
    pub fn fill(
        &self,
        key: &str,
        compute: impl FnOnce() -> Result<Cached, String>,
    ) -> Result<Arc<Cached>, String> {
        if !self.enabled {
            return compute().map(Arc::new);
        }
        if let Some(v) = self.get_mem(key) {
            return Ok(v);
        }
        if let Some(v) = self.read_disk(key) {
            let v = Arc::new(v);
            self.put_mem(key, Arc::clone(&v));
            return Ok(v);
        }
        let v = Arc::new(compute()?);
        self.put_mem(key, Arc::clone(&v));
        if let Err(e) = self.write_disk(key, &v) {
            tracing::debug!("dashboard layout cache: write failed: {e}");
        }
        Ok(v)
    }

    fn file_of(&self, key: &str) -> Option<PathBuf> {
        // Digests are 64 hex digits; anything else never names a file.
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.disk.as_ref()?.join(format!("{key}.bin")))
    }

    fn read_disk(&self, key: &str) -> Option<Cached> {
        use std::io::Read;
        let path = self.file_of(key)?;
        let mut f = open_nofollow(&path).ok()?;
        let meta = f.metadata().ok()?;
        if !meta.is_file() || meta.len() > MAX_FILE {
            return None;
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        f.read_to_end(&mut bytes).ok()?;
        let body = bytes.strip_prefix(MAGIC.as_slice())?;
        let (version, body) = body.split_at_checked(4)?;
        let (stored, body) = body.split_at_checked(64)?;
        if version != super::LAYOUT_VERSION.to_le_bytes() || stored != key.as_bytes() {
            return None;
        }
        let v = Cached::from_bytes(body)?;
        // The overview is spliced into the response as text: a corrupt one
        // would make every `/graph` of this key invalid JSON until evicted.
        if let Cached::Overview(text) | Cached::Grid(text) = &v {
            serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
        }
        // Refresh the LRU stamp on the same handle; a failure only makes it
        // evict sooner.
        let _ = f.set_modified(SystemTime::now());
        Some(v)
    }

    fn write_disk(&self, key: &str, v: &Cached) -> std::io::Result<()> {
        let (Some(path), Some(dir)) = (self.file_of(key), self.disk.as_ref()) else {
            return Ok(());
        };
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&super::LAYOUT_VERSION.to_le_bytes());
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(&v.to_bytes());
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
            f.write_all(&bytes)?;
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

/// Open `path` for reading without following a symlink or blocking on a
/// FIFO (the `state_file` rule).
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::File::options();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    opts.open(path)
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
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("group or other can access it".into());
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err("another user owns it".into());
        }
    }
    Ok(())
}

/// Delete the least recently used layouts until the directory holds at most
/// `cap` bytes (to three quarters of it, so a full cache does not prune on
/// every write), stale temp files and files of the older format.
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
        // Only names this cache writes: the directory may be one the user
        // pointed `ENGRAMDB_DASHBOARD_CACHE_DIR` at, holding files of theirs.
        match cache_file_kind(&name) {
            Some(FileKind::Tmp) => {
                if now.duration_since(mtime).unwrap_or_default() > STALE_TMP {
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }
            Some(FileKind::OldJson) => {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            Some(FileKind::Bin) => {}
            None => continue,
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

/// A file name this cache writes, by kind.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileKind {
    /// `<key>.bin`: a layout.
    Bin,
    /// `<key>.json`: the older format's layout.
    OldJson,
    /// `.<key>.<pid>.<seq>.tmp`: a write in progress (or a crashed one).
    Tmp,
}

fn is_key(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Which of the cache's own names `name` is: `<64 hex>.bin`, `<64 hex>.json`
/// or `.<64 hex>.<digits>.<digits>.tmp`; `None` for anything else, which
/// prune never touches.
pub(crate) fn cache_file_kind(name: &str) -> Option<FileKind> {
    if let Some(key) = name.strip_suffix(".bin") {
        return is_key(key).then_some(FileKind::Bin);
    }
    if let Some(key) = name.strip_suffix(".json") {
        return is_key(key).then_some(FileKind::OldJson);
    }
    let rest = name.strip_prefix('.')?.strip_suffix(".tmp")?;
    let mut parts = rest.split('.');
    let ok = parts.next().is_some_and(is_key)
        && parts.next().is_some_and(is_number)
        && parts.next().is_some_and(is_number)
        && parts.next().is_none();
    ok.then_some(FileKind::Tmp)
}

#[cfg(test)]
pub(crate) fn prune_for_test(dir: &Path, cap: u64) {
    prune(dir, cap);
}
