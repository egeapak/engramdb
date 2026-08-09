//! Readers and writers shared by the small state files under a project's
//! `.engramdb/state/` ([`crate::harvest_state`], [`crate::task_state`]).
//!
//! Nothing under `state/` may be touched with bare `std::fs`: every open in
//! this module carries protections a plain `read_to_string` or `write` does
//! not, and a caller that goes around it re-opens whichever hole this file
//! exists to close.
//!
//! Both wrote through a *predictable* temp path (`<name>.json.tmp`) with a
//! plain `std::fs::write`, and that is reachable by an attacker who never
//! touches the machine: `.engramdb/` is designed to be committed, `init`
//! never overwrites an existing `.engramdb/.gitignore`, and a symlink
//! committed at the temp path is checked out verbatim by `git clone`
//! regardless of what is ignored. The unattended SessionEnd hook then writes
//! through it and `rename(2)` moves the symlink rather than its target,
//! leaving the victim file overwritten and no trace in the state dir.
//!
//! `O_NOFOLLOW` is the fix: the open fails outright rather than following.
//! The `rename` needs no equivalent — it never dereferences its source, so
//! once the temp file is known to be a real file the move is safe.
//!
//! [`append_state_file`] needs the same flag for a stronger reason: it opens
//! the *live* path rather than a temp, so a symlink planted there is followed
//! on every append, not just on the first write after a crash.
//!
//! **Reads need it too, and for a worse outcome.** A redirected write
//! overwrites a file the user can see; a redirected *read* copies whatever the
//! symlink points at into a store the harvest flow then hands to a model —
//! [`crate::harvest_state::adopt_ledger`] appends the bytes it reads verbatim
//! into the root project's ledger. Same delivery, same unattended hook, so
//! [`read_state_file`] carries the same `O_NOFOLLOW`.
//!
//! `O_NOFOLLOW` alone is not enough on the read side: it refuses a *symlink*
//! at the path, not a FIFO planted directly there, and a `read` on a FIFO with
//! no writer blocks forever — which is a hang in the SessionEnd hook, not an
//! error it can report. So every open here is `O_NONBLOCK` (a no-op on a
//! regular file, the difference between "returns" and "blocks" on a FIFO) and
//! every opened handle is checked to be a regular file before it is used.

use crate::error::{Result, StorageError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Write `contents` to a `.json` file under a state dir, atomically.
///
/// Creates the parent directory, writes a sibling `<name>.json.tmp`, then
/// renames over `path`. A planted temp path is refused rather than cleaned
/// up: silently unlinking whatever sits there is how a *legitimate* file gets
/// destroyed, and advisory state is not worth that trade.
pub(crate) fn write_state_json(path: &Path, contents: &str) -> Result<()> {
    write_via_temp(path, path.with_extension("json.tmp"), contents)
}

/// Replace a state file of any extension, atomically.
///
/// Same discipline as [`write_state_json`], but the temp name is the file name
/// with `.tmp` *appended* rather than the extension replaced: on
/// `harvest_ledger.jsonl` the latter yields `harvest_ledger.tmp`, colliding
/// with any sibling of another extension.
pub(crate) fn write_state_file(path: &Path, contents: &str) -> Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp: PathBuf = path.with_file_name(name);
    write_via_temp(path, tmp, contents)
}

