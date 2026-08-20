//! `config`, `stats`, `gc`, `compress`, `migrate`, `rollback`, `reindex`,
//! `challenge`.
//!
//! `config` and `stats` render almost entirely outside `OutputFormatter`
//! (25 and 27 bare `println!` sites respectively), so they get the full
//! three-format treatment here; the rest are default-format only.

use super::Fixture;

fn seeded() -> Fixture {
    let f = Fixture::new();
    f.init();
    f.seed();
    f
}

// =====================================================================
// config — renderer-thin, all three formats
// =====================================================================

#[test]
fn config_effective() {
    let f = seeded();
    snap_all_formats!(f, "config_effective", &["config"]);
}

#[test]
fn config_with_top_tags() {
    let f = Fixture::new();
    f.init();
    f.run(&[
        "add",
        "-t",
        "decision",
        "-s",
        "Tagged one",
        "-c",
        "C",
        "--tags",
        "alpha,beta",
    ]);
    f.run(&[
        "add",
        "-t",
        "hazard",
        "-s",
        "Tagged two",
        "-c",
        "C",
        "--tags",
        "alpha",
    ]);
    snap_all_formats!(f, "config_top_tags", &["config", "--top-tags", "5"]);
}

#[test]
fn config_on_uninitialized_store() {
    let f = Fixture::new();
    f.write_config_only();
    insta::assert_snapshot!("config_uninitialized", f.run(&["config"]));
}

// =====================================================================
// stats — renderer-thin, all three formats
// =====================================================================

#[test]
fn stats_empty_store() {
    let f = Fixture::new();
    f.init();
    snap_all_formats!(f, "stats_empty", &["stats"]);
}

#[test]
fn stats_seeded_store() {
    let f = seeded();
    snap_all_formats!(f, "stats_seeded", &["stats"]);
}

#[test]
fn stats_all_projects() {
    let f = seeded();
    snap_all_formats!(f, "stats_all_projects", &["stats", "--all-projects"]);
}

/// `stats` is where a user asks "what should I run next", so it has to be able
/// to say that the index is serving text the files no longer contain.
///
/// The edit here is the ordinary one — a hand edit, a `git checkout`, a
/// restore — and it is invisible to both existing checks: the memory count is
/// unchanged and so is the id set. Only the content digest can see it, which
/// is why this warning could not exist before.
#[test]
fn stats_reports_drift_after_a_file_is_edited_behind_the_store() {
    let f = seeded();
    let id = one_id(&f);
    let dir = f.path().join(".engramdb/memories");
    let path = std::fs::read_dir(&dir)
        .expect("memories dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.contains(&id))
        })
        .expect("file for the chosen id");
    let text = std::fs::read_to_string(&path).unwrap();
    let (frontmatter, _) = text.split_once("\n---\n").expect("frontmatter delimiter");
    std::fs::write(
        &path,
        format!("{frontmatter}\n---\nedited behind the store\n"),
    )
    .unwrap();

    insta::assert_snapshot!("stats_drifted", f.run(&["--format", "plain", "stats"]));
}

/// With `[daemon] enabled = false` and an unbound socket, this is the
/// "no daemon" branch — the only one reachable without spawning a process.
#[test]
fn stats_daemon() {
    let f = seeded();
    snap_all_formats!(f, "stats_daemon", &["stats", "--daemon"]);
}

// =====================================================================
// gc / compress
// =====================================================================

#[test]
fn gc_dry_run() {
    let f = seeded();
    insta::assert_snapshot!("gc_dry_run", f.run(&["gc"]));
}

#[test]
fn gc_confirmed() {
    let f = seeded();
    insta::assert_snapshot!("gc_confirmed", f.run(&["gc", "--confirm"]));
}

#[test]
fn gc_with_threshold() {
    let f = seeded();
    insta::assert_snapshot!("gc_with_threshold", f.run(&["gc", "--threshold", "0.95"]));
}

#[test]
fn compress_candidates() {
    let f = seeded();
    insta::assert_snapshot!("compress_candidates", f.run(&["compress"]));
}

#[test]
fn compress_with_threshold() {
    let f = seeded();
    insta::assert_snapshot!(
        "compress_with_threshold",
        f.run(&["compress", "--threshold", "0.1"])
    );
}

// =====================================================================
// migrate / rollback
// =====================================================================

