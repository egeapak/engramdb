//! `add -e` and `update -e` — the two flows that hand a file to `$EDITOR`.
//!
//! # Why these are tier-2 cases
//!
//! Nothing about an editor needs a terminal. Both flows do the same four
//! things: put a file somewhere (`add` writes a template into
//! `std::env::temp_dir()`, `update` locates the memory's own markdown),
//! `shell_words::split($EDITOR)`, spawn it with the file path appended as the
//! final argument, and fail if it exits non-zero. A `#!/bin/sh` script that
//! rewrites `$1` is therefore a complete stand-in for a person editing and
//! saving, and the whole flow — template contents, parse failures, the
//! non-zero-exit bail, the `shell_words` errors — is reachable from the
//! binary. Only the *interactive* `add -i` prompter genuinely needs a TTY.
//!
//! Two things are load-bearing about the fake editors below.
//!
//! **They are explicit scripts, never `true`/`false`.** `Fixture::base`
//! rewrites `PATH` to drop any directory holding an `engramdb`, so what is
//! left on it is not something to depend on; and `EDITOR=/nonexistent/editor`
//! is only a *launch* failure because the path is guaranteed absent.
//!
//! **They print to stderr, not stdout.** The child inherits the CLI's stdio,
//! so anything an editor writes lands in the same captured streams — which is
//! exactly how [`add_editor_flags_prefill_template`] proves the flags reached
//! the template, by `cat`-ing the file it was handed. Stdout is reserved: it
//! resolves to JSON under a pipe, and interleaved script output would stop the
//! transcript renderer from parsing it.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::Fixture;

/// An editor that overwrites the file it is given with `payload`.
///
/// The heredoc is quoted, so the payload reaches the file byte for byte with
/// no shell expansion of `$`, backticks or backslashes.
fn saving_editor(f: &Fixture, name: &str, payload: &str) -> PathBuf {
    f.fake_editor(
        name,
        &format!("cat > \"$1\" <<'ENGRAMDB_PAYLOAD'\n{payload}\nENGRAMDB_PAYLOAD\n"),
    )
}

/// An editor that reports the file it was handed, then rewrites one line of
/// it in place.
///
/// A line *substitution*, not an append: a memory file ends with an HTML
/// comment carrying `visibility`/`accessed_at`/`decay`, so text appended to
/// the file lands outside `## Content` and the body is unchanged — an "edit"
/// that proves nothing. Reporting `$1` is what pins the path being appended
/// as the editor's final argument, and it is the only thing that puts either
/// per-run path through the redactor.
fn substituting_editor(f: &Fixture, name: &str, from: &str, to: &str) -> PathBuf {
    f.fake_editor(
        name,
        &format!(
            "printf 'editing %s\\n' \"$1\" >&2\n\
             sed 's/{from}/{to}/' \"$1\" > \"$1.edited\"\n\
             mv \"$1.edited\" \"$1\"\n"
        ),
    )
}

/// The script path as `EDITOR` would carry it.
fn editor_arg(path: &Path) -> String {
    path.to_str().unwrap().to_string()
}

/// The id of the one memory in the fixture store.
///
/// Deliberately *one*: `Fixture::seed` adds three, and which one a test then
/// edits would depend on directory order — the ids are redacted in the
/// snapshot, but the memory's contents are not, so the transcript would flake.
fn only_memory_id(f: &Fixture) -> String {
    static UUID: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap()
    });

    let dir = f.path().join(".engramdb").join("memories");
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .expect("memories directory exists")
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|p| {
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            UUID.find(&stem)
                .unwrap_or_else(|| panic!("no id in memory filename {stem}"))
                .as_str()
                .to_string()
        })
        .collect();
    assert_eq!(ids.len(), 1, "expected exactly one memory, got {ids:?}");
    ids.pop().unwrap()
}

