//! Snapshot tests for the `engramdb` binary (tier 2).
//!
//! # What this tier is for
//!
//! Tier 1 (in `crates/engram-cli/src/output.rs`) snapshots the *renderers* —
//! every `OutputFormatter::print_*` method across pretty/json/plain, driven
//! in-process from fixtures with pinned ids and pinned clocks, so it needs no
//! redaction at all.
//!
//! This tier snapshots the *binary*: which flag reaches which renderer, what
//! the exit code is, which stream each message lands on, and what clap prints
//! when parsing fails. None of that is reachable in-process —
//! [`engram_cli::run`] returns `anyhow::Result<()>` and the `Error: …` text
//! plus exit 1 come from `Termination` in `main.rs`, while every `--help`,
//! bad `--format` and missing-argument failure exits 2 from `Cli::parse()`
//! without ever entering `run`.
//!
//! So the two tiers divide as: **tier 1 owns the format matrix, tier 2 owns
//! the wiring.** Most commands here are snapshotted in the default format
//! only. The exceptions are the *renderer-thin* commands — `config`, `stats`,
//! `daemon`, `review`, `doctor` — which print the bulk of their output with
//! bare `println!` rather than through `OutputFormatter`, so tier 1 cannot see
//! them and tier 2 covers all three formats instead.
//!
//! # Not covered, and why
//!
//! - `serve` and `daemon run` / `daemon restart` start a long-running process.
//!   Only their `--help` and fast error paths (bad `--transport`) are here.
//! - `add -i`, `add -e`, `update -e` need a TTY prompter or `$EDITOR`. Their
//!   non-TTY *error* paths are covered; the interactive flows are not.
//! - The `review` interactive loop, likewise — the empty-result path is here.
//! - Colour. `use_color` requires a TTY (`output.rs`), so no ANSI ever reaches
//!   a captured pipe, in these tests or in any real redirected invocation.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use tempfile::TempDir;

/// Snapshot one invocation in all three formats.
///
/// Only for the *renderer-thin* commands — `config`, `stats`, `daemon`,
/// `review`, `doctor` — which print most of their output with bare `println!`
/// instead of through `OutputFormatter`, so tier 1's format matrix does not
/// reach them. Everything else is snapshotted in the default format only;
/// duplicating a renderer that tier 1 already covers just triples the review
/// burden for no extra signal.
///
/// Defined before the `mod` declarations below so it is in scope for all of
/// them, and expanded at the call site so each snapshot is attributed to the
/// module that owns the case.
macro_rules! snap_all_formats {
    ($f:expr, $case:expr, $args:expr) => {
        for fmt in ["pretty", "json", "plain"] {
            let mut all = vec!["--format", fmt];
            all.extend_from_slice($args);
            insta::assert_snapshot!(format!("{}__{}", $case, fmt), $f.run(&all));
        }
    };
}

mod admin;
mod core;
mod env;
mod help;
mod hook;
mod projects_groups;
mod query;

/// The config every fixture store is initialized with: the real defaults with
/// four switches flipped, serialized in full.
///
/// Built from `EngramConfig::default()` rather than hand-written TOML for two
/// reasons.
///
/// A hand-written *partial* table is a trap: `EmbeddingsConfig::{provider,
/// dimensions, max_tokens}` and the `[rerank]` / `[nli]` fields carry no serde
/// default, so omitting one makes the table fail to deserialize — and
/// `load_config_or_default` swallows that and hands back defaults. The
/// override silently does nothing.
///
/// And the models have to be *configured* off, not merely absent. Whether
/// `libonnxruntime` exists differs between a developer box and CI (the `test`
/// job installs one), and a missing runtime prints a "reranker init failed"
/// warning on stderr — so snapshots taken without a runtime would not match on
/// a machine that has one. Disabling rerank/NLI, and pinning the keyword
/// titler over the default T5, makes model availability irrelevant either way.
fn fixture_config() -> String {
    let mut config = engramdb::types::EngramConfig::default();
    config.title.strategy = engramdb::types::TitleStrategy::Keyword;
    config.rerank.enabled = false;
    config.nli.enabled = false;
    config.daemon.enabled = false;
    config.maintenance.enabled = false;
    toml::to_string_pretty(&config).expect("EngramConfig is plain data")
}

