//! Integration tests for `engramdb harvest`.
//!
//! These build a synthetic Claude Code transcript corpus on disk and drive the
//! real binary against it. Each session is planted with a *known* outcome so
//! the assertions are about behavior a user would notice, not implementation
//! detail:
//!
//! - `rich` — holds several durable facts; must survive digestion intact.
//! - `empty` — routine lookup; listed, but an agent should find nothing.
//! - `worktree` — lives under a *different* cwd; must still be found, because
//!   its memories route to the same store.
//! - `noise` — only machine-generated turns; must never be offered.
//! - `collision` — sits in the target project's encoded directory but records a
//!   foreign cwd; must never be offered (the encoding is lossy).
//! - `sibling` — a project whose path shares a textual prefix with the target;
//!   must never be offered.

use super::helpers::cmd;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Mirror of Claude Code's transcript-directory naming.
fn encode(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Write a transcript for `cwd` into the projects root, named after `owner`
/// (which is normally `cwd`, but differs for the collision fixture).
fn write_transcript(root: &Path, owner: &Path, session: &str, lines: &[String]) -> PathBuf {
    let dir = root.join("projects").join(encode(owner));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    path
}

fn user_line(cwd: &Path, ts: &str, text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "cwd": cwd.to_string_lossy(),
        "gitBranch": "main",
        "timestamp": ts,
        "message": { "role": "user", "content": text }
    })
    .to_string()
}

fn assistant_text_line(cwd: &Path, ts: &str, text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "cwd": cwd.to_string_lossy(),
        "timestamp": ts,
        "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] }
    })
    .to_string()
}

fn tool_use_line(cwd: &Path, ts: &str, id: &str, name: &str, command: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "cwd": cwd.to_string_lossy(),
        "timestamp": ts,
        "message": { "role": "assistant", "content": [
            { "type": "tool_use", "id": id, "name": name, "input": { "command": command } }
        ]}
    })
    .to_string()
}

fn tool_result_line(cwd: &Path, ts: &str, id: &str, content: &str, is_error: bool) -> String {
    serde_json::json!({
        "type": "user",
        "cwd": cwd.to_string_lossy(),
        "timestamp": ts,
        "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": id, "is_error": is_error, "content": content }
        ]}
    })
    .to_string()
}

/// Build the on-disk shape git uses for a linked worktree: the worktree's
/// `.git` is a *file* pointing into `<main>/.git/worktrees/<name>/`.
fn make_linked_worktree(main: &Path, worktree: &Path, name: &str) {
    let gitdir = main.join(".git").join("worktrees").join(name);
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::write(gitdir.join("commondir"), "../..").unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .unwrap();
}

/// A built corpus: the temp dirs must stay alive for the duration of a test.
struct Corpus {
    _tmp: TempDir,
    claude: PathBuf,
    main: PathBuf,
    worktree: PathBuf,
}

impl Corpus {
    /// Run `engramdb` against this corpus, with the transcript root injected.
    fn engramdb(&self, dir: &Path, args: &[&str]) -> assert_cmd::Command {
        let mut c = cmd();
        c.env("CLAUDE_CONFIG_DIR", &self.claude);
        c.arg("--dir").arg(dir).arg("--format").arg("plain");
        c.args(args);
        c
    }