/// Add the single memory the `update -e` cases edit.
fn add_one(f: &Fixture) -> String {
    f.run(&[
        "add",
        "-t",
        "convention",
        "-s",
        "Prefer explicit imports",
        "-c",
        "Glob imports hide where a name came from.",
    ]);
    only_memory_id(f)
}

// =====================================================================
// add -e
// =====================================================================

/// The whole happy path: template written, editor fills it in, memory created.
#[test]
fn add_editor_saves_a_filled_in_template() {
    let f = Fixture::new();
    f.init();
    let editor = saving_editor(
        &f,
        "fill-template.sh",
        "\
# Type: hazard
# Summary: Reindex rebuilds every row from the markdown files
# Title: reindex rebuilds rows
# Tags: index, reindex
# Physical: src/storage/lance_index.rs
# Logical: storage.index
# Criticality: 0.8
# Visibility: shared

Anything written straight to the table and not to a file is lost.",
    );
    insta::assert_snapshot!(
        "add_editor_success",
        f.run_with_editor(&["add", "-e"], &editor_arg(&editor))
    );
}

/// The flags reach the template.
///
/// The editor saves the file unchanged and `cat`s it to stderr, so the
/// snapshot holds the template as `add` generated it — every flag's value in
/// its field, the untouched `# Title:` hint, and the `0.7` criticality default
/// for the one field left unset. That the memory is still created from it also
/// pins the parser's own defaulting: the parenthesised hint is stripped rather
/// than becoming the title.
///
/// The reported `$1` is `add`'s template file. It is written to
/// `std::env::temp_dir()` under a per-run UUID name, outside every fixture
/// directory, and `[ADD_TEMPLATE]` in `normalize` is what keeps it out of the
/// snapshot.
#[test]
fn add_editor_flags_prefill_template() {
    let f = Fixture::new();
    f.init();
    let editor = f.fake_editor(
        "show-template.sh",
        "printf 'editing %s\\n' \"$1\" >&2\ncat \"$1\" >&2\n",
    );
    insta::assert_snapshot!(
        "add_editor_flags_prefill_template",
        f.run_with_editor(
            &[
                "add",
                "-e",
                "-t",
                "decision",
                "-s",
                "Keep the module graph a DAG",
                "-c",
                "Lower layers must never use a higher one.",
                "--tags",
                "architecture,layering",
                "-p",
                "src/ops/mod.rs",
                "-l",
                "architecture.layers",
                "--visibility",
                "personal",
            ],
            &editor_arg(&editor)
        )
    );
}

/// A saved template missing a required field fails the parse, after the editor
/// has already run — so this is the one error whose message comes from
/// `parse_editor_template` rather than from the spawn.
#[test]
fn add_editor_missing_required_field_fails() {
    let f = Fixture::new();
    f.init();
    let editor = saving_editor(
        &f,
        "drop-summary.sh",
        "\
# Type: convention
# Title: no summary at all

The summary line was deleted rather than filled in.",
    );
    insta::assert_snapshot!(
        "add_editor_missing_required_field",
        f.run_with_editor(&["add", "-e"], &editor_arg(&editor))
    );
}

/// Quitting the editor with a failure abandons the add.
#[test]
fn add_editor_nonzero_exit_fails() {
    let f = Fixture::new();
    f.init();
    let editor = f.fake_editor("refuse.sh", "exit 1\n");
    insta::assert_snapshot!(
        "add_editor_nonzero_exit",
        f.run_with_editor(&["add", "-e"], &editor_arg(&editor))
    );
}

/// `EDITOR=""` splits to zero words — distinct from `EDITOR` being *unset*,
/// which falls back to `vi`.
#[test]
fn add_editor_empty_env_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_editor_empty_env",
        f.run_with_editor(&["add", "-e"], "")
    );
}

/// An unbalanced quote fails in `shell_words::split`, before any spawn.
#[test]
fn add_editor_unparseable_env_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_editor_unparseable_env",
        f.run_with_editor(&["add", "-e"], "'")
    );
}

