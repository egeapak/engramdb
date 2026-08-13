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
//! - `add -i` needs a TTY prompter, so only its non-TTY *error* path is here;
//!   the interactive flow belongs to the command tier (`crate::testutil`),
//!   which swaps in a scripted `MockPrompter`. (`add -e` and `update -e` *are*
//!   covered here, in `snapshot::editor` — an `$EDITOR` is just a child
//!   process, so a shell script standing in for one drives the whole flow
//!   from this tier.)
//! - The `review` interactive loop, likewise — the empty-result path is here,
//!   the loop itself is in the command tier.
//! - The `projects prune` progress bar. `indicatif` draws to the real stderr,
//!   whose `is_term()` is false under a pipe, so it renders nothing here by
//!   design. It is covered in `crate::progress`, which takes the draw target
//!   as a parameter so a test can hand it an `InMemoryTerm`.
//! - Colour, in the positive direction — that is tier 1's, under
//!   `snap_colored`. `OutputFormatter::new` checks `is_tty` itself, before
//!   owo-colors is consulted, so no environment variable can make the binary
//!   style a pipe; forcing it would take a PTY harness to re-test rendering
//!   that lives entirely in `output.rs`. The *negative* direction is covered
//!   here by construction: any escape leaking into redirected output would
//!   land in these snapshots.

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
// The `$EDITOR` cases stand a `#!/bin/sh` script up as the editor and set the
// executable bit on it, neither of which means anything on Windows. CI's
// Windows/macOS job runs `cargo check --workspace --all-features` *without*
// `--all-targets`, so this module does not compile there today; the gate keeps
// that a property of the code rather than of the CI invocation.
#[cfg(unix)]
mod editor;
mod env;
mod harvest;
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

    /// Like [`Fixture::run`], but with `EDITOR` set to `editor` — for `add -e`
    /// and `update -e`, the only commands that read it.
    ///
    /// A separate entry point rather than a change to [`Fixture::base`]: every
    /// other case in this tier depends on `EDITOR` being *absent*, and one that
    /// leaked in would silently run the developer's editor on a temp file.
    /// `editor` is passed through verbatim so the malformed values (empty, an
    /// unbalanced quote) are reachable too.
    pub fn run_with_editor(&self, args: &[&str], editor: &str) -> String {
        let mut all = vec!["--no-maintenance", "--dir", self.path().to_str().unwrap()];
        all.extend_from_slice(args);
        let mut cmd = self.base();
        cmd.env("EDITOR", editor);
        self.finish(cmd, &all, None)
    }

    /// Write an executable `#!/bin/sh` script and return its path.
    ///
    /// This is how an `$EDITOR` is faked: the real flows just spawn a command
    /// with the file path appended as the final argument, so a script that
    /// rewrites `$1` is indistinguishable from a person editing and saving.
    ///
    /// The script lands *inside the project root* on purpose — [`normalize`]
    /// already rewrites that prefix to `[PROJECT]`, so the path is redacted
    /// wherever it surfaces (`update -e` prints the editor command on success,
    /// and both flows name it in the launch-failure message) with no extra
    /// rule. Callers must not use a random file name for the same reason.
    ///
    /// `PATH` is pinned to the system directories in the prologue. The one the
    /// script would otherwise inherit is [`Fixture::base`]'s, which has had
    /// every directory containing an `engramdb` removed — on a machine with
    /// one installed in `/usr/bin`, that would take `cat` and `sed` with it.
    ///
    /// [`normalize`]: Fixture::normalize
    #[cfg(unix)]
    pub fn fake_editor(&self, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = self.root.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nPATH=/usr/bin:/bin\nexport PATH\n{body}"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Like [`Fixture::run`], but with a stub `claude` executable first on
    /// `PATH` — for `setup`, the only command that probes for the Claude CLI.
    ///
    /// A separate entry point for the same reason as
    /// [`run_with_editor`](Self::run_with_editor): every other case depends on
    /// the CLI being *absent*, and [`base`](Self::base) strips it from the
    /// inherited `PATH` so the answer never depends on the machine. The stub
    /// only has to answer `--version` with exit 0, which is the whole of the
    /// probe; `--dry-run` stops before anything would actually be installed.
    ///
    /// The directory sits inside the project root so [`normalize`] redacts it
    /// if it ever surfaces.
    ///
    /// [`normalize`]: Fixture::normalize
    #[cfg(unix)]
    pub fn run_with_claude_cli(&self, args: &[&str]) -> String {
        use std::os::unix::fs::PermissionsExt;

        let bin = self.root.join("fake-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, "#!/bin/sh\necho 'claude 1.0.0'\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&path_without_engramdb())),
        )
        .expect("PATH entries contain no separator");

        let mut all = vec!["--no-maintenance", "--dir", self.path().to_str().unwrap()];
        all.extend_from_slice(args);
        let mut cmd = self.base();
        cmd.env("PATH", path);
        self.finish(cmd, &all, None)
    }

    /// Plant a Claude Code transcript for this project, as `harvest` reads it.
    ///
    /// The layout is Claude Code's: `<claude home>/projects/<encoded cwd>/
    /// <session>.jsonl`, one JSON object per line. `claude_home` resolves to
    /// `$HOME/.claude`, and [`base`](Self::base) already redirects `HOME`, so
    /// the corpus lands inside the fixture with nothing else to override.
    /// `encode_project_dir` is the real encoder rather than a copy of it, so a
    /// change to Claude Code's naming breaks these tests instead of quietly
    /// making them search an empty directory.
    ///
    /// `session` is used verbatim as the filename stem, and `harvest` treats
    /// that stem as the session id. Real ids are uuids, but a uuid here would
    /// be rewritten to `[UUID]` by [`normalize`] — every session would read the
    /// same and the snapshots could not show which one was listed, marked or
    /// skipped. Descriptive stems keep the transcripts legible; the *shape* of
    /// an id is pinned by tier 1, which renders fixtures with real uuids in
    /// them.
    ///
    /// [`normalize`]: Fixture::normalize
    pub fn write_transcript(&self, session: &str, lines: &[String]) {
        let dir = self.home.path().join(".claude").join("projects").join(
            engramdb::storage::transcripts::encode_project_dir(
                &self
                    .root
                    .canonicalize()
                    .unwrap_or_else(|_| self.root.clone()),
            ),
        );
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{session}.jsonl")),
            format!("{}\n", lines.join("\n")),
        )
        .unwrap();
    }

    /// Run with no store flags at all — for `--help`, `--version` and
    /// `completions`, which do not touch a store and read better without a
    /// temp path in the recorded command line.
    pub fn run_bare(&self, args: &[&str]) -> String {
        self.exec(args, None)
    }

    fn exec(&self, args: &[&str], stdin: Option<&str>) -> String {
        self.finish(self.base(), args, stdin)
    }

    /// Run an already-configured command and render its transcript.
    fn finish(&self, mut cmd: assert_cmd::Command, args: &[&str], stdin: Option<&str>) -> String {
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
            render_stdout(&String::from_utf8_lossy(&out.stdout)),
            String::from_utf8_lossy(&out.stderr),
        );
        self.normalize(&transcript)
    }

    /// Place the pinned config without creating a store.
    ///
    /// For the commands that auto-create on first use: they still take the
    /// uninitialized-store path, but with model configuration pinned so the
    /// snapshot does not depend on whether this machine has an ONNX runtime.
    /// Delete the registry, leaving an initialized store nothing points at.
    ///
    /// This is what a lost or hand-cleared `registry.json` looks like, and the
    /// state that made `doctor --fix` destructive — see
    /// `doctor_fix_yes_on_unregistered_store`. Distinct from
    /// [`write_config_only`](Self::write_config_only), which is a store that
    /// was never built in the first place.
    pub fn deregister(&self) {
        let path = self.registry.path().join("registry.json");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
    }

    pub fn write_config_only(&self) {
        let dir = self.root.join(".engramdb");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), fixture_config()).unwrap();
    }

    /// Initialize the store with the pinned config template.
    pub fn init(&self) {
        let template = self.root.join("fixture-config.toml");
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

    /// The project's ID, for the commands that take one as an argument.
    ///
    /// Computed the way the store computes it rather than scraped out of a
    /// command's output, so a test that needs an id does not depend on the
    /// formatting of some other command.
    pub fn project_id(&self) -> String {
        engramdb::storage::project_id::compute_project_id(&self.root)
    }

    /// Initialize a second store at `rel`, below the project root.
    ///
    /// For `projects discover`, which is about stores that exist on disk but
    /// are not in the registry — so it needs a tree with more than one to walk.
    /// Returns the absolute path.
    pub fn init_nested(&self, rel: &str) -> PathBuf {
        let dir = self.root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        let template = self.root.join("fixture-config.toml");
        std::fs::write(&template, fixture_config()).unwrap();
        let out = self
            .base()
            .args([
                "--no-maintenance",
                "--dir",
                dir.to_str().unwrap(),
                "init",
                "--no-embeddings",
                "--template",
                template.to_str().unwrap(),
            ])
            .output()
            .expect("failed to init nested store");
        assert!(
            out.status.success(),
            "nested init failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        dir
    }

    /// Give the project a git origin, changing the ID it hashes to.
    ///
    /// `compute_project_id` prefers the origin URL over the directory path and
    /// reads `.git/config` as plain text, so this needs no git binary — and
    /// because the ID then derives from the URL rather than a temp path, it is
    /// the same on every machine.
    ///
    /// Adding a remote *after* `init` is precisely the drift `projects repair`
    /// exists for: the registry still points at the path-derived ID while the
    /// project now hashes to a different one.
    pub fn write_git_remote(&self, url: &str) {
        let git = self.root.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(
            git.join("config"),
            format!("[remote \"origin\"]\n\turl = {url}\n"),
        )
        .unwrap();
    }

    /// Add one **personal** memory.
    ///
    /// Personal memories live only under `<data>/projects/<id>/personal/`,
    /// outside the project tree, so they are the ones a mistaken sweep of the
    /// global data directory destroys for good.
    ///
    /// Asserts the add succeeded, like [`seed`](Self::seed): `run` returns a
    /// transcript rather than checking the status, so a setup command that
    /// failed — a mistyped flag exits 2 before clap ever reaches the handler —
    /// would otherwise leave an empty store and a vacuously passing test.
    pub fn seed_personal(&self, summary: &str, content: &str) {
        let out = self
            .base()
            .args([
                "--no-maintenance",
                "--dir",
                self.path().to_str().unwrap(),
                "add",
                "-t",
                "context",
                "-s",
                summary,
                "-c",
                content,
                "--visibility",
                "personal",
            ])
            .output()
            .expect("failed to seed personal memory");
        assert!(
            out.status.success(),
            "personal seed add failed: {}",
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

        s = redact_onnx_runtime_check(&s);

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
    path_without(&["engramdb", "claude"])
}

/// The inherited `PATH` with every directory holding one of `names` removed.
///
/// `engramdb` is removed so `doctor`'s "binary on PATH" check reports a fixed
/// answer. `claude` is removed for the same reason and it is *not* theoretical:
/// `setup` probes for the Claude CLI by running `claude --version`, and the
/// snapshot was recorded on a machine that had one while CI has none — so
/// `setup_dry_run_global` passed locally and failed on the runner with an
/// entirely different branch ("Claude CLI not found, falling back to
/// settings.json"). Removing it pins every fixture to the no-CLI branch, which
/// is also what a fresh install hits; `setup_dry_run_with_claude_cli` covers
/// the other branch by planting a fake `claude` rather than hoping for a real
/// one.
fn path_without(names: &[&str]) -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let kept: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|dir| {
            !names
                .iter()
                .any(|n| dir.join(n).exists() || dir.join(format!("{n}.exe")).exists())
        })
        .collect();
    std::env::join_paths(kept)
        .expect("PATH entries contain no separator")
        .to_string_lossy()
        .into_owned()
}

/// Collapse `doctor`'s ONNX Runtime check to a single stable marker.
///
/// This one check is a *report about the machine* — whether `libonnxruntime`
/// is installed and where it was loaded from — so it necessarily differs
/// between a laptop without one and CI's `test` job, which installs one. It is
/// not just the message that changes: the passing form has no `status` and no
/// `suggestion`, so a line-level substitution cannot square the two shapes.
///
/// Everything else `doctor` reports stays under test; only this row is
/// replaced. (Model *availability* needs no such treatment — `ENGRAMDB_OFFLINE`
/// plus an empty cache pins it either way.)
/// Pretty/plain form: the status line, plus the hint line that only the
/// failing form emits. (The JSON form is handled in [`render_stdout`], where
/// the document is still parseable — by the time a transcript is assembled it
/// is no longer valid JSON.)
fn redact_onnx_runtime_check(text: &str) -> String {
    ONNX_CHECK_LINES
        .replace_all(text, "[ONNX_RUNTIME_CHECK]\n")
        .into_owned()
}

/// Replace the `message`/`status`/`suggestion` of any check named
/// "ONNX Runtime", anywhere in the tree. Returns whether anything changed.
fn redact_onnx_in_json(value: &mut serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("name").and_then(|n| n.as_str()) == Some("ONNX Runtime") {
                map.insert(
                    "message".into(),
                    serde_json::Value::String("[ONNX_RUNTIME_CHECK]".into()),
                );
                map.remove("status");
                map.remove("suggestion");
                return true;
            }
            // `fold`, not `any`: `any` short-circuits, so a document with two
            // ONNX rows would keep the second one's machine-specific path.
            map.values_mut()
                .fold(false, |hit, v| redact_onnx_in_json(v) | hit)
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .fold(false, |hit, v| redact_onnx_in_json(v) | hit),
        _ => false,
    }
}

