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

// =====================================================================
// link expansion (`--expand-links`)
// =====================================================================

/// The full id of the memory whose summary is `summary`, read off `list
/// --format json` directly (the snapshot transcript redacts ids).
fn id_of(f: &Fixture, summary: &str) -> String {
    let out = f
        .base()
        .args([
            "--no-maintenance",
            "--dir",
            f.path().to_str().unwrap(),
            "--format",
            "json",
            "list",
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .find(|m| m["summary"] == summary)
        .unwrap_or_else(|| panic!("no memory {summary:?} in {v}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// `seeded()` plus a confirmed `supports` edge from the hazard into the Rust
/// decision. The hazard shares no word with "rust", so only expansion can
/// bring it into a filter-mode answer.
fn linked() -> Fixture {
    let f = seeded();
    let rust = id_of(&f, "Use Rust for the backend");
    let hazard = id_of(&f, "Never unwrap in production paths");
    f.run(&["link", &hazard, "supports", &rust]);
    f
}

#[test]
fn query_filter_expand_links_marks_neighbour() {
    let f = linked();
    insta::assert_snapshot!(
        "query_filter_expand_links",
        f.run(&[
            "--format",
            "plain",
            "query",
            "--mode",
            "filter",
            "--query",
            "rust",
            "--show-scores",
            "--expand-links=3"
        ])
    );
}

/// A bare `--expand-links` means 5. Plain rather than JSON: a halved score
/// has a 16-digit fraction, which the project-id redaction would eat.
#[test]
fn query_filter_expand_links_bare_flag() {
    let f = linked();
    insta::assert_snapshot!(
        "query_filter_expand_links_bare",
        f.run(&[
            "--format",
            "plain",
            "query",
            "--mode",
            "filter",
            "--query",
            "rust",
            "--expand-links"
        ])
    );
}

/// The bare flag before a positional query leaves the query alone: the
/// value form needs `=` (`require_equals`), so "rust" is the query, not N.
#[test]
fn query_expand_links_before_positional_query() {
    let f = linked();
    insta::assert_snapshot!(
        "query_expand_links_before_positional_query",
        f.run(&[
            "--format",
            "plain",
            "query",
            "--mode",
            "filter",
            "--expand-links",
            "rust"
        ])
    );
}

/// Off by default: the same query without the flag is unchanged.
#[test]
fn query_filter_without_expand_links_is_unchanged() {
    let f = linked();
    insta::assert_snapshot!(
        "query_filter_without_expand_links",
        f.run(&["--format", "plain", "query", "--mode", "filter", "--query", "rust"])
    );
}

#[test]
fn query_expand_links_over_cap_fails() {
    let f = seeded();
    insta::assert_snapshot!(
        "query_expand_links_over_cap_fails",
        f.run(&[
            "query",
            "--mode",
            "filter",
            "--query",
            "rust",
            "--expand-links=21"
        ])
    );
}
