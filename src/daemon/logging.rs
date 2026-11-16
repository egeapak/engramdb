//! Diagnostic logging for the shared daemon.
//!
//! The daemon is spawned **detached**: it outlives the process that started it
//! and nobody is attached to its streams. Until this module existed both were
//! `Stdio::null()`, so every reason a daemon could fail to start — a bind
//! error, a missing ONNX runtime, a panic on first model load — was written to
//! a stream that discarded it. The failure was not merely non-fatal (callers
//! fall back to in-process models by design); it was *invisible*, which is a
//! different and worse thing.
//!
//! So an auto-spawned daemon writes to `daemon.log` in the daemon folder
//! ([`super::daemon_log_path`] — beside the socket and the reclaim lock), and
//! `engramdb doctor` surfaces its tail. A daemon run by hand in a terminal keeps writing to that terminal
//! — see [`stderr_target`].
//!
//! # Size cap and its loss path
//!
//! The log is capped at [`MAX_LOG_BYTES`] when it is opened and, while a
//! daemon runs, every minute by [`cap_in_place`]. Every writer holds an
//! `O_APPEND` descriptor, so after a truncation the next write lands at offset
//! 0 (no sparse file). Declared loss: a cap discards the whole log, including
//! lines written in the last minute; between checks a hot loop can still
//! write up to ~60 s of warnings, which the accept loop's rate limit keeps to
//! a handful of lines.

use std::fs::{File, OpenOptions};
use std::io::Seek;
use std::path::Path;

/// Truncate the log once it exceeds this. A daemon that crash-loops writes the
/// same failure forever, and an unbounded diagnostic file is its own bug — the
/// point is the *most recent* failure, not every failure ever.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Where a daemon process should send its stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrTarget {
    /// Keep writing to the inherited stderr — someone is watching it.
    Inherit,
    /// Append to the daemon log file — nobody is watching.
    LogFile,
}

/// Decide where a *directly invoked* `engramdb daemon run` should write.
///
/// Split out as a pure function because the real call site can only be
/// exercised from an actual terminal, which no test has. A TTY means a human
/// is watching and redirecting to a file they would have to `tail` is worse
/// than useless; anything else (a pipe, a redirect, systemd, CI) means the
/// output would otherwise vanish.
///
/// This does **not** govern the auto-spawn path in `client::spawn_daemon`:
/// that child is detached and outlives its parent, so it always logs to the
/// file regardless of whether the *parent* happened to have a terminal.
pub fn stderr_target(stderr_is_terminal: bool) -> StderrTarget {
    if stderr_is_terminal {
        StderrTarget::Inherit
    } else {
        StderrTarget::LogFile
    }
}

/// Open the daemon log for appending, truncating it first if it has grown past
/// [`MAX_LOG_BYTES`].
///
/// Creates the parent directory on demand. The truncate-on-open policy keeps
/// this bounded without rotation machinery: a diagnostic log only needs to
/// answer "why did the daemon just fail", and losing older history to keep the
/// file finite is the right trade.
pub fn open_capped(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Always `O_APPEND`: a descriptor opened without it keeps writing at its
    // own offset after a later truncation and leaves a sparse file.
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if file.metadata()?.len() > MAX_LOG_BYTES {
        file.set_len(0)?;
    }
    file.seek(std::io::SeekFrom::End(0))?;
    Ok(file)
}

/// Truncate the log at `path` to empty if it has grown past [`MAX_LOG_BYTES`].
///
/// Returns whether it truncated. A missing file is `Ok(false)`. Safe while the
/// daemon is writing to it, because every writer appends.
pub fn cap_in_place(path: &Path) -> std::io::Result<bool> {
    let file = match OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if file.metadata()?.len() > MAX_LOG_BYTES {
        file.set_len(0)?;
        return Ok(true);
    }
    Ok(false)
}

/// Open the daemon log for the daemon serving `socket`, applying the size cap.
///
/// The daemon folder is created (or verified) first with the same 0700 /
/// owner check the bind applies, so the log is never written into a folder
/// the daemon would refuse to serve from. Separate from [`open_capped`] so the
/// spawn site has one fallible call to handle.
pub fn daemon_log_for_spawn(socket: &Path) -> anyhow::Result<File> {
    let dir = super::daemon_dir_for(socket);
    super::transport::prepare_daemon_dir(&dir)
        .map_err(|e| anyhow::anyhow!("preparing the daemon folder: {e}"))?;
    Ok(open_capped(&super::daemon_log_path(socket))?)
}