/// Append to a state file, creating it if needed.
///
/// The write is a single `write_all` on a handle opened `O_APPEND`, which is
/// what makes concurrent writers safe without a lock: the kernel places each
/// record at the current end of file, so two processes appending at the same
/// moment interleave records rather than overwrite each other. Callers must
/// therefore hand over whole, newline-terminated records — a caller that
/// appends half a line at a time gets the interleaving it asked for.
///
/// An unterminated final byte gets a newline first. A crash mid-append leaves
/// one, and splicing the next record onto that fragment would make a torn line
/// cost *two* records instead of one — turning the format's whole crash story
/// ("a partial write costs the partial line") into a lie the very next time
/// anything is written.
pub(crate) fn append_state_file(path: &Path, contents: &str) -> Result<()> {
    if contents.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Named rather than inlined, for the same reason as `open_temp`: a bare
    // ELOOP reads like a broken filesystem instead of a planted file.
    let mut file = open_append(path).map_err(|e| {
        StorageError::Validation(format!(
            "could not append to {} ({e}); if that path is a symlink, remove it — \
             state writes deliberately refuse to follow one",
            path.display()
        ))
    })?;
    if ends_mid_line(&mut file).unwrap_or(false) {
        file.write_all(b"\n")?;
    }
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// Read a state file, refusing to follow a symlink or open a FIFO.
///
/// A missing file is `Ok(None)` — the same "reads as empty" every caller here
/// already relies on. Anything that exists but is not a plain regular file is
/// an error naming the cause, never a silent empty read: a planted path is a
/// deliberate act and the operator has to be told, and never a deletion
/// either, for the same reason [`write_state_json`] refuses a planted temp
/// rather than clearing it.
pub(crate) fn read_state_file(path: &Path) -> Result<Option<String>> {
    let Some(mut file) = open_state_file_for_read(path)? else {
        return Ok(None);
    };
    let mut out = String::new();
    file.read_to_string(&mut out)?;
    Ok(Some(out))
}

/// The handle behind [`read_state_file`], for the one caller that reads a
/// state file incrementally (compaction re-reads the tail through a handle it
/// held across the rewrite).
pub(crate) fn open_state_file_for_read(path: &Path) -> Result<Option<std::fs::File>> {
    match open_read(path) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StorageError::Validation(format!(
            "could not read {} ({e}); if that path is a symlink, remove it — \
             state reads deliberately refuse to follow one",
            path.display()
        ))),
    }
}

/// Open for reading without following a symlink, and without blocking on a
/// FIFO.
///
/// The type check is on the *handle*, not on the path: a `symlink_metadata`
/// probe followed by an open is a TOCTOU race, while `fstat` on an already-open
/// descriptor describes the exact object the read will draw from.
#[cfg(unix)]
fn open_read(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure_regular(file)
}

#[cfg(not(unix))]
fn open_read(path: &Path) -> std::io::Result<std::fs::File> {
    ensure_regular(std::fs::File::open(path)?)
}

/// Reject a handle that is not a plain regular file.
///
/// A directory, a device or a FIFO at a state path is never something this
/// program wrote, and a FIFO in particular turns an ordinary read or append
/// into an unbounded wait inside an unattended hook.
fn ensure_regular(file: std::fs::File) -> std::io::Result<std::fs::File> {
    if file.metadata()?.file_type().is_file() {
        return Ok(file);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "not a regular file",
    ))
}

/// Is there a trailing fragment with no newline after it?
///
/// Reads through the same handle the append goes to; `O_APPEND` fixes the
/// *write* offset at the end regardless of where the read cursor is left, so
/// the seek here cannot misplace the record.
fn ends_mid_line(file: &mut std::fs::File) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

