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
//! The temp is now a name nobody can plant ahead of time
//! (`.<name>.<pid>.<8 random hex>.tmp`) opened `O_CREAT|O_EXCL|O_NOFOLLOW`:
//! an existing path of any kind fails the open, and another name is tried.
//! A unique name is also what lets two writers of one state file (two
//! processes, or two hooks inside the daemon) run at once — with the fixed
//! name each truncated and renamed the other's half-written temp. The
//! `rename` needs no protection of its own — it never dereferences its
//! source. The temp is removed on every error path; a process killed between
//! the create and the rename leaves one behind, which nothing reads and
//! nothing reuses.
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
/// Creates the parent directory, writes a uniquely named sibling temp (see the
/// module docs), syncs it, renames it over `path` and syncs the directory.
pub(crate) fn write_state_json(path: &Path, contents: &str) -> Result<()> {
    write_via_temp(path, contents.as_bytes(), || true).map(|_| ())
}

/// Replace a state file of any extension, atomically. Same discipline as
/// [`write_state_json`].
pub(crate) fn write_state_file(path: &Path, contents: &str) -> Result<()> {
    write_via_temp(path, contents.as_bytes(), || true).map(|_| ())
}

/// [`write_state_file`] for exact bytes, with a last check before the rename.
///
/// `still_current` runs after the temp is complete and synced, immediately
/// before the `rename`; when it answers `false` nothing is replaced, the temp
/// is removed and the result is `Ok(false)`. Ledger compaction uses it to
/// give up when the file it read is no longer the file at `path`.
pub(crate) fn replace_state_file_if(
    path: &Path,
    contents: &[u8],
    still_current: impl FnOnce() -> bool,
) -> Result<bool> {
    write_via_temp(path, contents, still_current)
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
    append_state_bytes(path, contents.as_bytes())
}

/// [`append_state_file`] for exact bytes (a tail carried over verbatim may
/// hold a torn character).
pub(crate) fn append_state_bytes(path: &Path, contents: &[u8]) -> Result<()> {
    if contents.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Named rather than inlined: a bare ELOOP reads like a broken filesystem
    // instead of a planted file.
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
    file.write_all(contents)?;
    Ok(())
}

/// Read a state file, refusing to follow a symlink or open a FIFO.
///
/// A missing file is `Ok(None)` — the same "reads as empty" every caller here
/// already relies on. Anything that exists but is not a plain regular file is
/// an error naming the cause, never a silent empty read: a planted path is a
/// deliberate act and the operator has to be told, and never a deletion
/// either: silently unlinking whatever sits there is how a *legitimate* file
/// gets destroyed.
///
/// **Lossy on invalid UTF-8.** Every file read here is line-oriented and
/// append-only, and a crash can tear a multibyte character in half. A strict
/// `read_to_string` then failed the whole read — one bad byte made a ledger
/// fold as empty and a claimed stats file fail every later drain. Invalid
/// sequences become U+FFFD (warned, with the count): the damaged line either
/// still parses (the damage was inside a string) or is skipped and counted
/// by its reader's malformed-line handling, and every other line survives.
/// Byte offsets into the file must come from [`read_state_bytes`], never from
/// this string (U+FFFD is three bytes).
pub(crate) fn read_state_file(path: &Path) -> Result<Option<String>> {
    let Some(bytes) = read_state_bytes(path)? else {
        return Ok(None);
    };
    Ok(Some(lossy_text(bytes, path)))
}

/// The raw bytes behind [`read_state_file`], same refusals.
pub(crate) fn read_state_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    let Some(mut file) = open_state_file_for_read(path)? else {
        return Ok(None);
    };
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(Some(out))
}

/// Decode state-file bytes, replacing invalid UTF-8 and saying so.
pub(crate) fn lossy_text(bytes: Vec<u8>, path: &Path) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => {
            let bytes = e.into_bytes();
            let bad = bytes
                .utf8_chunks()
                .filter(|chunk| !chunk.invalid().is_empty())
                .count();
            tracing::warn!(
                "{} holds {bad} invalid UTF-8 sequence(s) (a torn write?); they are read as \
                 U+FFFD and the lines they are in may be skipped",
                path.display()
            );
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }
}

/// Open (creating it) a lock file under a state dir for `flock`.
///
/// Same protections as every other open here: a symlink planted at the lock
/// path is refused rather than followed (opening it for write would create
/// or touch whatever it names), and so is anything that is not a regular
/// file. The caller decides what a refusal means; for advisory state that is
/// "proceed unlocked, with a warning".
pub(crate) fn open_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    open_lock(path)
}

#[cfg(unix)]
fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure_regular(file)
}

#[cfg(not(unix))]
fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    ensure_regular(file)
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

/// How many fresh names to try before giving up. A collision needs the same
/// pid and the same 32 random bits, so the second attempt is already unheard
/// of; the bound only keeps a broken random source from looping.
const TEMP_NAME_ATTEMPTS: usize = 8;

/// `.<name>.<pid>.<8 random hex>.tmp` beside `path`.
fn temp_path_for(path: &Path) -> PathBuf {
    // The random tail of a v7 UUID: 32 of its 62 random bits.
    let random = uuid::Uuid::now_v7().as_u128() as u32;
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}.{random:08x}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// Create a fresh temp beside `path`. Never reuses and never follows.
fn create_temp(path: &Path) -> Result<(std::fs::File, PathBuf)> {
    let mut last = None;
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let tmp = temp_path_for(path);
        match open_temp(&tmp) {
            Ok(file) => return Ok((file, tmp)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e.into()),
        }
    }
    Err(last
        .unwrap_or_else(|| std::io::Error::other("no temp name available"))
        .into())
}

