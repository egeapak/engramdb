//! `engramdb harvest` — reading past Claude Code sessions, and the ledger
//! that records what was done with them.
//!
//! # What this tier adds over `tests/cli/harvest.rs`
//!
//! That file already drives the real binary against a synthetic corpus, and it
//! asserts *behaviour*: which sessions are in scope, that a worktree resolves
//! to its main project, that a foreign cwd in a colliding directory is never
//! offered. Those are claims about the scope resolver, and they belong there.
//!
//! These are claims about the **transcript a user sees**: the exact bytes of
//! every branch, which stream each line lands on, and the exit code. Nothing
//! in the other file pins a byte of output — it greps for substrings — so a
//! reworded hint, a lost stderr routing, or a suddenly-zero exit code would
//! pass there and fail here.
//!
//! # The corpus
//!
//! Three sessions, each planted for a branch of the renderer rather than for a
//! scope question:
//!
//! - `session-alpha` — several human turns, a git branch and a first prompt:
//!   the fully populated list row, and enough prose to digest.
//! - `session-beta` — one human turn, no branch: the singular "1 turn" and the
//!   row with no preview line.
//! - `session-gamma` — assistant and tool turns only. Never listed, because a
//!   session with no human turns is machine noise; `--include-empty` is the
//!   flag that admits it, and pinning both sides is what makes the default
//!   meaningful.
//!
//! Session ids are descriptive stems rather than uuids on purpose — see
//! [`Fixture::write_transcript`]. Timestamps are fixed, which matters because
//! `harvest list` prints `ended_at` directly.
//!
//! # Not covered, and why
//!
//! `harvest index` / `harvest search` need an embedding model, and the fixture
//! runs with an empty model cache and `ENGRAMDB_OFFLINE` (see `fixture_config`)
//! precisely so nothing downloads. Their *unavailable* branch is pinned below,
//! which is the one a machine without a runtime hits; the indexed path needs a
//! model and belongs to the ml-models test group, not to a snapshot tier.

use super::Fixture;

/// A `user` turn, as Claude Code writes it.
fn user(cwd: &str, ts: &str, text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "cwd": cwd,
        "gitBranch": "main",
        "timestamp": ts,
        "message": { "role": "user", "content": text }
    })
    .to_string()
}

/// A `user` turn with an explicit branch, so a row can render `[branch]`.
fn user_on_branch(cwd: &str, ts: &str, branch: &str, text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "cwd": cwd,
        "gitBranch": branch,
        "timestamp": ts,
        "message": { "role": "user", "content": text }
    })
    .to_string()
}

fn assistant(cwd: &str, ts: &str, text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "cwd": cwd,
        "timestamp": ts,
        "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] }
    })
    .to_string()
}

/// Build the three-session corpus described in the module docs.
fn corpus() -> Fixture {
    let f = Fixture::new();
    f.init();
    let cwd = f.path().to_string_lossy().to_string();

    f.write_transcript(
        "session-alpha",
        &[
            user_on_branch(
                &cwd,
                "2026-03-04T09:00:00Z",
                "feat/harvest",
                "Why does the daemon reap while a session is still open?",
            ),
            assistant(
                &cwd,
                "2026-03-04T09:00:30Z",
                "The idle watchdog only counts served requests, so a connected \
                 session that is not asking for inference looks idle.",
            ),
            user(
                &cwd,
                "2026-03-04T09:05:00Z",
                "So a heartbeat ping would keep it resident?",
            ),
            assistant(
                &cwd,
                "2026-03-04T09:05:20Z",
                "Yes — every served request refreshes last_activity, Ping included.",
            ),
        ],
    );

    f.write_transcript(
        "session-beta",
        &[user(
            &cwd,
            "2026-03-05T14:15:00Z",
            "What is the default digest budget?",
        )],
    );

    f.write_transcript(
        "session-gamma",
        &[assistant(
            &cwd,
            "2026-03-06T08:00:00Z",
            "Compacting conversation history.",
        )],
    );

    f
}

// =====================================================================
// list
// =====================================================================

/// No transcripts at all: the empty branch names the scope it searched and
/// points at the three ways out (`--include-harvested`, `--all-projects`, the
/// ledger). That guidance is the whole value of the branch — "no sessions" is
/// otherwise indistinguishable from a scope that resolved somewhere wrong.
#[test]
fn list_with_no_transcripts() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!("harvest_list_empty", f.run(&["harvest", "list"]));
}