#[test]
fn migrate_dry_run() {
    let f = seeded();
    insta::assert_snapshot!("migrate_dry_run", f.run(&["migrate", "--dry-run"]));
}

#[test]
fn migrate_applied() {
    let f = seeded();
    insta::assert_snapshot!("migrate_applied", f.run(&["migrate"]));
}

#[test]
fn rollback_dry_run() {
    let f = seeded();
    insta::assert_snapshot!("rollback_dry_run", f.run(&["rollback", "--dry-run"]));
}

/// Only version 1 is a supported rollback target; anything else is rejected
/// before the store is touched.
#[test]
fn rollback_unsupported_version_fails() {
    let f = seeded();
    insta::assert_snapshot!(
        "rollback_unsupported_version_fails",
        f.run(&["rollback", "--target-version", "99"])
    );
}

// =====================================================================
// reindex
// =====================================================================

/// The per-memory failure line reads `embedding failed`, with no cause.
///
/// It used to name one — `Failed to send embed request to Ollama` — and this
/// snapshot is what caught the change when master's batching work merged in.
/// `RetrievalEngine::embed_texts` returns `Vec<Option<Vec<f32>>>`, so a failure
/// arrives as a bare `None` and `embed_memories` has nothing to report
/// (`src/retrieval/engine.rs:493`). The batched path *structurally* cannot say
/// why, where the per-memory path it replaced could.
///
/// Pinned as-is because it is current behaviour, not because it is good: a
/// user whose reindex fails now cannot tell an unreachable Ollama from a
/// missing ONNX runtime. Restoring the detail means threading the error
/// through `embed_texts`, and this snapshot will flag that when it happens.
#[test]
fn reindex_full() {
    let f = seeded();
    insta::assert_snapshot!("reindex_full", f.run(&["reindex"]));
}

#[test]
fn reindex_index_only() {
    let f = seeded();
    insta::assert_snapshot!("reindex_index_only", f.run(&["reindex", "--index-only"]));
}

/// `--dry-run` reports and writes nothing — the human rendering.
///
/// `--format plain` because this tier's default is JSON (see
/// `reindex_index_only` above), and the per-category id listing exists only on
/// the human path.
///
/// The seeded memories are indexed but hold no vectors — `add` returns before
/// its detached ingest embeds — so this pins the distinction the report is
/// built around: they land in `not embedded`, never in `vectors out of date`.
/// Reporting a just-created memory as stale-vectored is the false positive an
/// agent that calls `create` then `reindex --dry-run` would hit every time.
#[test]
fn reindex_dry_run() {
    let f = seeded();
    insta::assert_snapshot!(
        "reindex_dry_run",
        f.run(&["--format", "plain", "reindex", "--dry-run"])
    );
}

/// The JSON shape agents and scripts consume — and the one-document rule: a
/// dry run in JSON mode emits exactly one object, with no human lines leaking
/// onto stdout beside it.
#[test]
fn reindex_dry_run_json() {
    let f = seeded();
    insta::assert_snapshot!(
        "reindex_dry_run_json",
        f.run(&["--format", "json", "reindex", "--dry-run"])
    );
}

// =====================================================================
// challenge
// =====================================================================

fn one_id(f: &Fixture) -> String {
    let dir = f.path().join(".engramdb/memories");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("memories dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .map(|e| e.path().file_stem().unwrap().to_string_lossy().to_string())
        .collect();
    // `read_dir` order is filesystem-defined; sort so the same memory is
    // challenged on every run.
    names.sort();
    names[0].rsplit('_').next().unwrap().to_string()
}

#[test]
fn challenge_existing() {
    let f = seeded();
    let id = one_id(&f);
    insta::assert_snapshot!(
        "challenge_existing",
        f.run(&["challenge", &id, "-e", "Contradicted by the new benchmark."])
    );
}

#[test]
fn challenge_with_source_file() {
    let f = seeded();
    let id = one_id(&f);
    insta::assert_snapshot!(
        "challenge_with_source_file",
        f.run(&[
            "challenge",
            &id,
            "-e",
            "Contradicted by the new benchmark.",
            "--source-file",
            "benches/retrieval.rs",
        ])
    );
}

#[test]
fn challenge_unknown_id_fails() {
    let f = seeded();
    insta::assert_snapshot!(
        "challenge_unknown_id_fails",
        f.run(&["challenge", "no-such-memory", "-e", "Because."])
    );
}
