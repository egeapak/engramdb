//! Shared harness for the **command-tier** snapshot tests.
//!
//! # Why a third tier
//!
//! The two existing tiers cannot reach the prompt-driven commands:
//!
//! - **Tier 1** (`output.rs::tests`) drives `OutputFormatter::print_*` from
//!   literals. It has no store and no prompter, so it can render a memory but
//!   not *run* `add -i`.
//! - **Tier 2** (`tests/cli/snapshot/`) spawns the real binary. `inquire`
//!   needs a terminal, so every interactive flow either fails immediately or
//!   hangs; and `MockPrompter` lives behind `#[cfg(test)]` in this lib, which
//!   an integration-test crate cannot see.
//!
//! This tier sits between them: a real `MemoryStore` in a temp dir, a scripted
//! [`MockPrompter`], and a capturing [`OutputFormatter`] — so a whole
//! interaction becomes one snapshot.
//!
//! # What a snapshot holds
//!
//! ```text
//! --- prompts ---
//! ? Memory type: [decision, convention, …] → hazard
//! ? Summary (required): → Blocking calls deadlock the daemon
//! --- stdout ---
//! ✓ Created memory [ID]
//! --- stderr ---
//! ```
//!
//! The prompts section comes from [`MockPrompter::transcript`] and is the part
//! neither other tier can produce: prompt text reaches the terminal through
//! `inquire`, never through the formatter.
//!
//! # Redaction
//!
//! Unlike tier 1 (which needs none) this tier runs against a real store, so
//! ids are generated and paths are temporary. The filter set is deliberately
//! far smaller than tier 2's — the fixture controls everything except ids and
//! its own directory. Tier 2's `Fixture::normalize` cannot be shared: it lives
//! in an integration-test crate, where `#[cfg(test)]` items in this lib are
//! invisible.

use std::sync::LazyLock;

use engramdb::storage::{InMemoryRegistry, MemoryStore, RegistryBackend};
use regex::Regex;
use tempfile::TempDir;

use crate::app::OutputFormat;
use crate::output::{Capture, OutputFormatter};
use crate::prompter::MockPrompter;

/// A temp directory, its registry, and (optionally) an initialised store.
///
/// Commands in this tier take a `dir` and either open the store themselves
/// (`run_review`) or receive one (`run_interactive_mode`), and some take only
/// the registry (`run_groups`). So this exposes the pieces rather than one
/// pre-wired object.
pub(crate) struct TempProject {
    dir: TempDir,
    pub(crate) registry: InMemoryRegistry,
}

impl TempProject {
    pub(crate) fn new() -> Self {
        Self {
            dir: TempDir::new().expect("temp dir"),
            registry: InMemoryRegistry::new(),
        }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// Initialise a store in this project and return it.
    pub(crate) async fn init_store(&self) -> MemoryStore {
        MemoryStore::init(self.dir.path(), &self.registry)
            .await
            .expect("init store")
    }

    /// Register this directory as a project, returning its derived id.
    ///
    /// The id is a SHA-256 of the path, so it differs per run — which is what
    /// [`normalize`] redacts.
    pub(crate) async fn register(&self) -> String {
        let pid = engramdb::storage::project_id::compute_project_id(self.dir.path());
        self.registry.update(self.dir.path(), &pid).await.unwrap();
        pid
    }
}

/// A capturing formatter in `Plain`.
///
/// `Plain` rather than `Pretty` because these snapshots are about *what* an
/// interaction says, not how it is styled — colour is tier 1's, and `Pretty`
/// would only add markup to every line. Not the `None` default either:
/// `OutputFormatter::new(None, …)` resolves to `Json` without a TTY, which
/// sends the confirmation-guarded commands down their non-interactive bail
/// path instead of through the prompt.
pub(crate) fn capturing_plain() -> (OutputFormatter, Capture) {
    OutputFormatter::capturing(OutputFormat::Plain)
}

/// A capturing formatter in `Json`, for the "refuses to prompt" cases.
pub(crate) fn capturing_json() -> (OutputFormatter, Capture) {
    OutputFormatter::capturing(OutputFormat::Json)
}

/// Join the dialogue and the captured streams into one snapshot body.
pub(crate) fn interaction(prompter: &MockPrompter, cap: &Capture) -> String {
    format!(
        "--- prompts ---\n{}{}",
        prompter.transcript(),
        cap.transcript()
    )
}

/// Rewrite the few genuinely per-run values.
///
/// **No `\b` around an id pattern.** Ids appear inside filenames
/// (`one-memory_019fd0b6-…`) and `_` is a word character, so `\b` silently
/// skips them — a trap tier 2 already fell into once. Dashed ids match
/// unanchored; bare-hex ids capture a non-hex delimiter on each side and put
/// it back.
static FILTERS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        // Full UUID (memory ids), then the 8- and 13-char prefixes the
        // renderers truncate to. Longest first so a prefix rule cannot bite a
        // full id in half.
        (
            Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap(),
            "[ID]",
        ),
        (
            Regex::new(r"(^|[^0-9a-f-])[0-9a-f]{8}-[0-9a-f]{4}([^0-9a-f-]|$)").unwrap(),
            "${1}[ID13]${2}",
        ),
        // `compute_project_id` is 16 hex chars; `short_id` truncates it to 13.
        (
            Regex::new(r"(^|[^0-9a-f])[0-9a-f]{16}([^0-9a-f]|$)").unwrap(),
            "${1}[PID]${2}",
        ),
        (
            Regex::new(r"(^|[^0-9a-f])[0-9a-f]{13}([^0-9a-f]|$)").unwrap(),
            "${1}[PID13]${2}",
        ),
        // `review` prints `id.chars().take(8)`. Eight hex characters is short
        // enough to occur in prose — `deadbeef` is a legal summary — so this
        // one is anchored to the line `review` actually emits instead of
        // matching a bare run. `print_memory`'s `ID: <full uuid>` is already
        // gone by here, consumed by the first rule.
        (Regex::new(r"(?m)^ID: [0-9a-f]{8}$").unwrap(), "ID: [ID8]"),
    ]
});

