//! Temp-then-rename writer shared by the small JSON files under a project's
//! `.engramdb/state/` ([`crate::harvest_state`], [`crate::task_state`]).
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

use crate::error::{Result, StorageError};
use std::io::Write;
use std::path::Path;

/// Write `contents` to a `.json` file under a state dir, atomically.
///
/// Creates the parent directory, writes a sibling `<name>.json.tmp`, then
/// renames over `path`. A planted temp path is refused rather than cleaned
/// up: silently unlinking whatever sits there is how a *legitimate* file gets
/// destroyed, and advisory state is not worth that trade.
pub(crate) fn write_state_json(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
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
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(tmp)
}

/// No `O_NOFOLLOW` equivalent off unix, and no committed-symlink delivery
/// path either (git checks a symlink out as a plain file without developer
/// mode). Same fallback shape as `transcript_archive::restrict_to_owner`.
#[cfg(not(unix))]
fn open_temp(tmp: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(tmp)
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