/// Read the last `max_lines` non-empty lines of the daemon log.
///
/// Returns an empty vector when the log is missing or unreadable — a daemon
/// that has never failed has nothing to say, and this is a diagnostic aid, not
/// a source of errors of its own.
pub fn tail(path: &Path, max_lines: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn stderr_target_follows_the_terminal() {
        assert_eq!(stderr_target(true), StderrTarget::Inherit);
        assert_eq!(stderr_target(false), StderrTarget::LogFile);
    }

    #[test]
    fn open_capped_appends_below_the_cap() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("logs").join("daemon.log");

        writeln!(open_capped(&path).unwrap(), "first").unwrap();
        writeln!(open_capped(&path).unwrap(), "second").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("first") && text.contains("second"),
            "a reopen below the cap must append, not clobber: {text:?}"
        );
    }

    #[test]
    fn open_capped_truncates_past_the_cap() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("logs").join("daemon.log");

        // Push it over the cap, then reopen.
        let mut file = open_capped(&path).unwrap();
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..20 {
            file.write_all(&chunk).unwrap();
        }
        file.flush().unwrap();
        drop(file);
        let before = std::fs::metadata(&path).unwrap().len();
        assert!(
            before > MAX_LOG_BYTES,
            "precondition: log must be oversized"
        );

        writeln!(open_capped(&path).unwrap(), "after truncate").unwrap();

        let after = std::fs::metadata(&path).unwrap().len();
        assert!(
            after <= MAX_LOG_BYTES,
            "reopening an oversized log must truncate it, got {after} bytes"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("after truncate") && !text.contains("xxxx"),
            "the truncated log must hold only what was written after: {text:?}"
        );
    }

    #[test]
    fn open_capped_creates_the_parent_directory() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("deep").join("nested").join("daemon.log");
        writeln!(open_capped(&path).unwrap(), "hello").unwrap();
        assert!(path.exists(), "open_capped must create missing parents");
    }

    #[test]
    fn tail_returns_the_last_lines_and_tolerates_a_missing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("daemon.log");
        assert!(
            tail(&path, 5).is_empty(),
            "a missing log is not an error, just nothing to report"
        );

        std::fs::write(&path, "one\ntwo\n\nthree\nfour\n").unwrap();
        assert_eq!(tail(&path, 2), vec!["three", "four"]);
        // Blank lines are dropped so the tail is all signal.
        assert_eq!(tail(&path, 10), vec!["one", "two", "three", "four"]);
    }

    /// The log lands in the socket's folder, and that folder is created
    /// private, exactly as the bind would create it.
    #[cfg(unix)]
    #[test]
    fn spawn_log_goes_in_the_private_daemon_folder() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("run").join("d.sock");
        writeln!(daemon_log_for_spawn(&socket).unwrap(), "hello").unwrap();
        let log = tmp.path().join("run").join("daemon.log");
        assert_eq!(crate::daemon::daemon_log_path(&socket), log);
        assert!(std::fs::read_to_string(&log).unwrap().contains("hello"));
        let mode = std::fs::metadata(tmp.path().join("run"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    /// A folder the bind would refuse gets no log either.
    #[cfg(unix)]
    #[test]
    fn spawn_log_refuses_a_loose_folder() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("loose");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(daemon_log_for_spawn(&dir.join("d.sock")).is_err());
        assert!(!dir.join("daemon.log").exists());
    }

    /// A cap while a writer holds the file must not leave the writer at its
    /// old offset: the next write lands at 0 because the descriptor appends.
    #[test]
    fn cap_in_place_truncates_and_appends_resume_at_zero() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("daemon.log");
        // Oversized before open: the variant that used to open without
        // `O_APPEND`.
        std::fs::write(&path, vec![b'y'; MAX_LOG_BYTES as usize + 10]).unwrap();
        let mut file = open_capped(&path).unwrap();
        file.write_all(&vec![b'x'; MAX_LOG_BYTES as usize + 1])
            .unwrap();
        assert!(cap_in_place(&path).unwrap());
        file.write_all(b"x\n").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 2);
        assert!(!cap_in_place(&path).unwrap(), "below the cap: untouched");
        assert!(!cap_in_place(&tmp.path().join("missing.log")).unwrap());
    }
}