#[test]
fn list_sessions() {
    let f = corpus();
    insta::assert_snapshot!("harvest_list", f.run(&["harvest", "list"]));
}

/// The same listing as human rows.
///
/// Worth its own case even though tier 1 owns the format matrix: `run` has no
/// terminal, and `OutputFormatter::new` falls back to Json without one — so
/// every other case here, including `list_sessions` above, is a *JSON*
/// snapshot no matter that it passes no `--format`. `--format plain` is the
/// only way this tier reaches the row layout at all, and it is the layout that
/// carries the turn count, the `[branch]` tag and the dimmed preview.
#[test]
fn list_sessions_plain() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_list_plain",
        f.run(&["--format", "plain", "harvest", "list"])
    );
}

/// `session-gamma` has no human turns, so it is absent from every listing
/// above. This is the flag that admits it.
#[test]
fn list_include_empty_admits_the_machine_only_session() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_list_include_empty",
        f.run(&["harvest", "list", "--include-empty"])
    );
}

#[test]
fn list_respects_limit() {
    let f = corpus();
    insta::assert_snapshot!("harvest_list_limit", f.run(&["harvest", "list", "-n", "1"]));
}

/// `--since` takes a relative shorthand as well as RFC 3339. The corpus is
/// pinned to March 2026, so a `1h` window is always empty — which is the
/// deterministic assertion here, and it pins that the flag parses at all.
#[test]
fn list_since_window_excludes_everything() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_list_since_recent",
        f.run(&["harvest", "list", "--since", "1h"])
    );
}

#[test]
fn list_rejects_an_unparseable_since() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_list_since_invalid",
        f.run(&["harvest", "list", "--since", "yesterday-ish"])
    );
}

// =====================================================================
// show — the digest
// =====================================================================

#[test]
fn show_digest() {
    let f = corpus();
    insta::assert_snapshot!("harvest_show", f.run(&["harvest", "show", "session-alpha"]));
}

/// `--no-tools` and `--max-chars` are the two knobs an agent reaches for when
/// a digest will not fit. A budget small enough to bite pins the truncation
/// path rather than the happy one.
#[test]
fn show_digest_within_a_small_budget() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_show_budgeted",
        f.run(&[
            "harvest",
            "show",
            "session-alpha",
            "--no-tools",
            "--max-chars",
            "400"
        ])
    );
}

#[test]
fn show_unknown_session_fails() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_show_unknown",
        f.run(&["harvest", "show", "session-omega"])
    );
}

/// A prefix shared by two sessions must not silently pick one.
#[test]
fn show_ambiguous_prefix_fails() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_show_ambiguous",
        f.run(&["harvest", "show", "session-"])
    );
}

// =====================================================================
// mark / reset / ledger
// =====================================================================

/// Marking with no `--memory` is the zero-yield review: the session was read,
/// nothing came of it, and recording that is what stops it being offered again.
#[test]
fn mark_without_memories_then_list() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_mark_no_yield",
        f.run(&[
            "harvest",
            "mark",
            "session-alpha",
            "--note",
            "Nothing durable — all of it is already in the daemon docs."
        ])
    );
    // Marked sessions drop out of the default listing…
    insta::assert_snapshot!("harvest_list_after_mark", f.run(&["harvest", "list"]));
    // …and come back, tagged, with --include-harvested.
    insta::assert_snapshot!(
        "harvest_list_include_harvested",
        f.run(&["harvest", "list", "--include-harvested"])
    );
}

/// `--defer` is the third state: reviewed, not settled, still offered.
#[test]
fn mark_deferred_stays_in_the_listing() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_mark_defer",
        f.run(&[
            "harvest",
            "mark",
            "session-alpha",
            "--defer",
            "--note",
            "Worth a second pass once the reranker lands."
        ])
    );
    insta::assert_snapshot!("harvest_list_after_defer", f.run(&["harvest", "list"]));
}

#[test]
fn mark_unknown_session_fails() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_mark_unknown",
        f.run(&["harvest", "mark", "session-omega"])
    );
}

#[test]
fn ledger_list_when_empty() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_ledger_list_empty",
        f.run(&["harvest", "ledger", "list"])
    );
}