    fn stdout(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.engramdb(dir, args).output().unwrap();
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

fn build_corpus() -> Corpus {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();
    let claude = base.join("claude");
    let main = base.join("proj");
    // Shares a textual prefix with `main` — a naive string prefix test would
    // wrongly attribute this project's sessions to `main`.
    let sibling = base.join("proj-other");
    let worktree = base.join("wt");
    for d in [&main, &sibling, &worktree] {
        std::fs::create_dir_all(d).unwrap();
    }
    make_linked_worktree(&main, &worktree, "wt");

    // --- rich: three planted facts, including a failure→fix pair -----------
    write_transcript(
        &claude,
        &main,
        "aaaa1111-rich",
        &[
            user_line(&main, "2026-07-20T09:00:00Z", "CI is red, tests won't run"),
            tool_use_line(
                &main,
                "2026-07-20T09:00:10Z",
                "t1",
                "Bash",
                "cargo test --workspace",
            ),
            tool_result_line(
                &main,
                "2026-07-20T09:01:00Z",
                "t1",
                "error: Could not find `protoc`. Install protobuf-compiler.",
                true,
            ),
            assistant_text_line(
                &main,
                "2026-07-20T09:05:00Z",
                "Use cargo nextest run --workspace; plain cargo test breaks test isolation.",
            ),
            tool_use_line(
                &main,
                "2026-07-20T09:05:10Z",
                "t2",
                "Bash",
                "cargo nextest run --workspace",
            ),
            tool_result_line(&main, "2026-07-20T09:06:00Z", "t2", "1927 passed", false),
            user_line(
                &main,
                "2026-07-20T09:10:00Z",
                "Going forward always run cargo fmt --all before clippy.",
            ),
        ],
    );

    // --- empty: routine, nothing durable ----------------------------------
    write_transcript(
        &claude,
        &main,
        "bbbb2222-empty",
        &[
            user_line(
                &main,
                "2026-07-21T14:00:00Z",
                "what does src/lib.rs export?",
            ),
            assistant_text_line(&main, "2026-07-21T14:00:20Z", "daemon, ops, retrieval."),
        ],
    );

    // --- noise: only machine-generated turns -------------------------------
    write_transcript(
        &claude,
        &main,
        "dddd4444-noise",
        &[
            user_line(
                &main,
                "2026-07-23T08:00:00Z",
                "<command-name>/reflect</command-name>",
            ),
            user_line(
                &main,
                "2026-07-23T08:00:01Z",
                "<system-reminder>be nice</system-reminder>",
            ),
            user_line(
                &main,
                "2026-07-23T08:00:02Z",
                "<local-command-stdout>ok</local-command-stdout>",
            ),
        ],
    );

    // --- collision: filed under `main`'s encoded name, foreign cwd ---------
    write_transcript(
        &claude,
        &main,
        "eeee5555-collision",
        &[user_line(
            Path::new("/some/entirely/other/repo"),
            "2026-07-24T10:00:00Z",
            "CONFIDENTIAL other project content",
        )],
    );

    // --- sibling: shares a path prefix with `main` -------------------------
    write_transcript(
        &claude,
        &sibling,
        "ffff6666-sibling",
        &[user_line(
            &sibling,
            "2026-07-25T10:00:00Z",
            "SIBLING project content",
        )],
    );

    // --- worktree: different cwd, same logical project ---------------------
    write_transcript(
        &claude,
        &worktree,
        "cccc3333-worktree",
        &[user_line(
            &worktree,
            "2026-07-22T11:00:00Z",
            "embeddings differ run to run on this box",
        )],
    );

    Corpus {
        _tmp: tmp,
        claude,
        main,
        worktree,
    }
}

/// Initialize the main store, then register the worktree by touching it.
///
/// The worktree is a *real* linked-worktree layout (a `.git` file pointing at
/// `<main>/.git/worktrees/<name>/`), mirroring `worktree.rs`'s own fixtures.
/// That matters: `resolve_project_root` routes on git worktree detection, not
/// on registry parent links, so a merely-`projects link`ed directory would
/// keep its own store and its own ledger and would not exercise the sharing
/// this test is about.
fn init_with_worktree(c: &Corpus) {
    c.engramdb(&c.main, &["init", "--no-embeddings"])
        .assert()
        .success();
    // Any non-exempt command inside the worktree consolidates and registers it.
    c.engramdb(&c.worktree, &["list"]).assert().success();
}

#[test]
fn lists_own_and_worktree_sessions_only() {
    let c = build_corpus();
    init_with_worktree(&c);

    let out = c.stdout(&c.main, &["harvest", "list"]);

    // In scope: this project's sessions and its worktree's.
    assert!(out.contains("aaaa1111-rich"), "missing rich session: {out}");
    assert!(
        out.contains("bbbb2222-empt"),
        "missing empty session: {out}"
    );
    assert!(
        out.contains("cccc3333-work"),
        "worktree session must be harvested with the main project: {out}"
    );

    // Out of scope, and each for a different reason.
    assert!(
        !out.contains("eeee5555"),
        "a transcript whose recorded cwd is a foreign project must never be \
         attributed here — the directory encoding is lossy: {out}"
    );
    assert!(
        !out.contains("CONFIDENTIAL"),
        "foreign project content leaked: {out}"
    );
    assert!(
        !out.contains("ffff6666"),
        "a sibling project sharing a path prefix must not match: {out}"
    );
    assert!(
        !out.contains("dddd4444"),
        "a session with only machine-generated turns has nothing to harvest: {out}"
    );
}

#[test]
fn worktree_and_main_see_the_same_sessions() {
    let c = build_corpus();
    init_with_worktree(&c);

    let from_main = c.stdout(&c.main, &["harvest", "list"]);
    let from_worktree = c.stdout(&c.worktree, &["harvest", "list"]);

    for id in ["aaaa1111-rich", "bbbb2222-empt", "cccc3333-work"] {
        assert!(from_main.contains(id), "main missing {id}: {from_main}");
        assert!(
            from_worktree.contains(id),
            "worktree missing {id}: {from_worktree}"
        );
    }
}

#[test]
fn digest_preserves_planted_facts_and_failure_outcomes() {
    let c = build_corpus();
    init_with_worktree(&c);

    let out = c.stdout(&c.main, &["harvest", "show", "aaaa"]);

    // Prose is verbatim — these are the durable facts.
    assert!(out.contains("cargo nextest run --workspace"), "{out}");
    assert!(out.contains("cargo fmt --all before clippy"), "{out}");
    // The failure and its fix are both legible, which is the point of keeping
    // result previews at all.
    assert!(out.contains("[FAILED]"), "failed tool not marked: {out}");
    assert!(out.contains("Could not find `protoc`"), "{out}");
    assert!(out.contains("[ok]"), "successful tool not marked: {out}");
    // A complete digest must not claim to be partial.
    assert!(!out.contains("partial digest"), "{out}");
}

#[test]
fn tight_budget_marks_the_digest_partial() {
    let c = build_corpus();
    init_with_worktree(&c);

    let out = c.stdout(&c.main, &["harvest", "show", "aaaa", "--max-chars", "300"]);
    assert!(
        out.contains("partial digest"),
        "a truncated digest must say so, or an agent will read a prefix as the \
         whole session: {out}"
    );
    // The opening prompt frames everything else and is never dropped.
    assert!(out.contains("CI is red"), "{out}");
}

#[test]
fn ledger_hides_reviewed_sessions_and_reset_restores_them() {
    let c = build_corpus();
    init_with_worktree(&c);

    // A session that yielded nothing is still recorded — that is the case the
    // ledger exists for, since it leaves no other trace.
    c.engramdb(&c.main, &["harvest", "mark", "bbbb"])
        .assert()
        .success();
    let out = c.stdout(&c.main, &["harvest", "list"]);
    assert!(
        !out.contains("bbbb2222"),
        "reviewed session still offered: {out}"
    );
    assert!(out.contains("aaaa1111"), "{out}");

    let out = c.stdout(&c.main, &["harvest", "list", "--include-harvested"]);
    assert!(out.contains("bbbb2222"), "{out}");
    assert!(out.contains("(harvested)"), "{out}");

    c.engramdb(&c.main, &["harvest", "reset", "bbbb"])
        .assert()
        .success();
    let out = c.stdout(&c.main, &["harvest", "list"]);
    assert!(out.contains("bbbb2222"), "reset did not re-offer: {out}");
}

#[test]
fn ledger_is_shared_between_main_and_worktree() {
    let c = build_corpus();
    init_with_worktree(&c);

    // Mark the worktree's own session from the main checkout...
    c.engramdb(&c.main, &["harvest", "mark", "cccc", "--memory", "m-1"])
        .assert()
        .success();

    // ...and it must be hidden when listing from the worktree too: one store,
    // one ledger.
    let out = c.stdout(&c.worktree, &["harvest", "list"]);
    assert!(
        !out.contains("cccc3333"),
        "ledger is not shared across the worktree boundary: {out}"
    );
}

#[test]
fn mark_can_reach_any_session_show_can_reach() {
    let c = build_corpus();
    init_with_worktree(&c);

    // `show --all-projects` reaches a session outside this project...
    let shown = c.stdout(&c.main, &["harvest", "show", "ffff6666", "--all-projects"]);
    assert!(shown.contains("ffff6666"), "{shown}");

    // ...so `mark` must be able to reach it too. Without a matching flag the
    // error even advised passing `--all-projects`, which did not exist.
    c.engramdb(&c.main, &["harvest", "mark", "ffff6666", "--all-projects"])
        .assert()
        .success();
}

#[test]
fn digest_is_labelled_untrusted() {
    let c = build_corpus();
    init_with_worktree(&c);

    let out = c.stdout(&c.main, &["harvest", "show", "aaaa"]);
    assert!(
        out.contains("Recorded transcript") && out.contains("not instructions"),
        "digest must warn that transcript content is data, not instructions: {out}"
    );
}

#[test]
fn unknown_session_is_a_clear_error() {
    let c = build_corpus();
    init_with_worktree(&c);

    let out = c
        .engramdb(&c.main, &["harvest", "show", "nope"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("No session matching"),
        "unhelpful error: {err}"
    );
}
