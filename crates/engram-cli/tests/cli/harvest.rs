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
//!   must never be offered. The parent/child tests reuse it as a *linked*
//!   sub-project, where the opposite holds: once linked it shares the root's
//!   scope, ledger, and archive directory.

use super::helpers::{cmd, data_dir};
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

/// Locate the archive written for `session`, if any.
///
/// Searches `<data>/projects/*/transcripts/` rather than recomputing the
/// project id, which is a hash of a per-test temp path. Every test fixture
/// gets a fresh project, so a hit is unambiguous.
fn find_archive(session: &str) -> Option<PathBuf> {
    let projects = data_dir().join("projects");
    let wanted = format!("{session}.jsonl.zst");
    for project in std::fs::read_dir(projects).ok()?.flatten() {
        let candidate = project.path().join("transcripts").join(&wanted);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// A built corpus: the temp dirs must stay alive for the duration of a test.
struct Corpus {
    _tmp: TempDir,
    claude: PathBuf,
    main: PathBuf,
    /// A second, independent project whose path shares a prefix with `main`.
    /// Used both as the out-of-scope fixture and, once linked, as the
    /// sub-project half of the parent/child tests.
    sibling: PathBuf,
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

    /// Fire the SessionEnd hook for a session, as Claude Code would.
    ///
    /// This is the only path that archives a transcript, so any test about
    /// archives has to go through it rather than calling the archive helper
    /// directly.
    fn session_end(&self, dir: &Path, session: &str) {
        let transcript = self
            .claude
            .join("projects")
            .join(encode(dir))
            .join(format!("{session}.jsonl"));
        self.session_end_with(dir, session, &transcript);
    }

    /// Fire SessionEnd with an explicit `session_id` / `transcript_path` pair,
    /// so a test can supply a hostile id alongside a perfectly valid file.
    fn session_end_with(&self, dir: &Path, session: &str, transcript: &Path) {
        let event = serde_json::json!({
            "session_id": session,
            "transcript_path": transcript.to_string_lossy(),
            "cwd": dir.to_string_lossy(),
        })
        .to_string();
        self.engramdb(dir, &["hook", "session-end"])
            .write_stdin(event)
            .assert()
            .success();
    }

    /// The registry id of the project at `dir` — what `projects link` takes.
    fn project_id(&self, dir: &Path) -> String {
        let out = cmd()
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .arg("--dir")
            .arg(dir)
            .args(["--json", "projects", "info"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "projects info failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        value["project_id"].as_str().unwrap().to_string()
    }

    /// The harvest ledger file for a project directory.
    fn ledger_file(&self, dir: &Path) -> PathBuf {
        dir.join(".engramdb")
            .join("state")
            .join("harvested_sessions.json")
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
        sibling,
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

/// Initialize both projects and make `sibling` a sub-project of `main` with
/// `projects link`.
///
/// Deliberately *not* a git worktree: `resolve_project_root` rewrites the
/// working directory only for worktrees, so a linked sub-project is the case
/// where nothing upstream has already collapsed the two paths into one. It
/// still shares the parent's harvest scope and archive directory, which is
/// what makes a split ledger a bug rather than a preference.
fn init_linked_projects(c: &Corpus) {
    c.engramdb(&c.main, &["init", "--no-embeddings"])
        .assert()
        .success();
    c.engramdb(&c.sibling, &["init", "--no-embeddings"])
        .assert()
        .success();
    link_sibling_to_main(c);
}

fn link_sibling_to_main(c: &Corpus) {
    let child = c.project_id(&c.sibling);
    let parent = c.project_id(&c.main);
    c.engramdb(&c.main, &["projects", "link", &child, "--parent", &parent])
        .assert()
        .success();
}

/// A parent and a linked sub-project list the *same* sessions, so a review
/// recorded from either side has to settle it for both. Splitting the ledger
/// by invoking directory means each one re-offers forever what the other
/// already decided.
#[test]
fn a_linked_sub_project_shares_the_root_ledger() {
    let c = build_corpus();
    init_linked_projects(&c);

    // Both directions: the parent's session marked from the child...
    c.engramdb(&c.sibling, &["harvest", "mark", "aaaa"])
        .assert()
        .success();
    let from_main = c.stdout(&c.main, &["harvest", "list"]);
    assert!(
        !from_main.contains("aaaa1111"),
        "a session settled in the sub-project is still offered by the root: {from_main}"
    );

    // ...and the child's session marked from the parent.
    c.engramdb(&c.main, &["harvest", "mark", "ffff6666"])
        .assert()
        .success();
    let from_sibling = c.stdout(&c.sibling, &["harvest", "list"]);
    assert!(
        !from_sibling.contains("ffff6666"),
        "a session settled in the root is still offered by the sub-project: {from_sibling}"
    );

    // One ledger, at the root — the same place the archives are keyed by.
    assert!(
        !c.ledger_file(&c.sibling).exists(),
        "the sub-project kept a ledger of its own"
    );
}

/// The SessionEnd hook prunes archives on every session, from whichever
/// directory the session ran in. Pruning is keyed by the *root* project, so a
/// sweep run in the sub-project deletes the parent's archives — and must clear
/// the same ledger those files are recorded in, or the parent advertises a
/// transcript with a sha256 that no longer exists and `export` fails.
#[test]
fn pruning_from_a_linked_sub_project_clears_the_root_ledger() {
    let c = build_corpus();
    init_linked_projects(&c);
    c.session_end(&c.main, "aaaa1111-rich");
    assert!(
        find_archive("aaaa1111-rich").is_some(),
        "positive control: nothing was archived"
    );

    c.engramdb(
        &c.sibling,
        &["harvest", "ledger", "prune", "--max-bytes", "1", "--apply"],
    )
    .assert()
    .success();
    assert!(
        find_archive("aaaa1111-rich").is_none(),
        "the sub-project's prune did not reach the root's archives"
    );

    let show = c.stdout(&c.main, &["harvest", "ledger", "show", "aaaa"]);
    assert!(
        show.contains("Archive:   none"),
        "the root still advertises an archive the sub-project deleted: {show}"
    );
}

/// A ledger written under a sub-project's own path — every review recorded
/// before it was linked, or by an older version — must not become invisible
/// when the root takes over.
#[test]
fn a_ledger_left_at_a_sub_project_path_is_adopted_by_the_root() {
    let c = build_corpus();
    c.engramdb(&c.main, &["init", "--no-embeddings"])
        .assert()
        .success();
    c.engramdb(&c.sibling, &["init", "--no-embeddings"])
        .assert()
        .success();

    // Reviewed while `sibling` was still a root project of its own.
    c.engramdb(&c.sibling, &["harvest", "mark", "ffff6666"])
        .assert()
        .success();
    assert!(
        c.ledger_file(&c.sibling).exists(),
        "fixture wrote no ledger"
    );

    link_sibling_to_main(&c);

    let from_main = c.stdout(&c.main, &["harvest", "list"]);
    assert!(
        !from_main.contains("ffff6666"),
        "linking silently discarded the sub-project's review decisions: {from_main}"
    );
    // Kept, not deleted: nothing here is worth destroying evidence over.
    assert!(
        !c.ledger_file(&c.sibling).exists()
            && c.ledger_file(&c.sibling)
                .with_extension("json.adopted")
                .exists(),
        "the old ledger was not moved aside"
    );
}

/// An archive is only reachable *through* its ledger entry, so the entry must
/// outlive the entry-retention window whenever a file is still behind it.
/// `archive_retention_days` defaults to a year and the docs tell users to set
/// 3650, so the ledger's own 365-day sweep otherwise strands the file: no
/// `show`, no `export`, no `ledger list`, and 2 GiB of budget still spoken for.
#[test]
fn an_archived_session_survives_the_ledger_age_sweep() {
    let c = build_corpus();
    init_with_worktree(&c);
    c.session_end(&c.main, "aaaa1111-rich");

    // Age the entry past the ledger's retention window.
    let path = c.ledger_file(&c.main);
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut ledger: serde_json::Value = serde_json::from_str(&raw).unwrap();
    ledger["aaaa1111-rich"]["harvested_at"] = serde_json::json!("2020-01-01T00:00:00Z");
    std::fs::write(&path, serde_json::to_string_pretty(&ledger).unwrap()).unwrap();

    // Any later session end runs the sweep — unattended, on every session.
    c.session_end(&c.main, "bbbb2222-empty");

    assert!(
        find_archive("aaaa1111-rich").is_some(),
        "the archive file itself must still be there"
    );
    let show = c.stdout(&c.main, &["harvest", "ledger", "show", "aaaa"]);
    assert!(
        show.contains(".jsonl.zst"),
        "the archive became unreachable when its entry aged out: {show}"
    );
    let dest = c.main.join("restored.jsonl");
    c.engramdb(
        &c.main,
        &[
            "harvest",
            "ledger",
            "export",
            "aaaa",
            "-o",
            dest.to_str().unwrap(),
        ],
    )
    .assert()
    .success();
    assert!(dest.is_file(), "export produced no file");
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

/// Archiving a transcript must not remove it from the review queue, and
/// evicting the archive must not leave the ledger advertising a file that is
/// no longer there.
/// `ledger rm` destroys the only remaining copy of a conversation, so it must
/// confirm first — and non-interactively (no TTY) that means it aborts.
#[test]
fn ledger_rm_without_force_aborts_non_interactively() {
    let c = build_corpus();
    init_with_worktree(&c);
    c.session_end(&c.main, "aaaa1111-rich");

    let out = c.stdout(&c.main, &["harvest", "ledger", "rm", "aaaa"]);
    assert!(out.contains("Aborted"), "expected an abort: {out}");
    assert!(
        find_archive("aaaa1111-rich").is_some(),
        "the archive was deleted without confirmation"
    );
    // The record must survive too.
    let show = c.stdout(&c.main, &["harvest", "ledger", "show", "aaaa"]);
    assert!(show.contains(".jsonl.zst"), "{show}");
}

#[test]
fn ledger_rm_force_removes_entry_and_archive() {
    let c = build_corpus();
    init_with_worktree(&c);
    c.session_end(&c.main, "aaaa1111-rich");

    let out = c.stdout(&c.main, &["harvest", "ledger", "rm", "aaaa", "--force"]);
    assert!(out.contains("ledger entry and archive"), "{out}");
    assert!(find_archive("aaaa1111-rich").is_none());
    // Dropping the record re-offers the session — the opposite of
    // `--archive-only`, which keeps it settled.
    let list = c.stdout(&c.main, &["harvest", "list"]);
    assert!(list.contains("aaaa1111"), "rm did not re-offer: {list}");
}

#[test]
fn ledger_rm_archive_only_reports_nothing_when_there_is_no_archive() {
    let c = build_corpus();
    init_with_worktree(&c);
    // Reviewed but never archived.
    c.engramdb(&c.main, &["harvest", "mark", "bbbb"])
        .assert()
        .success();

    let out = c.stdout(
        &c.main,
        &[
            "harvest",
            "ledger",
            "rm",
            "bbbb",
            "--archive-only",
            "--force",
        ],
    );
    assert!(
        !out.contains("Removed archive"),
        "reported removing an archive that never existed: {out}"
    );
    // ...and the decision record is untouched, so it stays settled.
    let list = c.stdout(&c.main, &["harvest", "list"]);
    assert!(!list.contains("bbbb2222"), "{list}");
}

#[test]
fn ledger_rm_in_json_mode_requires_force() {
    let c = build_corpus();
    init_with_worktree(&c);
    c.session_end(&c.main, "aaaa1111-rich");

    let out = cmd()
        .env("CLAUDE_CONFIG_DIR", &c.claude)
        .arg("--dir")
        .arg(&c.main)
        .args(["--format", "json", "harvest", "ledger", "rm", "aaaa"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "JSON mode must not prompt");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--force"), "{err}");
    assert!(find_archive("aaaa1111-rich").is_some());
}

/// The privacy switch. Every failure inside `archive_ending_session` is
/// swallowed by `tracing::debug!`, so a regression here is silent — nothing
/// is the expected output either way. The positive control is what makes the
/// negative assertion mean anything.
#[test]
fn archive_false_writes_no_transcript_copy() {
    let c = build_corpus();
    init_with_worktree(&c);
    std::fs::write(
        c.main.join(".engramdb").join("config.toml"),
        "[harvest]\narchive = false\n",
    )
    .unwrap();

    c.session_end(&c.main, "aaaa1111-rich");

    assert!(
        find_archive("aaaa1111-rich").is_none(),
        "archiving was disabled but a copy was written"
    );
    // No ledger entry either — `set_archive` never ran.
    c.engramdb(&c.main, &["harvest", "ledger", "show", "aaaa"])
        .assert()
        .failure();
    // ...and the session is still offered for review.
    let list = c.stdout(&c.main, &["harvest", "list"]);
    assert!(list.contains("aaaa1111"), "{list}");
}

#[test]
fn archive_default_on_writes_a_transcript_copy() {
    let c = build_corpus();
    init_with_worktree(&c);
    c.session_end(&c.main, "aaaa1111-rich");
    assert!(
        find_archive("aaaa1111-rich").is_some(),
        "the positive control failed: archiving is broken outright"
    );
}

/// `--defer` is the one decision that deliberately does NOT settle a session.
#[test]
fn deferring_keeps_the_session_offered_and_records_the_note() {
    let c = build_corpus();
    init_with_worktree(&c);

    c.engramdb(
        &c.main,
        &[
            "harvest",
            "mark",
            "bbbb",
            "--defer",
            "--note",
            "needs the author",
        ],
    )
    .assert()
    .success();

    let list = c.stdout(&c.main, &["harvest", "list"]);
    assert!(
        list.contains("bbbb2222"),
        "a deferred session must keep appearing: {list}"
    );
    let show = c.stdout(&c.main, &["harvest", "ledger", "show", "bbbb"]);
    assert!(show.contains("Deferred"), "{show}");
    assert!(show.contains("needs the author"), "{show}");

    // Settling it for real removes it.
    c.engramdb(&c.main, &["harvest", "mark", "bbbb"])
        .assert()
        .success();
    let list = c.stdout(&c.main, &["harvest", "list"]);
    assert!(!list.contains("bbbb2222"), "{list}");
}

#[test]
fn defer_conflicts_with_recording_memories() {
    let c = build_corpus();
    init_with_worktree(&c);
    // Guards a clap `conflicts_with` string, which is only checked at runtime
    // and silently stops working if the field is renamed.
    c.engramdb(
        &c.main,
        &["harvest", "mark", "bbbb", "--defer", "--memory", "m-1"],
    )
    .assert()
    .failure();
}

/// The SessionEnd hook is registered machine-wide by the plugin, so it fires
/// in directories that are not EngramDB projects at all. It must write
/// nothing there — no `state/` tree, no archive.
#[test]
fn session_end_writes_nothing_in_an_uninitialized_directory() {
    let c = build_corpus();
    // Deliberately NOT `init_with_worktree` — this is a bare directory.
    let bare = c.main.join("not-a-project");
    std::fs::create_dir_all(&bare).unwrap();
    write_transcript(
        &c.claude,
        &bare,
        "eeee5555-bare",
        &[user_line(
            &bare,
            "2026-07-25T09:00:00Z",
            "just passing through",
        )],
    );

    c.session_end(&bare, "eeee5555-bare");

    assert!(
        !bare.join(".engramdb").exists(),
        "SessionEnd created state in a directory that was never `engramdb init`ed"
    );
    assert!(
        find_archive("eeee5555-bare").is_none(),
        "SessionEnd archived a transcript for an uninitialized project"
    );
}

/// The premise of the whole archive: Claude Code prunes its own transcripts,
/// and a session must stay readable afterwards. If `show` only ever reads the
/// live `.jsonl`, archiving preserves bytes nobody can use.
#[test]
fn an_archived_session_is_still_readable_after_its_transcript_is_pruned() {
    let c = build_corpus();
    init_with_worktree(&c);

    c.session_end(&c.main, "aaaa1111-rich");
    std::fs::remove_file(
        c.claude
            .join("projects")
            .join(encode(&c.main))
            .join("aaaa1111-rich.jsonl"),
    )
    .unwrap();

    let out = c.stdout(&c.main, &["harvest", "show", "aaaa"]);
    assert!(
        out.contains("nextest"),
        "a pruned session must still digest from its archive: {out}"
    );
    assert!(out.contains("CI is red"), "{out}");
}

/// After Claude Code prunes a transcript, `show` reads the archive — and
/// whatever `show` can display, `mark` must be able to settle, or the ledger
/// re-offers it forever. The restored digest must also report the session's
/// real id, not the temp file it was restored into.
#[test]
fn a_pruned_session_reports_its_real_id_and_can_still_be_marked() {
    let c = build_corpus();
    init_with_worktree(&c);
    c.session_end(&c.main, "aaaa1111-rich");
    std::fs::remove_file(
        c.claude
            .join("projects")
            .join(encode(&c.main))
            .join("aaaa1111-rich.jsonl"),
    )
    .unwrap();

    let out = c.stdout(&c.main, &["harvest", "show", "aaaa"]);
    assert!(
        out.contains("## Session aaaa1111-rich"),
        "restored digest must carry the real session id: {out}"
    );

    let marked = c.stdout(&c.main, &["harvest", "mark", "aaaa"]);
    assert!(marked.contains("aaaa1111-rich"), "{marked}");
    let show = c.stdout(&c.main, &["harvest", "ledger", "show", "aaaa"]);
    assert!(
        show.contains("Skipped"),
        "the mark was not recorded: {show}"
    );
}

/// A session started in a *subdirectory* of the project belongs to it. Claude
/// Code names the transcript directory after the session's cwd, so requiring
/// the directory to encode the project root exactly made every such session
/// permanently invisible.
#[test]
fn sessions_started_in_a_subdirectory_are_found() {
    let c = build_corpus();
    init_with_worktree(&c);

    let sub = c.main.join("crates").join("engram-cli");
    std::fs::create_dir_all(&sub).unwrap();
    write_transcript(
        &c.claude,
        &sub,
        "dddd4444-subdir",
        &[user_line(
            &sub,
            "2026-07-24T09:00:00Z",
            "ran the build from inside the crate directory",
        )],
    );

    let out = c.stdout(&c.main, &["harvest", "list"]);
    assert!(
        out.contains("dddd4444"),
        "a session started in a subdirectory was not offered: {out}"
    );
}

/// Session ids reach `Path::join`, so a traversal id must be refused rather
/// than writing an archive outside the data directory.
#[test]
fn a_traversing_session_id_cannot_escape_the_archive_directory() {
    let c = build_corpus();
    init_with_worktree(&c);

    // A hostile id paired with a real, readable transcript — the shape a
    // forged hook event takes.
    let real = c
        .claude
        .join("projects")
        .join(encode(&c.main))
        .join("aaaa1111-rich.jsonl");
    let escape = c.main.join("escaped.jsonl.zst");
    c.session_end_with(&c.main, "../../../../escaped", &real);
    assert!(
        !escape.exists(),
        "a traversing session id escaped the archive directory"
    );

    // And the ledger must not accept the poisoned key either, since
    // `ledger rm` would later aim `remove_file` at it.
    c.engramdb(&c.main, &["harvest", "mark", "../../../../escaped"])
        .assert()
        .failure();
}

#[test]
fn archiving_then_pruning_leaves_a_consistent_ledger() {
    let c = build_corpus();
    init_with_worktree(&c);

    c.session_end(&c.main, "aaaa1111-rich");

    // An archived session is only *stored*, not reviewed — it must still be
    // offered. Reading presence in the ledger as "already handled" would hide
    // every session the SessionEnd hook ever touched.
    let out = c.stdout(&c.main, &["harvest", "list"]);
    assert!(
        out.contains("aaaa1111"),
        "archiving removed a session from the review queue: {out}"
    );

    let out = c.stdout(&c.main, &["harvest", "ledger", "show", "aaaa"]);
    assert!(out.contains(".jsonl.zst"), "{out}");

    // Evict it, then the ledger must stop claiming an archive exists...
    c.engramdb(
        &c.main,
        &["harvest", "ledger", "prune", "--max-bytes", "1", "--apply"],
    )
    .assert()
    .success();

    let out = c.stdout(&c.main, &["harvest", "ledger", "show", "aaaa"]);
    assert!(
        out.contains("Archive:   none"),
        "ledger still points at an evicted archive: {out}"
    );

    // ...and export must explain itself rather than surfacing a bare IO error.
    let err = String::from_utf8_lossy(
        &c.engramdb(&c.main, &["harvest", "ledger", "export", "aaaa"])
            .assert()
            .failure()
            .get_output()
            .stderr
            .clone(),
    )
    .to_string();
    assert!(
        err.contains("no archived transcript") || err.contains("archive"),
        "unhelpful export error: {err}"
    );
}

/// A session that ends inside a linked worktree must archive into the *root*
/// project's directory, or the main checkout could never export it: the
/// ledger is shared across the worktree boundary, so the archives must be too.
#[test]
fn worktree_sessions_archive_into_the_root_project() {
    let c = build_corpus();
    init_with_worktree(&c);

    c.session_end(&c.worktree, "cccc3333-worktree");

    let out = c.stdout(&c.main, &["harvest", "ledger", "show", "cccc"]);
    assert!(
        out.contains(".jsonl.zst"),
        "worktree archive is invisible from the main checkout: {out}"
    );

    let dest = c.main.join("restored.jsonl");
    c.engramdb(
        &c.main,
        &[
            "harvest",
            "ledger",
            "export",
            "cccc",
            "-o",
            dest.to_str().unwrap(),
        ],
    )
    .assert()
    .success();
    assert!(dest.is_file(), "export produced no file");
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
    // A pruned session is the common reason a prefix stops resolving, and the
    // archive is the only thing that still answers it — so the error has to
    // name that route, not just `harvest list` and `--all-projects`.
    assert!(
        err.contains("harvest ledger list"),
        "the archive recovery route must be named: {err}"
    );
}

#[test]
fn an_empty_listing_names_the_archive_route() {
    // `harvest list` reads live transcripts only. In a scope whose sessions
    // Claude Code has already pruned, "no unharvested sessions" is exactly
    // what a full archive looks like.
    let c = build_corpus();
    let empty = c._tmp.path().join("no-sessions");
    std::fs::create_dir_all(&empty).unwrap();
    c.engramdb(&empty, &["init"]).assert().success();

    let out = c.stdout(&empty, &["harvest", "list"]);
    assert!(out.contains("No unharvested sessions"), "{out}");
    assert!(
        out.contains("harvest ledger list"),
        "an empty listing must point at the archive: {out}"
    );
}
