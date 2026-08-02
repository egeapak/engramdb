//! Compressed archives of Claude Code session transcripts.
//!
//! Claude Code prunes its own transcripts (there is a `~/.claude/.last-cleanup`
//! marker). Once a transcript is gone, the conversation is unrecoverable and
//! any memory derived from it has no evidence behind it. Archiving a copy when
//! a session *ends* is what makes it possible to harvest a conversation weeks
//! later, and what lets `harvest ledger export` show the source behind a
//! memory that was created months ago.
//!
//! Archiving at harvest time would protect nothing — you necessarily still
//! hold the transcript at that moment. The point of this module is the copy
//! taken at `SessionEnd`.
//!
//! ## Location
//!
//! Archives live at
//! `<global_data_dir>/projects/<root_id>/transcripts/<session-id>.jsonl.zst`,
//! **never** under the project's `.engramdb/`. See
//! [`paths::transcript_archive_dir`](crate::paths::transcript_archive_dir) for
//! why that distinction is load-bearing rather than cosmetic.
//!
//! ## Bounds
//!
//! Real transcripts compress about 4.5x — noticeably less than the ~10x one
//! might assume, because the bulk of a transcript is high-entropy tool output
//! rather than repetitive structure. A 2.9 MB session lands around 650 KB, so
//! the default 2 GiB budget holds on the order of a few thousand sessions.
//! Both an age limit and a total-size limit apply, with oldest-first eviction.

use crate::error::Result;
use crate::paths;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Compression level. 3 is zstd's default: it runs at roughly gzip -1 speed
/// while compressing better than gzip -9. Session teardown must not stall, so
/// throughput matters more here than the last few percent of ratio.
const ZSTD_LEVEL: i32 = 3;

/// Extension used for archived transcripts.
pub const ARCHIVE_EXT: &str = "jsonl.zst";

/// A stored archive, as recorded in the harvest ledger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveRef {
    /// File name within the project's archive directory.
    pub file_name: String,
    /// Size of the compressed archive.
    pub bytes: u64,
    /// Size of the original transcript.
    pub original_bytes: u64,
    /// SHA-256 of the *uncompressed* transcript, so an export can be shown to
    /// round-trip intact.
    pub sha256: String,
    pub archived_at: DateTime<Utc>,
}

impl ArchiveRef {
    /// Compression ratio achieved, for reporting.
    pub fn ratio(&self) -> f64 {
        if self.bytes == 0 {
            return 0.0;
        }
        self.original_bytes as f64 / self.bytes as f64
    }
}

/// Absolute path of an archive within a project's archive directory.
pub fn archive_path(project_id: &str, session_id: &str) -> Result<PathBuf> {
    Ok(paths::transcript_archive_dir(project_id)?.join(format!("{session_id}.{ARCHIVE_EXT}")))
}

/// Compress `transcript` into the project's archive directory.
///
/// Streams through a fixed buffer rather than reading the transcript into
/// memory: transcripts reach tens of megabytes, and this runs during session
/// teardown. Writes to a temp file and renames, so a crash mid-write cannot
/// leave a truncated archive that later reads as a valid-but-short session.
///
/// Re-archiving an existing session overwrites it, which makes the operation
/// idempotent for a hook that may fire more than once.
pub fn archive_transcript(
    project_id: &str,
    session_id: &str,
    transcript: &Path,
) -> Result<ArchiveRef> {
    let dest = archive_path(project_id, session_id)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Process-unique temp name: a SessionEnd hook can fire twice for the same
    // session, and two writers sharing one temp path would interleave into a
    // corrupt archive that still renames into place looking valid.
    let tmp = dest.with_extension(format!("tmp{}", std::process::id()));

    // A partial temp file is invisible to `list_archives` (it does not end in
    // `.jsonl.zst`), so a failure that left one behind would leak bytes the
    // size budget can never reclaim. Clean up on every error path.
    let written = compress_into(transcript, &tmp);
    let (hasher, original_bytes) = match written {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };

    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    let bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);

    Ok(ArchiveRef {
        file_name: dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        bytes,
        original_bytes,
        sha256: hex_digest(hasher),
        archived_at: Utc::now(),
    })
}

/// Stream `src` through zstd into `tmp`, returning the plaintext hasher and
/// the original size.
///
/// Hashes as the bytes stream past, so integrity costs one pass rather than a
/// second read of a file that can be tens of megabytes.
fn compress_into(src: &Path, tmp: &Path) -> Result<(Sha256, u64)> {
    use std::io::{Read, Write};

    let mut input = std::fs::File::open(src)?;
    let original_bytes = input.metadata().map(|m| m.len()).unwrap_or(0);
    let out = std::fs::File::create(tmp)?;
    let mut encoder = zstd::stream::Encoder::new(out, ZSTD_LEVEL)?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        encoder.write_all(&buf[..n])?;
    }
    encoder.finish()?;
    Ok((hasher, original_bytes))
}