fn write_via_temp(path: &Path, tmp: PathBuf, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    {
        // Named rather than inlined: `?` on the open would surface an attack
        // as a bare ELOOP ("Too many levels of symbolic links"), which reads
        // like a broken filesystem instead of a planted file.
        let mut file = open_temp(&tmp).map_err(|e| {
            StorageError::Validation(format!(
                "could not create {} ({e}); if that path exists and is a symlink, \
                 remove it — state writes deliberately refuse to follow one",
                tmp.display()
            ))
        })?;
        file.write_all(contents.as_bytes())?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Mode at *creation*, matching [`crate::transcript_archive`]: the ledger
/// carries session ids and free-text review notes, and a `chmod` after the
/// fact would leave it `0644` for the whole write window.
#[cfg(unix)]
fn open_temp(tmp: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(tmp)?;
    ensure_regular(file)
}

/// No `O_NOFOLLOW` equivalent off unix, and no committed-symlink delivery
/// path either (git checks a symlink out as a plain file without developer
/// mode). Same fallback shape as `transcript_archive::restrict_to_owner`.
#[cfg(not(unix))]
fn open_temp(tmp: &Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(tmp)?;
    ensure_regular(file)
}

/// Same protections as [`open_temp`], on the live path.
#[cfg(unix)]
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure_regular(file)
}

#[cfg(not(unix))]
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    ensure_regular(file)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_symlinked_temp_path_cannot_redirect_the_write() {
        let tmp = TempDir::new().unwrap();
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "do not touch").unwrap();

        let target = tmp.path().join("state").join("thing.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, target.with_extension("json.tmp")).unwrap();

        let result = write_state_json(&target, "{}");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "the write landed on the symlink's target"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "the refusal should name the cause: {err}"
        );
        assert!(!target.exists(), "the symlink was renamed over the target");
    }

    #[test]
    fn appends_land_at_the_end_and_create_the_file() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("state").join("log.jsonl");
        append_state_file(&target, "one\n").unwrap();
        append_state_file(&target, "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "one\ntwo\n");
    }

    /// A crash leaves a fragment with no newline after it. Splicing the next
    /// record onto it would make one torn write cost two records.
    #[test]
    fn an_append_after_a_torn_write_starts_its_own_line() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("log.jsonl");
        std::fs::write(&target, "whole\ntor").unwrap();

        append_state_file(&target, "next\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "whole\ntor\nnext\n"
        );
    }

    /// An append opens the *live* path, so unlike the temp-then-rename writers
    /// a symlink planted there is followed on every single write.
    #[test]
    fn a_symlinked_target_cannot_redirect_an_append() {
        let tmp = TempDir::new().unwrap();
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "do not touch").unwrap();

        let target = tmp.path().join("state").join("log.jsonl");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, &target).unwrap();

        let err = append_state_file(&target, "line\n").unwrap_err();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "the append landed on the symlink's target"
        );
        assert!(
            err.to_string().contains("symlink"),
            "the refusal should name the cause: {err}"
        );
    }

    /// The read counterpart of the two write tests above. A followed read is
    /// the worse half of the same planted symlink: it hands the target's bytes
    /// to whatever consumes the state file.
    #[test]
    fn a_symlinked_target_cannot_redirect_a_read() {
        let tmp = TempDir::new().unwrap();
        let secret = tmp.path().join("secret.txt");
        std::fs::write(&secret, "top secret").unwrap();

        let target = tmp.path().join("state").join("log.jsonl");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&secret, &target).unwrap();

        let err = read_state_file(&target).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "the refusal should name the cause: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&secret).unwrap(),
            "top secret",
            "a refused read must not delete what it refused"
        );
    }

    /// `O_NOFOLLOW` says nothing about a FIFO planted directly at the path,
    /// and `read` on one with no writer blocks forever. The `open` is where
    /// that has to be caught, because after it there is nothing to time out.
    #[test]
    fn a_fifo_is_refused_rather_than_read() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("log.jsonl");
        let c_path = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        assert!(read_state_file(&target).is_err(), "a FIFO was read");
        assert!(
            append_state_file(&target, "line\n").is_err(),
            "a FIFO was appended to"
        );
    }

    #[test]
    fn a_missing_file_reads_as_nothing_rather_than_an_error() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            read_state_file(&tmp.path().join("nope.jsonl")).unwrap(),
            None
        );
    }

    #[test]
    fn a_stale_temp_file_is_reused_not_refused() {
        // A crash mid-write leaves the temp behind. Refusing it (`O_EXCL`)
        // would wedge every later write to that state file permanently.
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("thing.json");
        std::fs::write(target.with_extension("json.tmp"), "leftover garbage").unwrap();

        write_state_json(&target, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"a\":1}");
    }
}
