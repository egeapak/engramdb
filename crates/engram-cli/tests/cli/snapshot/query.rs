//! `engramdb query` in both modes.
//!
//! Scores are *not* redacted. With no embedding provider the composite
//! weights renormalize deterministically (`src/scoring/composite.rs`), so the
//! `[0.xx]` values are stable — and a silent change in how relevance is
//! combined is exactly the kind of regression worth catching.

use super::Fixture;

fn seeded() -> Fixture {
    let f = Fixture::new();
    let src = f.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
    f.init();
    f.run(&[
        "add",
        "-t",
        "decision",
        "-s",
        "Use Rust for the backend",
        "-c",
        "Chosen for the memory-safety guarantees.",
        "-p",
        "src/main.rs",
        "-l",
        "core.lang",
        "--tags",
        "rust,backend",
        "--criticality",
        "0.9",
    ]);
    f.run(&[
        "add",
        "-t",
        "convention",
        "-s",
        "Name variables in snake_case",
        "-c",
        "Applies to every crate in the workspace.",
        "-l",
        "core.style",
        "--tags",
        "style",
        "--criticality",
        "0.6",
    ]);
    f.run(&[
        "add",
        "-t",
        "hazard",
        "-s",
        "Never unwrap in production paths",
        "-c",
        "A panic in the daemon takes down every session.",
        "-p",
        "src/daemon/server.rs",
        "--criticality",
        "0.3",
    ]);
    f
}

// =====================================================================
// filter mode
// =====================================================================

#[test]
fn query_filter_by_query_text() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_filter_by_query_text",
        f.run(&["query", "--mode", "filter", "--query", "rust"])
    );
}

#[test]
fn query_filter_positional_query() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_filter_positional_query",
        f.run(&["query", "--mode", "filter", "snake_case"])
    );
}

#[test]
fn query_filter_by_type() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_filter_by_type",
        f.run(&["query", "--mode", "filter", "--query", "e", "-t", "hazard"])
    );
}

#[test]
fn query_filter_by_logical_scope() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_filter_by_logical_scope",
        f.run(&["query", "--mode", "filter", "-l", "core.style"])
    );
}

#[test]
fn query_filter_by_tags() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_filter_by_tags",
        f.run(&["query", "--mode", "filter", "--tags", "backend"])
    );
}

#[test]
fn query_filter_no_matches() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_filter_no_matches",
        f.run(&["query", "--mode", "filter", "--query", "zzzznotpresent"])
    );
}

/// Filter mode needs at least one positive signal; `min_criticality` alone is
/// a narrowing filter, not a signal, so it is rejected.
#[test]
fn query_filter_without_signal_fails() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_filter_without_signal_fails",
        f.run(&["query", "--mode", "filter"])
    );
}

#[test]
fn query_filter_min_criticality_alone_fails() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_filter_min_criticality_alone_fails",
        f.run(&["query", "--mode", "filter", "--min-criticality", "0.5"])
    );
}

// =====================================================================
// rank mode
// =====================================================================

#[test]
fn query_rank_by_path() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_rank_by_path",
        f.run(&["query", "--mode", "rank", "-p", "src/main.rs"])
    );
}

#[test]
fn query_rank_with_scores() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_rank_with_scores",
        f.run(&[
            "query",
            "--mode",
            "rank",
            "-p",
            "src/main.rs",
            "--show-scores"
        ])
    );
}

#[test]
fn query_rank_by_logical_scope() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_rank_by_logical_scope",
        f.run(&[
            "query",
            "--mode",
            "rank",
            "-l",
            "core.lang",
            "--show-scores"
        ])
    );
}

#[test]
fn query_rank_limited() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_rank_limited",
        f.run(&["query", "--mode", "rank", "-p", "src/main.rs", "-n", "1"])
    );
}

#[test]
fn query_rank_min_criticality() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_rank_min_criticality",
        f.run(&[
            "query",
            "--mode",
            "rank",
            "-p",
            "src/main.rs",
            "--min-criticality",
            "0.8"
        ])
    );
}

#[test]
fn query_rank_on_empty_store() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "query_rank_on_empty_store",
        f.run(&["query", "--mode", "rank", "-p", "src/main.rs"])
    );
}

// =====================================================================
// mode validation
// =====================================================================

/// `--mode` is a plain string validated inside `run`, not a clap `ValueEnum`,
/// so an invalid value is an exit-1 anyhow error rather than clap's exit 2.
#[test]
fn query_invalid_mode_fails() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_invalid_mode_fails",
        f.run(&["query", "--mode", "sideways", "--query", "rust"])
    );
}

#[test]
fn query_missing_mode_fails() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_missing_mode_fails",
        f.run(&["query", "--query", "rust"])
    );
}