/// Decompress an archive to `dest`.
///
/// Returns the SHA-256 of the restored bytes so the caller can compare it
/// against the [`ArchiveRef`] recorded at archive time.
pub fn export_archive(project_id: &str, session_id: &str, dest: &Path) -> Result<String> {
    let src = archive_path(project_id, session_id)?;
    let input = std::fs::File::open(&src)?;
    let mut decoder = zstd::stream::Decoder::new(input)?;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = std::fs::File::create(dest)?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        use std::io::{Read, Write};
        let n = decoder.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    Ok(hex_digest(hasher))
}

/// Lowercase hex of a finished SHA-256.
///
/// sha2 0.11's `finalize` returns a `hybrid_array::Array` that no longer
/// implements `LowerHex`, so `{:x}` does not compile — same gotcha already
/// documented in `project_id::hash_to_id`.
fn hex_digest(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Remove one session's archive. Returns whether a file was deleted.
pub fn remove_archive(project_id: &str, session_id: &str) -> Result<bool> {
    let path = archive_path(project_id, session_id)?;
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)?;
    Ok(true)
}

/// One archive on disk, as discovered by a directory scan.
#[derive(Debug, Clone)]
pub struct StoredArchive {
    pub session_id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: DateTime<Utc>,
}

/// List every archive for a project, oldest first.
///
/// A missing directory lists as empty: archiving may be disabled, or the
/// project may simply never have ended a session.
pub fn list_archives(project_id: &str) -> Result<Vec<StoredArchive>> {
    let dir = paths::transcript_archive_dir(project_id)?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let suffix = format!(".{ARCHIVE_EXT}");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(session_id) = name.strip_suffix(&suffix) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta
            .modified()
            .ok()
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(Utc::now);
        out.push(StoredArchive {
            session_id: session_id.to_string(),
            path,
            bytes: meta.len(),
            modified,
        });
    }
    out.sort_by_key(|a| a.modified);
    Ok(out)
}

/// Total bytes currently held by a project's archives.
pub fn total_bytes(project_id: &str) -> Result<u64> {
    Ok(list_archives(project_id)?.iter().map(|a| a.bytes).sum())
}

/// What an eviction pass did (or, in dry-run, would do).
#[derive(Debug, Clone, Default)]
pub struct PruneOutcome {
    pub removed: Vec<String>,
    pub bytes_freed: u64,
    pub bytes_remaining: u64,
}