/// A fully isolated store plus the environment to talk to it.
///
/// Every directory is per-test. The shared-`OnceLock` harness in
/// `tests/cli/helpers.rs` cannot be reused here: `tests/cli/output_renderers.rs`
/// documents that its shared registry pollutes across tests, which is fatal
/// when the registry contents are part of the snapshot.
pub struct Fixture {
    project: TempDir,
    /// `project`/workspace — see [`Fixture::new`].
    root: PathBuf,
    data: TempDir,
    config: TempDir,
    registry: TempDir,
    model_cache: TempDir,
    home: TempDir,
    runtime: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        let project = TempDir::new().unwrap();
        // The store lives in a fixed-name subdirectory rather than at the
        // temp root. `projects info` prints the project *name*, which is the
        // directory's basename — and `TempDir` basenames are random
        // (`.tmpMlY7Co`), so using the root directly made that output differ
        // on every run. Replacing the path handles the full path but not a
        // bare basename; giving the directory a stable name removes the
        // problem instead of papering over it.
        let root = project.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        Self {
            root,
            project,
            data: TempDir::new().unwrap(),
            config: TempDir::new().unwrap(),
            registry: TempDir::new().unwrap(),
            model_cache: TempDir::new().unwrap(),
            home: TempDir::new().unwrap(),
            runtime: TempDir::new().unwrap(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// A command with every environment override applied but no arguments.
    ///
    /// The model cache is empty and `ENGRAMDB_OFFLINE` is set, which is what
    /// pins `stats` / `doctor` / `reindex` / `query` to their deterministic
    /// "no model available" branch — the same trick `tests/cli/hook.rs` uses.
    #[allow(deprecated)]
    fn base(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::cargo_bin("engramdb").expect("binary engramdb not found");
        c.env("ENGRAMDB_DATA_DIR", self.data.path());
        c.env("ENGRAMDB_CONFIG_DIR", self.config.path());
        c.env(
            "ENGRAMDB_REGISTRY_PATH",
            self.registry.path().join("registry.json"),
        );
        c.env("ENGRAMDB_MODEL_CACHE_DIR", self.model_cache.path());
        c.env("ENGRAMDB_OFFLINE", "1");
        // Never reach for a shared daemon: the socket is a path that is never
        // bound, and the policy is forced local anyway.
        c.env("ENGRAMDB_IN_PROCESS", "1");
        c.env("ENGRAMDB_DAEMON_SOCKET", self.runtime.path().join("d.sock"));
        // `run()` installs a tracing subscriber at WARN on stderr; anything it
        // logs would land in the snapshot.
        c.env("RUST_LOG", "off");
        c.env("NO_COLOR", "1");
        // nextest exports RUST_BACKTRACE=1, and the child inherits it — so
        // every `Err` out of `main` printed an anyhow backtrace full of
        // absolute paths, a rustc commit hash and `~/.cargo/registry` lines.
        // The error *message* is the contract worth snapshotting; the frames
        // are this machine's.
        c.env("RUST_BACKTRACE", "0");
        // `dirs::home_dir()` feeds `setup`'s paths and doctor's plugin probe.
        c.env("HOME", self.home.path());
        c.env("XDG_CONFIG_HOME", self.home.path().join("config"));
        c.env("XDG_CACHE_HOME", self.home.path().join("cache"));
        c.env("XDG_DATA_HOME", self.home.path().join("data"));
        c.env("XDG_RUNTIME_DIR", self.runtime.path());
        c.env_remove("EDITOR");
        c.env_remove("VISUAL");
        // `doctor` shells out to whatever `engramdb` is on PATH and prints its
        // `--version`. Drop any directory that has one so the check renders
        // its "not on PATH" branch instead of the developer's install.
        c.env("PATH", path_without_engramdb());
        c
    }

    /// Run with the store flags prepended (`--no-maintenance --dir <project>`).
    ///
    /// `--no-maintenance` matters: `auto_maintain` runs before every
    /// non-exempt, non-hook command and logs on failure.
    pub fn run(&self, args: &[&str]) -> String {
        let mut all = vec!["--no-maintenance", "--dir", self.path().to_str().unwrap()];
        all.extend_from_slice(args);
        self.exec(&all, None)
    }

    /// Like [`Fixture::run`], but feeds `stdin` — the hook subcommands read
    /// their event JSON from it.
    pub fn run_with_stdin(&self, args: &[&str], stdin: &str) -> String {
        let mut all = vec!["--no-maintenance", "--dir", self.path().to_str().unwrap()];
        all.extend_from_slice(args);
        self.exec(&all, Some(stdin))
    }

    /// Run with no store flags at all — for `--help`, `--version` and
    /// `completions`, which do not touch a store and read better without a
    /// temp path in the recorded command line.
    pub fn run_bare(&self, args: &[&str]) -> String {
        self.exec(args, None)
    }

    fn exec(&self, args: &[&str], stdin: Option<&str>) -> String {
        let mut cmd = self.base();
        cmd.args(args);
        if let Some(input) = stdin {
            cmd.write_stdin(input.to_string());
        } else {
            // `add` reads stdin when it is not a terminal; an empty one keeps
            // that path deterministic rather than inheriting the test runner's.
            cmd.write_stdin(String::new());
        }
        let out = cmd.output().expect("failed to run engramdb");

        let exit = match out.status.code() {
            Some(code) => code.to_string(),
            None => "signal".to_string(),
        };
        let transcript = format!(
            "$ engramdb {}\nexit: {}\n--- stdout ---\n{}--- stderr ---\n{}",
            args.join(" "),
            exit,
            pretty_json(&String::from_utf8_lossy(&out.stdout)),
            String::from_utf8_lossy(&out.stderr),
        );
        self.normalize(&transcript)
    }

    /// Place the pinned config without creating a store.
    ///
    /// For the commands that auto-create on first use: they still take the
    /// uninitialized-store path, but with model configuration pinned so the
    /// snapshot does not depend on whether this machine has an ONNX runtime.
    pub fn write_config_only(&self) {
        let dir = self.project.path().join(".engramdb");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), fixture_config()).unwrap();
    }

