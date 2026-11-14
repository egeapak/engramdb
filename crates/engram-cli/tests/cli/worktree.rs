//! A linked git worktree of a repository that commits `.engramdb/`.
//!
//! The directory in the worktree is a checkout, not a stray store: no command
//! and no hook may write under it or delete it. Real git, real binary.

use crate::helpers::{add_memory, cmd, init_store};
use std::path::Path;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git must be on PATH for the worktree tests");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn list_and_session_start_hook_leave_a_tracked_worktree_clean() {
    let tmp = TempDir::new().unwrap();
    let main = tmp.path().join("main");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q"]);
    init_store(&main);
    let id = add_memory(&main, "decision", "Committed memory", "tracked by git");
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "store"]);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            wt.to_str().unwrap(),
        ],
    );
    assert!(wt.join(".engramdb/manifest.toml").exists());

    // `list` from the worktree reads the main worktree's store.
    let out = cmd()
        .args(["--dir", wt.to_str().unwrap(), "--format", "json", "list"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains(&id));
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");

    // A hook, as Claude Code runs it — and as it runs from inside
    // `git commit`, where git exports the variables that select another
    // repository and index. The tracked check must not inherit them.
    let foreign = tmp.path().join("foreign");
    std::fs::create_dir_all(&foreign).unwrap();
    git(&foreign, &["init", "-q"]);
    for with_git_env in [false, true] {
        let mut hook = cmd();
        hook.args(["--dir", wt.to_str().unwrap(), "hook", "session-start"])
            .env("ENGRAMDB_IN_PROCESS", "1")
            .write_stdin(format!(
                r#"{{"session_id":"s1","cwd":"{}","hook_event_name":"SessionStart","source":"startup"}}"#,
                wt.display()
            ));
        if with_git_env {
            hook.env("GIT_DIR", foreign.join(".git"))
                .env("GIT_INDEX_FILE", foreign.join(".git/index"));
        }
        hook.assert().success();
        assert_eq!(
            git(&wt, &["status", "--porcelain"]),
            "",
            "hook (git env: {with_git_env}) must leave the checkout clean"
        );
    }
    assert!(wt.join(".engramdb/manifest.toml").exists());
    assert_eq!(
        git(
            &main,
            &["status", "--porcelain", "--", ".engramdb/memories"]
        ),
        ""
    );
}