/// Evict archives past the retention limits, oldest first.
///
/// Age is applied before size, so an over-budget store first drops genuinely
/// stale archives and only then eats into recent ones. `max_bytes == 0`
/// disables the size limit; `retention_days == None` disables the age limit.
pub fn prune_archives(
    project_id: &str,
    retention_days: Option<u64>,
    max_bytes: u64,
    dry_run: bool,
) -> Result<PruneOutcome> {
    let archives = list_archives(project_id)?;
    let mut outcome = PruneOutcome::default();
    let mut keep: Vec<&StoredArchive> = Vec::new();

    let cutoff = retention_days.map(|d| Utc::now() - Duration::days(d as i64));
    for archive in &archives {
        let too_old = cutoff.is_some_and(|c| archive.modified < c);
        if too_old {
            outcome.removed.push(archive.session_id.clone());
            outcome.bytes_freed += archive.bytes;
        } else {
            keep.push(archive);
        }
    }

    if max_bytes > 0 {
        let mut remaining: u64 = keep.iter().map(|a| a.bytes).sum();
        // `keep` is oldest-first, so draining from the front evicts the
        // oldest survivors until the budget is met.
        let mut idx = 0;
        while remaining > max_bytes && idx < keep.len() {
            let archive = keep[idx];
            outcome.removed.push(archive.session_id.clone());
            outcome.bytes_freed += archive.bytes;
            remaining -= archive.bytes;
            idx += 1;
        }
        keep.drain(..idx);
        outcome.bytes_remaining = remaining;
    } else {
        outcome.bytes_remaining = keep.iter().map(|a| a.bytes).sum();
    }

    if !dry_run {
        for session_id in &outcome.removed {
            let _ = remove_archive(project_id, session_id);
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Point the global data dir at a temp dir for one test.
    ///
    /// The `#[ctor]` test arm already redirects `ENGRAMDB_DATA_DIR` per
    /// process; this narrows it further per test. Safe under nextest's
    /// process-per-test model.
    fn with_data_dir<T>(f: impl FnOnce(&Path) -> T) -> T {
        let tmp = TempDir::new().unwrap();
        let prev = std::env::var("ENGRAMDB_DATA_DIR").ok();
        std::env::set_var("ENGRAMDB_DATA_DIR", tmp.path());
        let out = f(tmp.path());
        match prev {
            Some(v) => std::env::set_var("ENGRAMDB_DATA_DIR", v),
            None => std::env::remove_var("ENGRAMDB_DATA_DIR"),
        }
        out
    }

    fn write_transcript(dir: &Path, name: &str, lines: usize) -> PathBuf {
        let path = dir.join(name);
        let body: String = (0..lines)
            .map(|i| {
                format!(
                    "{{\"type\":\"user\",\"n\":{i},\"pad\":\"{}\"}}\n",
                    "x".repeat(200)
                )
            })
            .collect();
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn archive_roundtrips_and_preserves_bytes() {
        with_data_dir(|_| {
            let src = TempDir::new().unwrap();
            let transcript = write_transcript(src.path(), "s1.jsonl", 500);
            let original = std::fs::read(&transcript).unwrap();

            let archived = archive_transcript("proj", "s1", &transcript).unwrap();
            assert_eq!(archived.original_bytes, original.len() as u64);
            assert!(archived.bytes > 0);
            assert!(
                archived.bytes < archived.original_bytes,
                "archive should be smaller than the source"
            );

            let out = src.path().join("restored.jsonl");
            let sha = export_archive("proj", "s1", &out).unwrap();
            assert_eq!(sha, archived.sha256, "export must round-trip byte-for-byte");
            assert_eq!(std::fs::read(&out).unwrap(), original);
        });
    }

    #[test]
    fn re_archiving_overwrites_rather_than_duplicating() {
        with_data_dir(|_| {
            let src = TempDir::new().unwrap();
            let transcript = write_transcript(src.path(), "s1.jsonl", 10);
            archive_transcript("proj", "s1", &transcript).unwrap();
            archive_transcript("proj", "s1", &transcript).unwrap();
            assert_eq!(list_archives("proj").unwrap().len(), 1);
        });
    }

    #[test]
    fn no_tmp_file_survives_a_successful_archive() {
        with_data_dir(|_| {
            let src = TempDir::new().unwrap();
            let transcript = write_transcript(src.path(), "s1.jsonl", 10);
            archive_transcript("proj", "s1", &transcript).unwrap();
            let dir = paths::transcript_archive_dir("proj").unwrap();
            let leftovers: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("tmp"))
                .collect();
            assert!(leftovers.is_empty(), "temp file leaked: {leftovers:?}");
        });
    }

    #[test]
    fn prune_by_size_evicts_oldest_first() {
        with_data_dir(|_| {
            let src = TempDir::new().unwrap();
            for (i, name) in ["a", "b", "c"].iter().enumerate() {
                let t = write_transcript(src.path(), &format!("{name}.jsonl"), 200);
                archive_transcript("proj", name, &t).unwrap();
                // Space the mtimes so ordering is deterministic.
                let path = archive_path("proj", name).unwrap();
                let when = std::time::SystemTime::now()
                    - std::time::Duration::from_secs(300 - i as u64 * 60);
                let f = std::fs::File::options().write(true).open(&path).unwrap();
                f.set_modified(when).unwrap();
            }
            let total = total_bytes("proj").unwrap();

            // Budget that fits roughly one archive: the two oldest must go.
            let outcome = prune_archives("proj", None, total / 3, false).unwrap();
            assert!(!outcome.removed.is_empty());
            assert_eq!(outcome.removed[0], "a", "oldest must be evicted first");
            assert!(outcome.bytes_remaining <= total / 3);
            assert!(list_archives("proj")
                .unwrap()
                .iter()
                .any(|x| x.session_id == "c"));
        });
    }

    #[test]
    fn prune_by_age_drops_stale_archives() {
        with_data_dir(|_| {
            let src = TempDir::new().unwrap();
            let t = write_transcript(src.path(), "old.jsonl", 20);
            archive_transcript("proj", "old", &t).unwrap();
            let path = archive_path("proj", "old").unwrap();
            let f = std::fs::File::options().write(true).open(&path).unwrap();
            f.set_modified(
                std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 40),
            )
            .unwrap();

            let t2 = write_transcript(src.path(), "new.jsonl", 20);
            archive_transcript("proj", "new", &t2).unwrap();

            let outcome = prune_archives("proj", Some(30), 0, false).unwrap();
            assert_eq!(outcome.removed, vec!["old"]);
            let left = list_archives("proj").unwrap();
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].session_id, "new");
        });
    }

    #[test]
    fn dry_run_reports_without_deleting() {
        with_data_dir(|_| {
            let src = TempDir::new().unwrap();
            let t = write_transcript(src.path(), "s.jsonl", 50);
            archive_transcript("proj", "s", &t).unwrap();

            let outcome = prune_archives("proj", None, 1, true).unwrap();
            assert_eq!(outcome.removed, vec!["s"]);
            assert!(outcome.bytes_freed > 0);
            assert_eq!(list_archives("proj").unwrap().len(), 1, "dry run deleted");
        });
    }

    #[test]
    fn missing_archive_dir_lists_empty() {
        with_data_dir(|_| {
            assert!(list_archives("never-used").unwrap().is_empty());
            assert_eq!(total_bytes("never-used").unwrap(), 0);
            assert!(!remove_archive("never-used", "nope").unwrap());
        });
    }
}