fn write_via_temp(
    path: &Path,
    contents: &[u8],
    still_current: impl FnOnce() -> bool,
) -> Result<bool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (mut file, tmp) = create_temp(path)?;
    let written = (|| -> std::io::Result<bool> {
        file.write_all(contents)?;
        // Before the rename: a rename that reaches the disk ahead of the data
        // it names leaves an empty or short state file after a power cut.
        file.sync_all()?;
        if !still_current() {
            return Ok(false);
        }
        std::fs::rename(&tmp, path)?;
        Ok(true)
    })();
    drop(file);
    match written {
        Ok(true) => {
            sync_parent_dir(path);
            Ok(true)
        }
        Ok(false) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(false)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// Make the rename itself durable. Best-effort: the new file is already in
/// place, and not every filesystem lets a directory be synced.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Mode at *creation*, matching [`crate::transcript_archive`]: the ledger
/// carries session ids and free-text review notes, and a `chmod` after the
/// fact would leave it `0644` for the whole write window. `O_EXCL` means the
/// path did not exist, so there is nothing to follow and nothing to reuse.
#[cfg(unix)]
fn open_temp(tmp: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(tmp)
}

#[cfg(not(unix))]
fn open_temp(tmp: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
}

/// The live path: never follow a symlink, never block on a FIFO.
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

    /// The temp name used to be fixed (`<name>.json.tmp`), so a symlink
    /// committed there redirected the write. It is now unpredictable and
    /// opened `O_EXCL`: the planted path is simply never opened.
    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_old_fixed_temp_path_is_never_written_through() {
        let tmp = TempDir::new().unwrap();
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "do not touch").unwrap();

        let target = tmp.path().join("state").join("thing.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let planted = target.with_extension("json.tmp");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        write_state_json(&target, "{}").unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "the write landed on the symlink's target"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}");
        assert!(
            std::fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_file(),
            "the symlink was renamed over the target"
        );
        assert!(
            std::fs::symlink_metadata(&planted)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the planted path was touched"
        );
    }

    fn temps_in(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    /// With one fixed temp name, two writers of the same file truncated and
    /// renamed each other's half-written temp.
    #[test]
    fn temp_names_do_not_collide() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("state").join("thing.jsonl");
        let names: std::collections::HashSet<PathBuf> =
            (0..256).map(|_| temp_path_for(&target)).collect();
        assert_eq!(names.len(), 256);
        let name = names
            .iter()
            .next()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with(&format!(".thing.jsonl.{}.", std::process::id()))
                && name.ends_with(".tmp"),
            "{name}"
        );

        // 16 threads replacing one file: every write succeeds, the result is
        // one of the bodies in full, and no temp is left.
        std::thread::scope(|scope| {
            for i in 0..16 {
                let target = &target;
                scope.spawn(move || {
                    let body = format!("{i}\n").repeat(4096);
                    for _ in 0..8 {
                        write_state_file(target, &body).unwrap();
                    }
                });
            }
        });
        let text = std::fs::read_to_string(&target).unwrap();
        let first = text.lines().next().unwrap();
        assert_eq!(text.lines().count(), 4096);
        assert!(text.lines().all(|l| l == first), "two writers interleaved");
        assert!(temps_in(target.parent().unwrap()).is_empty());
    }

    #[test]
    fn a_failed_write_leaves_no_temp() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        // The rename fails: a non-empty directory sits at the target.
        let target = state.join("thing.json");
        std::fs::create_dir_all(target.join("child")).unwrap();
        assert!(write_state_json(&target, "{}").is_err());
        assert!(temps_in(&state).is_empty(), "{:?}", temps_in(&state));

        // A refused replacement removes its temp too, and replaces nothing.
        let other = state.join("other.jsonl");
        std::fs::write(&other, "old").unwrap();
        assert!(!replace_state_file_if(&other, b"new", || false).unwrap());
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "old");
        assert!(temps_in(&state).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("thing.json");
        write_state_json(&target, "{}").unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
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
    fn a_stale_temp_file_does_not_wedge_later_writes() {
        // A crash mid-write leaves a temp behind (under the old fixed name,
        // or a unique one). Neither may stop the next write.
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("thing.json");
        std::fs::write(target.with_extension("json.tmp"), "leftover garbage").unwrap();
        std::fs::write(tmp.path().join(".thing.json.1.deadbeef.tmp"), "leftover").unwrap();

        write_state_json(&target, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"a\":1}");
    }

    /// A crash can leave half of a multibyte character at the end of an
    /// append. The strict read failed the whole file on it.
    #[test]
    fn a_torn_multibyte_character_does_not_fail_the_read() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state").join("log.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = b"{\"a\":1}\n{\"b\":\"caf".to_vec();
        bytes.push(0xC3); // first half of U+00E9, never completed
        bytes.extend_from_slice(b"\n{\"c\":3}\n");
        std::fs::write(&path, &bytes).unwrap();

        let text = read_state_file(&path).unwrap().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "every line survives: {text:?}");
        assert_eq!(lines[0], "{\"a\":1}");
        assert!(lines[1].contains('\u{FFFD}'), "{:?}", lines[1]);
        assert_eq!(lines[2], "{\"c\":3}");
        // The byte reader is exact.
        assert_eq!(read_state_bytes(&path).unwrap().unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_file_open_refuses_a_symlink() {
        let tmp = TempDir::new().unwrap();
        let victim = tmp.path().join("victim");
        let lock = tmp.path().join("state").join("x.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, &lock).unwrap();
        assert!(open_lock_file(&lock).is_err());
        assert!(!victim.exists(), "the open created the symlink's target");
    }
}