/// The check row, plus the suggestion line that only the failing form emits.
///
/// `print_hint` prefixes that suggestion differently per format — `ℹ` in
/// pretty, `Hint:` in plain — so both spellings have to be matched or the
/// plain snapshot alone stays runtime-dependent.
static ONNX_CHECK_LINES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*[○⚠✓✗] ONNX Runtime: .*\n(^[ \t]*(ℹ|Hint:) Install ONNX Runtime.*\n)?")
        .unwrap()
});

/// Prepare stdout for the transcript.
///
/// Re-renders a JSON document so snapshots diff line by line — the CLI is
/// deliberately inconsistent here, with some handlers emitting compact
/// `serde_json::json!(…)` and others `to_string_pretty`, and a compact
/// document would be one enormous snapshot line. While the value is parsed,
/// the ONNX Runtime check is redacted too; that has to happen here, because
/// once the transcript is assembled the text is no longer valid JSON.
fn render_stdout(stdout: &str) -> String {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return stdout.to_string();
    }
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        redact_onnx_in_json(&mut value);
        if let Ok(rendered) = serde_json::to_string_pretty(&value) {
            return format!("{rendered}\n");
        }
    }
    redact_json_lines(trimmed).unwrap_or_else(|| stdout.to_string())
}

/// The same redaction for stdout that is *several* JSON documents.
///
/// `doctor --fix` prints the report object and then one `{"message":…}` line
/// per proposed action, so the whole of stdout is not one parseable document
/// and the single-value branch above skips it — which let the ONNX Runtime
/// check through with the absolute path of whatever `libonnxruntime` the
/// machine loaded. That passes on the box the snapshot was accepted on and
/// fails everywhere else; CI caught it as `/tmp/onnxruntime-…` versus
/// `/usr/local/lib/…`.
///
/// Each document keeps the exact bytes it arrived with unless redaction
/// actually changed it, and a changed one is re-rendered in the shape it had
/// (multi-line stays multi-line). Reformatting wholesale would churn every
/// JSON-lines snapshot in the suite for no gain. Returns `None` if the input
/// is not a clean sequence of JSON values, leaving the caller's raw text.
fn redact_json_lines(trimmed: &str) -> Option<String> {
    let mut stream = serde_json::Deserializer::from_str(trimmed).into_iter::<serde_json::Value>();
    let mut docs: Vec<(usize, usize, serde_json::Value)> = Vec::new();
    let mut start = 0;
    // `while let`, not `for`: `byte_offset` needs the stream back between
    // items, and a `for` loop holds the mutable borrow for its whole body.
    while let Some(value) = stream.next() {
        let value = value.ok()?;
        let end = stream.byte_offset();
        docs.push((start, end, value));
        start = end;
    }
    // A trailing fragment means this was never JSON-lines; don't touch it.
    if docs.len() < 2 || trimmed[start..].trim() != "" {
        return None;
    }

    let mut out = String::new();
    for (from, to, mut value) in docs {
        let original = trimmed[from..to].trim();
        let rendered = if redact_onnx_in_json(&mut value) {
            if original.contains('\n') {
                serde_json::to_string_pretty(&value).ok()?
            } else {
                serde_json::to_string(&value).ok()?
            }
        } else {
            original.to_string()
        };
        out.push_str(&rendered);
        out.push('\n');
    }
    Some(out)
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
        // `add -e` writes its template to `std::env::temp_dir()` — *not* under
        // any fixture directory, so no `[…]` path replacement above reaches it
        // — under a per-run name, `engramdb-add-<uuid>.txt`. Must precede the
        // UUID rule, which would otherwise consume the name's variable half
        // and leave the machine's temp prefix behind.
        (
            Regex::new(r"\S*engramdb-add-[0-9a-f-]+\.txt").unwrap(),
            "[ADD_TEMPLATE]",
        ),
        (
            Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap(),
            "[UUID]",
        ),
        // `harvest show` frames the recorded transcript with a fence token
        // that is *deliberately* freshly random on every render, so recorded
        // content cannot forge the framing (`ops::harvest`). It appears three
        // times per digest — the `fence` field and the BEGIN/END lines — and
        // being 32 undashed hex characters it is caught by neither the UUID
        // rule above nor the 16-hex project-id rule below. It must precede
        // that one, which would otherwise consume the token's first half and
        // leave the second behind. Left unredacted this is a guaranteed flake:
        // the snapshot passes on the run that records it and fails on the next.
        (Regex::new(r"[0-9a-f]{32}").unwrap(), "[FENCE]"),
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
        // Why the embedding provider was unavailable is a report about the
        // machine, in the same sense as the ONNX Runtime check. The fixture
        // pins model *availability* (empty cache + `ENGRAMDB_OFFLINE`) but not
        // which backend `Auto` lands on, and the Ollama arm ends in a socket
        // error whose wording is the OS's: `os error 111` on Linux, `61` on
        // macOS, different again if a developer happens to have Ollama
        // running. The command's own framing before the colon is the contract
        // — that it names what it could not do — and it stays asserted, along
        // with the exit code and the stream.
        (
            Regex::new(r#"(cannot be (?:indexed|searched)): [^\n"]*"#).unwrap(),
            "${1}: [EMBEDDING_UNAVAILABLE]",
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