#[test]
fn ledger_list_and_show_after_a_mark() {
    let f = corpus();
    f.run(&[
        "harvest",
        "mark",
        "session-alpha",
        "--note",
        "Superseded by the daemon design note.",
    ]);
    insta::assert_snapshot!("harvest_ledger_list", f.run(&["harvest", "ledger", "list"]));
    insta::assert_snapshot!(
        "harvest_ledger_show",
        f.run(&["harvest", "ledger", "show", "session-alpha"])
    );
}

/// `summary` attaches curated prose to a session's search row, re-embedding
/// only that row — so with no model the write cannot complete, and this is the
/// branch a machine without a runtime hits.
#[test]
fn summary_without_a_model() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_summary_no_model",
        f.run(&[
            "harvest",
            "summary",
            "session-alpha",
            "A daemon idle-timeout investigation."
        ])
    );
}

#[test]
fn summary_unknown_session_fails() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_summary_unknown",
        f.run(&["harvest", "summary", "session-omega", "Anything."])
    );
}

/// `ledger export` writes a stored transcript copy back out. Only the
/// SessionEnd hook makes copies, so nothing here has one — and the contract is
/// that it says so rather than writing an empty file.
#[test]
fn ledger_export_without_an_archive_fails() {
    let f = corpus();
    f.run(&["harvest", "mark", "session-alpha"]);
    insta::assert_snapshot!(
        "harvest_ledger_export_no_archive",
        f.run(&["harvest", "ledger", "export", "session-alpha"])
    );
}

/// `ledger rm --force` drops the review record, so the session returns to the
/// default listing exactly as `reset` would leave it.
#[test]
fn ledger_rm_drops_the_record() {
    let f = corpus();
    f.run(&[
        "harvest",
        "mark",
        "session-alpha",
        "--note",
        "Nothing durable.",
    ]);
    insta::assert_snapshot!(
        "harvest_ledger_rm",
        f.run(&["harvest", "ledger", "rm", "session-alpha", "--force"])
    );
    insta::assert_snapshot!(
        "harvest_ledger_rm_after",
        f.run(&["harvest", "ledger", "list"])
    );
}

/// Without `--force` the removal is confirmed, and `Fixture::run` has no
/// terminal — so this pins the refusal a script sees, not an interactive flow.
#[test]
fn ledger_rm_without_force_does_not_remove() {
    let f = corpus();
    f.run(&["harvest", "mark", "session-alpha"]);
    insta::assert_snapshot!(
        "harvest_ledger_rm_needs_force",
        f.run(&["harvest", "ledger", "rm", "session-alpha"])
    );
}

#[test]
fn ledger_rm_unknown_session_fails() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_ledger_rm_unknown",
        f.run(&["harvest", "ledger", "rm", "session-omega", "--force"])
    );
}

#[test]
fn ledger_show_unknown_session_fails() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_ledger_show_unknown",
        f.run(&["harvest", "ledger", "show", "session-omega"])
    );
}

/// `reset` is the undo for `mark`: the session returns to the default listing.
#[test]
fn reset_returns_a_session_to_the_listing() {
    let f = corpus();
    f.run(&["harvest", "mark", "session-alpha"]);
    insta::assert_snapshot!(
        "harvest_reset",
        f.run(&["harvest", "reset", "session-alpha"])
    );
    insta::assert_snapshot!("harvest_list_after_reset", f.run(&["harvest", "list"]));
}

#[test]
fn reset_an_unmarked_session_says_so() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_reset_unmarked",
        f.run(&["harvest", "reset", "session-beta"])
    );
}

/// Nothing has been archived — only the SessionEnd hook writes copies — so
/// this is the empty-budget branch, and it must not claim to have freed
/// anything.
#[test]
fn ledger_prune_dry_run_with_no_archives() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_ledger_prune_dry_run",
        f.run(&["harvest", "ledger", "prune"])
    );
}

// =====================================================================
// index / search — the no-model branch
// =====================================================================

/// The fixture has no embedding model and cannot fetch one, so both of these
/// take the unavailable route. That is the branch worth pinning at this tier:
/// it is what a fresh install without an ONNX runtime hits, and the contract
/// is that it explains itself rather than panicking or exiting silently.
#[test]
fn index_without_a_model() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_index_no_model",
        f.run(&["harvest", "index", "session-alpha"])
    );
}

#[test]
fn search_without_a_model() {
    let f = corpus();
    insta::assert_snapshot!(
        "harvest_search_no_model",
        f.run(&["harvest", "search", "daemon idle timeout"])
    );
}