pub(crate) fn normalize(raw: &str, project_dir: &std::path::Path) -> String {
    // Both spellings of the temp dir. `RegistryBackend::update` stores
    // `dir.canonicalize()`, and commands like `projects delete` echo that
    // string back — while `TempProject::path()` hands out the uncanonicalised
    // handle. On Linux the two are equal and this is redundant; on macOS
    // `/tmp` resolves to `/private/tmp`, and without this the real temp path
    // would land in a snapshot.
    //
    // Longest first: the raw path is a *prefix* of the canonical one on macOS,
    // so replacing it first would leave a stray `/private` behind.
    let mut spellings = vec![project_dir.display().to_string()];
    if let Ok(canonical) = project_dir.canonicalize() {
        spellings.push(canonical.display().to_string());
    }
    spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));
    spellings.dedup();

    // The temp dir before the id rules: its path can contain hex runs they
    // would otherwise chew into placeholders.
    let mut out = raw.to_string();
    for spelling in spellings {
        out = out.replace(&spelling, "[PROJECT]");
    }
    for (re, replacement) in FILTERS.iter() {
        out = re.replace_all(&out, *replacement).into_owned();
    }
    out
}

/// Snapshot one interaction.
///
/// Snapshots land in `tests/snapshots/command/`, beside the renderer ones:
/// assertions live next to the code they cover, `.snap` files are test data.
///
/// The module prefix is switched off. insta derives it from wherever
/// `assert_snapshot!` is *called*, which is here — so every file would be
/// named `engram_cli__testutil__…` regardless of the command it covers. `name`
/// carries that instead, and must therefore be unique across the whole tier;
/// prefix it with the command (`groups_subscribe_declined`).
pub(crate) fn snap_command(name: &str, project_dir: &std::path::Path, raw: String) {
    let body = normalize(&raw, project_dir);
    insta::with_settings!({
        snapshot_path => "../tests/snapshots/command",
        prepend_module_to_snapshot => false,
    }, {
        insta::assert_snapshot!(name, body);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trap tier 2 hit: an id inside a filename has `_` on its left, and
    /// `_` is a word character, so a `\b`-anchored rule skips it silently.
    #[test]
    fn normalize_redacts_ids_embedded_in_filenames() {
        let dir = std::path::Path::new("/tmp/x");
        let out = normalize("one-memory_019fd0b6-1234-7abc-8def-0123456789ab.md", dir);
        assert_eq!(out, "one-memory_[ID].md");
    }

    #[test]
    fn normalize_replaces_the_project_dir() {
        let dir = std::path::Path::new("/tmp/.tmpAbC123");
        assert_eq!(
            normalize("Path: /tmp/.tmpAbC123/sub", dir),
            "Path: [PROJECT]/sub"
        );
    }

    /// A registry stores `dir.canonicalize()` and commands echo that back, so
    /// the canonical spelling has to be redacted too — on macOS it differs
    /// from what `TempProject::path()` returns. Both must collapse to the same
    /// placeholder, with no `/private` fragment surviving.
    #[test]
    fn normalize_replaces_both_spellings_of_the_project_dir() {
        let real = TempDir::new().unwrap();
        let dir = real.path();
        let canonical = dir.canonicalize().unwrap();
        let text = format!(
            "raw {} then canonical {}\n",
            dir.display(),
            canonical.display()
        );

        let out = normalize(&text, dir);
        assert_eq!(out, "raw [PROJECT] then canonical [PROJECT]\n");
        assert!(!out.contains("/private"), "canonical prefix leaked: {out}");
    }

    /// `short_id` output and `review`'s 8-char prefix both have to land on a
    /// placeholder, or the snapshot changes every run.
    #[test]
    fn normalize_redacts_truncated_ids() {
        let dir = std::path::Path::new("/tmp/x");
        assert_eq!(normalize("ID: 019fd0b6\n", dir), "ID: [ID8]\n");
        assert_eq!(
            normalize("id 019fd0b6-1234 rest\n", dir),
            "id [ID13] rest\n"
        );
        assert_eq!(normalize("0123456789abcdef ok\n", dir), "[PID] ok\n");
    }

    /// Ordinary prose must survive untouched. Eight hex characters is short
    /// enough to be a real word — this is why the 8-char rule is anchored to
    /// `review`'s `ID:` line rather than matching a bare run.
    #[test]
    fn normalize_leaves_prose_alone() {
        let dir = std::path::Path::new("/tmp/x");
        let prose = "Created decision: added a deadbeef cafe to the facade\n";
        assert_eq!(normalize(prose, dir), prose);
    }
}