/// A well-formed `EDITOR` naming nothing executable fails at the spawn, and
/// the message names the command it tried.
#[test]
fn add_editor_launch_failure() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_editor_launch_failure",
        f.run_with_editor(&["add", "-e"], "/nonexistent/editor")
    );
}

// =====================================================================
// update -e
// =====================================================================

/// `update -e` with no other flag: the memory's own file is opened, the edit
/// lands, and the command returns without going near the store.
///
/// Two things beyond the success message. The reported `$1` shows *which*
/// file `memory_path` resolved — the shared-memories markdown, named for the
/// id. And the follow-up `get` shows the edited body, because a read goes to
/// the file rather than to the index; the early return skips `update_memory`,
/// so the LanceDB row still holds the pre-edit text and only a `reindex`
/// would reconcile them.
///
/// **The `get` now says so out loud**, and that warning line is the point of
/// this snapshot. `-e` deliberately edits the file without touching the store
/// — it loads no embedding model, which is what makes it quick — so the row
/// goes stale by design. Before the content digest existed, nothing could see
/// that: the counts still matched and the id set was unchanged, so the user
/// was told nothing and served the pre-edit text on every semantic query until
/// they happened to reindex. The `size`-tier staleness check is what turns
/// that silent desync into an instruction.
#[test]
fn update_editor_success() {
    let f = Fixture::new();
    f.init();
    let id = add_one(&f);
    let editor = substituting_editor(
        &f,
        "widen-content.sh",
        "^Glob imports hide where a name came from\\.$",
        "A glob import silently shadows a local name.",
    );
    insta::assert_snapshot!(
        "update_editor_success",
        f.run_with_editor(&["update", &id, "-e"], &editor_arg(&editor))
    );
    insta::assert_snapshot!("update_editor_success__get", f.run(&["get", &id]));
}

/// An unknown id fails while *locating* the file — before `$EDITOR` is read at
/// all, which is why the editor here is one that would have failed loudly.
#[test]
fn update_editor_unknown_id_fails() {
    let f = Fixture::new();
    f.init();
    let editor = f.fake_editor("must-not-run.sh", "echo 'editor ran' >&2\nexit 3\n");
    insta::assert_snapshot!(
        "update_editor_unknown_id",
        f.run_with_editor(&["update", "no-such-memory", "-e"], &editor_arg(&editor))
    );
}

#[test]
fn update_editor_nonzero_exit_fails() {
    let f = Fixture::new();
    f.init();
    let id = add_one(&f);
    let editor = f.fake_editor("refuse.sh", "exit 1\n");
    insta::assert_snapshot!(
        "update_editor_nonzero_exit",
        f.run_with_editor(&["update", &id, "-e"], &editor_arg(&editor))
    );
}

/// `-e` combined with another flag does **not** return early.
///
/// The early return in `run_update` is guarded on *every* other update field
/// being absent, so a single `--tags` keeps the flow going. The proof is in
/// the transcript: after the "Edited memory" success the command carries on
/// into the store, where re-embedding fails against an unreachable Ollama and
/// takes the exit code to 1 — the same ending every other tier-2 `update` case
/// has, and an ending unreachable from the early-return branch. The follow-up
/// `get` then shows both halves landing: the editor's line *and* the new tag.
#[test]
fn update_editor_with_tags_continues_to_update() {
    let f = Fixture::new();
    f.init();
    let id = add_one(&f);
    let editor = substituting_editor(
        &f,
        "widen-content.sh",
        "^Glob imports hide where a name came from\\.$",
        "Edited before the tag update.",
    );
    insta::assert_snapshot!(
        "update_editor_with_tags",
        f.run_with_editor(
            &["update", &id, "-e", "--tags", "imports"],
            &editor_arg(&editor)
        )
    );
    insta::assert_snapshot!("update_editor_with_tags__get", f.run(&["get", &id]));
}