    /// Initialize the store with the pinned config template.
    pub fn init(&self) {
        let template = self.project.path().join("fixture-config.toml");
        std::fs::write(&template, fixture_config()).unwrap();
        let out = self
            .base()
            .args([
                "--no-maintenance",
                "--dir",
                self.path().to_str().unwrap(),
                "init",
                "--no-embeddings",
                "--template",
                template.to_str().unwrap(),
            ])
            .output()
            .expect("failed to init fixture store");
        assert!(
            out.status.success(),
            "fixture init failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Add three memories with *distinct* type and criticality values.
    ///
    /// Distinctness is load-bearing: `ops::list` stable-sorts on top of an
    /// unordered LanceDB scan, so equal sort keys leave the order unspecified
    /// and the snapshot would flake.
    pub fn seed(&self) {
        for (type_, summary, content, crit) in [
            (
                "decision",
                "Use Rust for the backend",
                "Chosen for the memory-safety guarantees.",
                "0.90",
            ),
            (
                "convention",
                "Name variables in snake_case",
                "Applies to every crate in the workspace.",
                "0.60",
            ),
            (
                "hazard",
                "Never unwrap in production paths",
                "A panic in the daemon takes down every session.",
                "0.30",
            ),
        ] {
            let out = self
                .base()
                .args([
                    "--no-maintenance",
                    "--dir",
                    self.path().to_str().unwrap(),
                    "add",
                    "-t",
                    type_,
                    "-s",
                    summary,
                    "-c",
                    content,
                    "--criticality",
                    crit,
                ])
                .output()
                .expect("failed to seed memory");
            assert!(
                out.status.success(),
                "seed add failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// Replace everything machine-specific with a stable placeholder.
    ///
    /// Exact paths go first and longest-first, so that a nested directory is
    /// not partly rewritten by its parent's placeholder.
    fn normalize(&self, text: &str) -> String {
        let mut s = text.to_string();

        // macOS hands out `/var/folders/...` temp dirs but canonicalizes them
        // to `/private/var/...`, so the same directory appears both ways.
        s = s.replace("/private/var/", "/var/");

        let mut paths: Vec<(String, &str)> = vec![
            (display(&self.root), "[PROJECT]"),
            // The enclosing temp dir, for anything that reports the parent
            // rather than the store root. Sorted after `[PROJECT]` by length
            // below, so the more specific path always wins.
            (display(self.project.path()), "[PROJECT_PARENT]"),
            (display(self.data.path()), "[DATA]"),
            (display(self.config.path()), "[CONFIG]"),
            (display(self.registry.path()), "[REGISTRY]"),
            (display(self.model_cache.path()), "[CACHE]"),
            (display(self.home.path()), "[HOME]"),
            (display(self.runtime.path()), "[RUNTIME]"),
        ];
        paths.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        for (path, placeholder) in paths {
            s = s.replace(&path, placeholder);
        }

        for (re, placeholder) in FILTERS.iter() {
            s = re.replace_all(&s, *placeholder).into_owned();
        }
        s
    }
}

fn display(p: &Path) -> String {
    p.to_string_lossy().replace("/private/var/", "/var/")
}

/// The inherited `PATH` with every directory that contains an `engramdb`
/// executable removed.
fn path_without_engramdb() -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let kept: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|dir| !dir.join("engramdb").exists() && !dir.join("engramdb.exe").exists())
        .collect();
    std::env::join_paths(kept)
        .expect("PATH entries contain no separator")
        .to_string_lossy()
        .into_owned()
}

/// Re-render a JSON document so snapshots diff line by line.
///
/// The CLI is inconsistent on purpose-built output: some handlers emit compact
/// `serde_json::json!(…)` and others `to_string_pretty`. A compact document
/// would otherwise be one enormous snapshot line.
fn pretty_json(stdout: &str) -> String {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return stdout.to_string();
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Ok(rendered) = serde_json::to_string_pretty(&value) {
            return format!("{rendered}\n");
        }
    }
    stdout.to_string()
}

/// Compiled once per test process rather than per assertion.
///
/// Two things about the patterns are load-bearing.
///
/// **Order.** The full-UUID rule runs before the truncated-id rule, or
/// `short_id`'s 13-character prefix would eat the head of a full UUID; and the
/// 16-hex project-id rule runs before the 13-hex one for the same reason.
///
/// **Boundaries.** The dashed ids deliberately carry *no* `\b`. An id is often
/// embedded in a filename — `one-memory_019fd0b6-ae1c-72c2-….md` — and `_` is
/// a word character, so there is no word boundary between the slug and the
/// id, and a `\b`-anchored pattern silently skips it. (This is not
/// hypothetical: `get --path` flaked on exactly that.) They are safe unanchored
/// because the dash layout cannot occur inside a longer hex run. The bare-hex
/// rules *do* need boundaries, so they capture a non-hex delimiter on each side
/// and put it back, `\b` being unusable here for the same reason.
#[allow(clippy::type_complexity)]
static FILTERS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap(),
            "[UUID]",
        ),
        // `short_id` truncates a UUID to 13 chars: `0198f2a1-9e4b`.
        (
            Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}").unwrap(),
            "[SHORT_ID]",
        ),
        // Project ids are 16 hex chars of a SHA-256 over the canonical path,
        // so they change with the temp dir. Group ids (`__g_` + 12 hex) are
        // derived from the group name and are deliberately left alone.
        (
            Regex::new(r"(^|[^0-9a-f])[0-9a-f]{16}([^0-9a-f]|$)").unwrap(),
            "${1}[PROJECT_ID]${2}",
        ),
        // `projects list` renders short_id(project_id) — 13 bare hex chars
        // with no dash, which neither rule above catches.
        (
            Regex::new(r"(^|[^0-9a-f])[0-9a-f]{13}([^0-9a-f]|$)").unwrap(),
            "${1}[SHORT_PROJECT_ID]${2}",
        ),
        // Without a runtime installed, `ort`'s loader reports every path it
        // tried — and the list starts with the *executable's own directory*,
        // which under nextest is this machine's `target/debug/deps`.
        (
            Regex::new(r"Searched: [^\n]*libonnxruntime\.so").unwrap(),
            "Searched: [ORT_SEARCH_PATHS]",
        ),
        (
            Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?")
                .unwrap(),
            "[TIMESTAMP]",
        ),
        (Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap(), "[DATE]"),
        (
            Regex::new(r"\b\d+(?:\.\d+)? (?:B|KB|MB|GB|TB)\b").unwrap(),
            "[SIZE]",
        ),
        (Regex::new(r"\b\d+(?:\.\d+)?ms\b").unwrap(), "[MS]"),
        (
            Regex::new(r"\b\d+ (?:second|minute|hour|day|week|month|year)s? ago\b").unwrap(),
            "[AGO]",
        ),
        (
            Regex::new(r"\bengramdb \d+\.\d+\.\d+\b").unwrap(),
            "engramdb [VERSION]",
        ),
        (Regex::new(r"\bpid \d+").unwrap(), "pid [PID]"),
        (Regex::new(r"\buptime \d+s").unwrap(), "uptime [UPTIME]"),
    ]
});

#[test]
fn smoke_list_on_empty_store() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!("smoke_list_empty", f.run(&["list"]));
}

#[test]
fn smoke_is_deterministic_across_fixtures() {
    // Two independent fixtures differ in every temp path and project id. If
    // the normalizer misses one, these two transcripts will not match — which
    // is the property every other snapshot in this tier depends on.
    let a = Fixture::new();
    a.init();
    let b = Fixture::new();
    b.init();
    assert_eq!(a.run(&["list"]), b.run(&["list"]));
    assert_eq!(a.run(&["stats"]), b.run(&["stats"]));
}
