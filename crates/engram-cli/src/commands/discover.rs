//! Handler for `engramdb projects discover`.
//!
//! Walks a directory tree for `.engramdb/` projects missing from the registry
//! (see [`engramdb::ops::discover`]), asks whether to adopt each one, and — on
//! accept — registers it and rebuilds its index with an indicatif progress bar.

use crate::output::OutputFormatter;
use crate::prompter::Prompter;
use anyhow::{bail, Result};
use engramdb::daemon::{DaemonCell, DaemonPolicy, InProcessFallback};
use engramdb::ops::discover::{
    DiscoverOptions, DiscoveredProject, DiscoveryReport, DiscoveryStatus,
};
use engramdb::ops::{self};
use engramdb::retrieval::engine::RetrievalEngine;
use engramdb::storage::{MemoryStore, RegistryBackend};
use engramdb::types::EmbeddingBackend;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Parsed `projects discover` arguments.
pub struct DiscoverParams {
    /// Directory to scan.
    pub root: PathBuf,
    /// Maximum depth to descend below `root`.
    pub max_depth: usize,
    /// Descend into dot-directories.
    pub hidden: bool,
    /// Follow directory symlinks.
    pub follow_symlinks: bool,
    /// Register everything found without prompting.
    pub yes: bool,
    /// Report only; never mutate.
    pub dry_run: bool,
    /// Register without rebuilding the index.
    pub no_index: bool,
}

/// Outcome of adopting one project, for the JSON document.
struct Adopted {
    path: PathBuf,
    project_id: String,
    /// `None` when `--no-index` skipped the rebuild — distinct from `Some(0)`,
    /// which means the rebuild ran and found nothing.
    indexed: Option<usize>,
    embedded: Option<usize>,
    /// Non-fatal conditions from the reindex — chiefly "no embedding provider,
    /// so nothing was embedded". Surfaced per project: silently registering a
    /// project whose memories got no vectors would look like a full success.
    warnings: Vec<String>,
}

/// Run `engramdb projects discover`.
#[allow(clippy::too_many_arguments)]
pub async fn run_discover(
    params: DiscoverParams,
    registry: &dyn RegistryBackend,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
    embedding_backend: Option<EmbeddingBackend>,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<()> {
    let json_mode = formatter.is_json();

    let opts = DiscoverOptions {
        max_depth: params.max_depth,
        include_hidden: params.hidden,
        follow_symlinks: params.follow_symlinks,
        ..Default::default()
    };

    // Scanning a home directory takes seconds; a spinner is the only signal
    // that anything is happening. Hidden in JSON mode — machine callers get no
    // chatter, exactly as prune's bars are suppressed.
    let scan_pb = if json_mode {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} scanning {wide_msg}")
                .unwrap(),
        );
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    };
    let report = ops::discover::discover_projects(&params.root, registry, &opts, |dir| {
        scan_pb.set_message(dir.display().to_string());
    })
    .await;
    scan_pb.finish_and_clear();
    let report = report?;

    // JSON is machine-consumed: never prompt (mirrors `projects prune`).
    // Checked before any early return so the contract is a property of the
    // flags alone — a script must not succeed or fail depending on whether the
    // tree happened to contain a candidate.
    if !params.yes && !params.dry_run && json_mode {
        bail!(
            "projects discover requires confirmation; re-run with --yes or --dry-run in JSON mode"
        );
    }

    let candidates: Vec<DiscoveredProject> = report.unregistered().cloned().collect();

    if !json_mode {
        print_scan_summary(formatter, &params.root, &report, &candidates);
    }

    if params.dry_run {
        if json_mode {
            println!("{}", scan_json(&params.root, &report, &candidates));
        } else {
            formatter.print_message("Dry run: nothing was registered.");
        }
        return Ok(());
    }

    if candidates.is_empty() {
        if json_mode {
            // The action shape with everything empty — a run that registers
            // nothing must still parse like a run that registers something.
            println!("{}", action_json(&params, &report, &[], &[], &[]));
        }
        return Ok(());
    }

    // Ask per project — adopting a project is a per-project decision (one may
    // be a scratch clone, the next a real checkout), so a single all-or-nothing
    // prompt would force the user to re-run with a narrower root.
    let mut accepted: Vec<DiscoveredProject> = Vec::new();
    let mut declined: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if params.yes {
            accepted.push(candidate);
            continue;
        }
        let question = if params.no_index {
            format!(
                "Register {} ({})?",
                candidate.path.display(),
                plural_memories(candidate.memory_count)
            )
        } else {
            format!(
                "Register and index {} ({})?",
                candidate.path.display(),
                plural_memories(candidate.memory_count)
            )
        };
        // Propagated, not swallowed: this prompt defaults to *yes*, so turning
        // a prompt failure (no TTY, EOF on stdin) into `false` would silently
        // decline every project and report "Nothing registered" as success —
        // indistinguishable from the user having declined. `doctor`'s
        // confirm, the other `true`-defaulted one, propagates for this reason.
        if prompter.confirm(&question, true)? {
            accepted.push(candidate);
        } else {
            declined.push(candidate.path);
        }
    }

    if accepted.is_empty() {
        if json_mode {
            println!("{}", action_json(&params, &report, &[], &declined, &[]));
        } else {
            formatter.print_message("Nothing registered.");
        }
        return Ok(());
    }

    // One cache shared across every project: the in-process fallback would
    // otherwise reload the embedding model (~240ms + hundreds of MB) per
    // project. With a live daemon this is unused — providers come over the
    // socket — but the CLI's default policy is ConnectOnly, so the no-daemon
    // path is the common one here.
    let cache = ops::ProviderCache::new();

    let pb = if json_mode {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(accepted.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{prefix} [{bar:40.green/dim}] {pos}/{len} ({eta}) {wide_msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb.set_prefix(if params.no_index { "register" } else { "index" });
        // Model loading writes plain `eprintln!` warnings (unknown provider,
        // unavailable reranker/T5) from deep inside provider resolution, and
        // indicatif draws to stderr too. A steady tick makes the bar redraw
        // itself after such a write instead of leaving a mangled row behind.
        pb.enable_steady_tick(Duration::from_millis(120));
        pb
    };

    let mut adopted: Vec<Adopted> = Vec::new();
    let mut errors: Vec<(PathBuf, String)> = Vec::new();
    for candidate in &accepted {
        pb.set_message(short_name(&candidate.path));
        match adopt(
            candidate,
            registry,
            params.no_index,
            embedding_backend,
            cell,
            policy,
            &cache,
        )
        .await
        {
            Ok(entry) => adopted.push(entry),
            // One unreadable/corrupt project must not abandon the rest of the
            // batch — collect and report at the end.
            Err(e) => errors.push((candidate.path.clone(), e.to_string())),
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    if json_mode {
        println!(
            "{}",
            action_json(&params, &report, &adopted, &declined, &errors)
        );
        return fail_if_any_errors(&errors);
    }

    for entry in &adopted {
        match (entry.indexed, entry.embedded) {
            (Some(indexed), Some(embedded)) => formatter.print_success(&format!(
                "Registered {} ({}) — {} indexed, {} embedded",
                entry.path.display(),
                entry.project_id,
                indexed,
                embedded
            )),
            _ => formatter.print_success(&format!(
                "Registered {} ({})",
                entry.path.display(),
                entry.project_id
            )),
        }
        for warning in &entry.warnings {
            formatter.print_warning(warning);
        }
    }
    for (path, err) in &errors {
        formatter.print_error(&format!("Failed to register {}: {}", path.display(), err));
    }
    if params.no_index && !adopted.is_empty() {
        formatter.print_hint("Memories are not searchable until you run `engramdb reindex`.");
    }

    fail_if_any_errors(&errors)
}

/// Exit non-zero when any project failed to be adopted.
///
/// The full report (human lines or the JSON document) is emitted first — a
/// caller needs to see *which* ones failed — but the process must not exit 0
/// after failing to do what it was asked. A provisioning script under `set -e`
/// would otherwise proceed believing every project was adopted. `projects
/// prune`, the sibling command, propagates its errors the same way.
fn fail_if_any_errors(errors: &[(PathBuf, String)]) -> Result<()> {
    if errors.is_empty() {
        return Ok(());
    }
    bail!(
        "{} project(s) could not be registered (see the reported errors)",
        errors.len()
    )
}

/// Register one discovered project and (unless `no_index`) rebuild its index.
///
/// Registration is `MemoryStore::init`, which is idempotent and non-destructive:
/// it creates only the pieces that are missing (never overwriting an existing
/// `manifest.toml` / `config.toml`), writes the registry entry, and migrates
/// the store's schema if it predates the current one.
async fn adopt(
    candidate: &DiscoveredProject,
    registry: &dyn RegistryBackend,
    no_index: bool,
    embedding_backend: Option<EmbeddingBackend>,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
    cache: &ops::ProviderCache,
) -> Result<Adopted> {
    let store = MemoryStore::init(&candidate.path, registry).await?;
    let project_id = store.project_id.clone();

    if no_index {
        return Ok(Adopted {
            path: candidate.path.clone(),
            project_id,
            indexed: None,
            embedded: None,
            warnings: Vec::new(),
        });
    }

    // A rediscovered project usually has no index on this machine at all
    // (registry.json and the LanceDB dir are both machine-local), so this is a
    // full rebuild from the `.md` files, plus embeddings when a provider
    // resolves. Without a provider `reindex` still rebuilds the metadata table
    // and reports a warning rather than failing.
    let engine = engine_for_project(store.clone(), embedding_backend, cell, policy, cache).await;
    let result = ops::reindex(&store, Some(&engine), false).await?;
    // `ReindexResult::errors` is populated only by the embedding loop
    // (`ops::reindex`), so these memories ARE in the metadata index — they just
    // have no vector, and semantic search will miss them. Saying "could not be
    // indexed" would contradict the `indexed` count printed beside it.
    let mut warnings = result.warnings;
    if !result.errors.is_empty() {
        warnings.push(format!(
            "{} memory(ies) failed to embed and will be missed by semantic search",
            result.errors.len()
        ));
    }
    Ok(Adopted {
        path: candidate.path.clone(),
        project_id,
        indexed: Some(result.indexed),
        embedded: Some(result.embedded),
        warnings,
    })
}

/// Like [`crate::engine::engine_for`], but serving in-process providers from a
/// shared cache so a multi-project run loads each model at most once.
async fn engine_for_project(
    store: MemoryStore,
    backend: Option<EmbeddingBackend>,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
    cache: &ops::ProviderCache,
) -> RetrievalEngine {
    let config_path = store.project_dir.join(".engramdb").join("config.toml");
    let mut config = engramdb::storage::config::load_config_or_default(&config_path).await;
    // `ProviderCache` is the MCP server's seam and auto-sizes the embedding
    // pool to `cores/2` for many concurrent callers. Here it is used purely as
    // a cache across projects — both this loop and `ops::reindex`'s embed loop
    // are strictly sequential — so the extra sessions could never be used and
    // would cost a full model load each. Pin to one; the size is part of
    // `provider_cache_key`, so this stays a coherent cache key.
    config.embeddings.pool_size = Some(1);
    let project_dir = store.project_dir.clone();
    let providers = engramdb::daemon::resolve_providers_with(
        cell,
        &config,
        backend,
        &project_dir,
        policy,
        InProcessFallback::Pool(cache),
    )
    .await;
    ops::assemble_engine(store, config, providers)
}

/// Human-readable scan summary (pretty/plain only).
fn print_scan_summary(
    formatter: &OutputFormatter,
    root: &Path,
    report: &DiscoveryReport,
    candidates: &[DiscoveredProject],
) {
    formatter.print_message(&format!(
        "Scanned {} directory(ies) under {}.",
        report.scanned_dirs,
        root.display()
    ));

    let already = report.registered().count();
    if already > 0 {
        formatter.print_message(&format!("  {already} project(s) already registered."));
    }

    for skipped in report.skipped() {
        match &skipped.status {
            DiscoveryStatus::SharedId { owner } => formatter.print_warning(&format!(
                "{} shares project ID {} with the checkout at {} — skipping (both would share one index).",
                skipped.path.display(),
                skipped.project_id,
                owner.display()
            )),
            DiscoveryStatus::Worktree { main } => formatter.print_warning(&format!(
                "{} is a linked git worktree of {} — skipping (worktrees route to the main checkout automatically; run any engramdb command inside it, or discover the main checkout).",
                skipped.path.display(),
                main.display()
            )),
            _ => {}
        }
    }

    if candidates.is_empty() {
        formatter.print_message("No unregistered projects found.");
    } else {
        formatter.print_message(&format!(
            "Found {} unregistered project(s):",
            candidates.len()
        ));
        for c in candidates {
            formatter.print_message(&format!(
                "  {}  ({}, id {})",
                c.path.display(),
                plural_memories(c.memory_count),
                c.project_id
            ));
        }
    }

    if report.depth_limited {
        formatter.print_hint("Some subtrees were cut off by --max-depth; raise it to scan deeper.");
    }
    // Without this, an unreadable subtree makes "No unregistered projects
    // found" indistinguishable from "there are none".
    if report.unreadable_dirs > 0 {
        formatter.print_hint(&format!(
            "{} director(ies) could not be read (permissions?) and were skipped; this scan may be incomplete.",
            report.unreadable_dirs
        ));
    }
}

/// The `--dry-run` JSON document: what a real run would act on.
///
/// A real run emits [`action_json`] instead. Those two shapes are the whole
/// contract — a run that registers nothing still emits the action shape, so a
/// script never has to sniff which document it got.
fn scan_json(
    root: &Path,
    report: &DiscoveryReport,
    candidates: &[DiscoveredProject],
) -> serde_json::Value {
    serde_json::json!({
        "root": root.display().to_string(),
        "scanned_dirs": report.scanned_dirs,
        "depth_limited": report.depth_limited,
        "dry_run": true,
        "candidates": candidates.iter().map(|c| serde_json::json!({
            "path": c.path.display().to_string(),
            "project_id": c.project_id,
            "memory_count": c.memory_count,
        })).collect::<Vec<_>>(),
        "already_registered": report.registered()
            .map(|p| p.path.display().to_string())
            .collect::<Vec<_>>(),
        "skipped": skipped_json(report),
    })
}

/// Projects found but deliberately not offered, with the reason.
///
/// Present in BOTH documents. These are invisible in JSON otherwise — the
/// human warning lives in `print_scan_summary`, which is suppressed in JSON
/// mode — leaving a consumer unable to tell "skipped for an ID conflict" from
/// "never found", and unable to reconcile found against registered.
fn skipped_json(report: &DiscoveryReport) -> Vec<serde_json::Value> {
    report
        .skipped()
        .map(|p| {
            let (reason, owner) = match &p.status {
                DiscoveryStatus::SharedId { owner } => ("shared_project_id", owner),
                DiscoveryStatus::Worktree { main } => ("git_worktree", main),
                // Not reachable: `skipped()` yields only the two above.
                _ => ("unknown", &p.path),
            };
            serde_json::json!({
                "path": p.path.display().to_string(),
                "project_id": p.project_id,
                "reason": reason,
                "owner": owner.display().to_string(),
            })
        })
        .collect()
}

/// The JSON document for a real (non-`--dry-run`) run: what actually happened.
/// Emitted with empty arrays when nothing was found or everything was
/// declined, so the shape never varies with the outcome.
fn action_json(
    params: &DiscoverParams,
    report: &DiscoveryReport,
    adopted: &[Adopted],
    declined: &[PathBuf],
    errors: &[(PathBuf, String)],
) -> serde_json::Value {
    serde_json::json!({
        "root": params.root.display().to_string(),
        "scanned_dirs": report.scanned_dirs,
        "depth_limited": report.depth_limited,
        "unreadable_dirs": report.unreadable_dirs,
        "dry_run": false,
        "no_index": params.no_index,
        // What the run had to work with, so registered + declined + skipped +
        // errors can be reconciled against it.
        "found_unregistered": report.unregistered().count(),
        "skipped": skipped_json(report),
        "registered": adopted.iter().map(|a| serde_json::json!({
            "path": a.path.display().to_string(),
            "project_id": a.project_id,
            // `null`, not 0, when --no-index skipped the rebuild: reporting
            // `indexed: 0` for a project holding 400 unindexed memories reads
            // as "this project is empty".
            "indexed": a.indexed.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
            "embedded": a.embedded.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
            "warnings": a.warnings,
        })).collect::<Vec<_>>(),
        "declined": declined.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "errors": errors.iter().map(|(p, e)| serde_json::json!({
            "path": p.display().to_string(),
            "error": e,
        })).collect::<Vec<_>>(),
    })
}

fn plural_memories(count: usize) -> String {
    if count == 1 {
        "1 memory".to_string()
    } else {
        format!("{count} memories")
    }
}

/// Last path component, for the progress bar's trailing message.
fn short_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::OutputFormat;
    use crate::prompter::MockPrompter;
    use engramdb::storage::InMemoryRegistry;
    use tempfile::TempDir;

    fn params(root: &Path) -> DiscoverParams {
        DiscoverParams {
            root: root.to_path_buf(),
            max_depth: 4,
            hidden: false,
            follow_symlinks: false,
            yes: false,
            dry_run: false,
            // Registration-only by default: building an engine resolves model
            // providers, which the `ml-models` nextest group serializes.
            // Tests that need the index path opt in and supply a config that
            // genuinely resolves no models (see `fake_project_with_memory`).
            no_index: true,
        }
    }

    /// A project directory that exists on disk but is in no registry.
    fn fake_project(root: &Path, rel: &str) -> PathBuf {
        let dir = root.join(rel);
        let memories = dir.join(".engramdb").join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("a.md"), "---\n---\n").unwrap();
        dir
    }

    /// Like [`fake_project`], but with one real, parseable memory file and a
    /// config that resolves no models, so the index path can be exercised
    /// without loading ONNX.
    ///
    fn fake_project_with_memory(root: &Path, rel: &str) -> PathBuf {
        use engramdb::storage::memory_file::{memory_filename, write_memory_file};
        use engramdb::types::{Memory, MemoryType, Provenance};

        let dir = fake_project(root, rel);
        std::fs::remove_file(dir.join(".engramdb").join("memories").join("a.md")).unwrap();
        // Serialize a real `EngramConfig` rather than hand-writing TOML: the
        // config structs have a chain of fields with no serde defaults
        // (`embeddings.dimensions`, `rerank.model`, `rerank.top_n`, …), a
        // partial table fails the WHOLE-file parse, and `load_config_or_default`
        // then silently falls back to `EngramConfig::default` — which enables
        // the reranker and T5 titling, putting this test back on the
        // model-loading path it exists to avoid. Round-tripping the struct
        // cannot drift as the schema grows.
        let mut config = engramdb::types::EngramConfig::default();
        // Unknown provider -> embeddings resolve to None, no ONNX session.
        config.embeddings.provider = "none".to_string();
        // Both default to loading a model; neither is needed to rebuild an index.
        config.rerank.enabled = false;
        config.nli.enabled = false;
        config.title.strategy = engramdb::types::TitleStrategy::Keyword;
        std::fs::write(
            dir.join(".engramdb").join("config.toml"),
            toml::to_string(&config).unwrap(),
        )
        .unwrap();

        let memory = Memory::new(
            MemoryType::Decision,
            "Discovered summary",
            "Discovered content",
            Provenance::human(),
        );
        std::fs::write(
            dir.join(".engramdb")
                .join("memories")
                .join(memory_filename(&memory)),
            write_memory_file(&memory).unwrap(),
        )
        .unwrap();
        dir
    }

    async fn run(
        params: DiscoverParams,
        registry: &InMemoryRegistry,
        prompter: &MockPrompter,
        json: bool,
    ) -> Result<()> {
        // The format must be explicit: with `None`, a non-TTY stdout (every
        // test harness) resolves to JSON, which would take the never-prompt
        // branch in the interactive tests.
        let format = if json {
            OutputFormat::Json
        } else {
            OutputFormat::Pretty
        };
        run_discover(
            params,
            registry,
            &OutputFormatter::new(Some(format), json, true),
            prompter,
            None,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await
    }

    #[tokio::test]
    async fn accepting_registers_the_project() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "proj");
        let registry = InMemoryRegistry::new();

        run(
            params(tmp.path()),
            &registry,
            &MockPrompter::new(vec!["true"]),
            false,
        )
        .await
        .unwrap();

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(
            PathBuf::from(&reg.projects[0].project_path),
            tmp.path().join("proj").canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn declining_registers_nothing() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "proj");
        let registry = InMemoryRegistry::new();

        run(
            params(tmp.path()),
            &registry,
            &MockPrompter::new(vec!["false"]),
            false,
        )
        .await
        .unwrap();

        assert!(registry.load().await.unwrap().projects.is_empty());
    }

    #[tokio::test]
    async fn prompts_once_per_project_and_honors_each_answer() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "a");
        fake_project(tmp.path(), "b");
        let registry = InMemoryRegistry::new();

        // Candidates are sorted by path, so this accepts `a` and declines `b`.
        run(
            params(tmp.path()),
            &registry,
            &MockPrompter::new(vec!["true", "false"]),
            false,
        )
        .await
        .unwrap();

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert!(reg.projects[0].project_path.ends_with("a"));
    }

    #[tokio::test]
    async fn dry_run_never_registers_and_never_prompts() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "proj");
        let registry = InMemoryRegistry::new();

        let mut p = params(tmp.path());
        p.dry_run = true;
        let prompter = MockPrompter::new(vec![]);
        run(p, &registry, &prompter, false).await.unwrap();

        assert!(registry.load().await.unwrap().projects.is_empty());
        assert_eq!(
            prompter.prompt_count(),
            0,
            "--dry-run must not even ask; an exhausted queue only errors, which \
             the caller could absorb into a decline"
        );
    }

    #[tokio::test]
    async fn yes_skips_prompting() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "a");
        fake_project(tmp.path(), "b");
        let registry = InMemoryRegistry::new();

        let mut p = params(tmp.path());
        p.yes = true;
        let prompter = MockPrompter::new(vec![]);
        run(p, &registry, &prompter, false).await.unwrap();

        assert_eq!(registry.load().await.unwrap().projects.len(), 2);
        assert_eq!(prompter.prompt_count(), 0, "--yes must not prompt");
    }

    #[tokio::test]
    async fn json_mode_without_yes_errors_instead_of_prompting() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "proj");
        let registry = InMemoryRegistry::new();

        let err = run(
            params(tmp.path()),
            &registry,
            &MockPrompter::new(vec![]),
            true,
        )
        .await
        .expect_err("JSON mode must refuse to prompt");
        assert!(format!("{err}").contains("--yes"));
        assert!(registry.load().await.unwrap().projects.is_empty());
    }

    #[tokio::test]
    async fn json_dry_run_is_allowed_without_yes() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "proj");
        let registry = InMemoryRegistry::new();

        let mut p = params(tmp.path());
        p.dry_run = true;
        run(p, &registry, &MockPrompter::new(vec![]), true)
            .await
            .unwrap();

        assert!(registry.load().await.unwrap().projects.is_empty());
    }

    #[tokio::test]
    async fn already_registered_project_is_not_offered_again() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(&dir, &registry).await.unwrap();

        let prompter = MockPrompter::new(vec![]);
        run(params(tmp.path()), &registry, &prompter, false)
            .await
            .unwrap();

        assert_eq!(registry.load().await.unwrap().projects.len(), 1);
        assert_eq!(
            prompter.prompt_count(),
            0,
            "an already-registered project must never be offered"
        );
    }

    /// The whole point of the command: an adopted project's memories are in the
    /// index afterwards, not just its path in `registry.json`. `list_summary`
    /// reads the LanceDB table, so a passing assert means the rebuild ran.
    #[tokio::test]
    async fn accepting_rebuilds_the_index_from_the_on_disk_memories() {
        let tmp = TempDir::new().unwrap();
        let dir = fake_project_with_memory(tmp.path(), "proj");
        let registry = InMemoryRegistry::new();

        let mut p = params(tmp.path());
        p.yes = true;
        p.no_index = false; // the config above resolves no models
        run(p, &registry, &MockPrompter::new(vec![]), false)
            .await
            .unwrap();

        let store = MemoryStore::open(&dir).await.unwrap();
        let indexed = store.list_summary().await.unwrap();
        assert_eq!(
            indexed.len(),
            1,
            "the discovered memory must be in the index, not merely on disk"
        );
    }

    /// Two clones of one remote, neither registered — the registry-loss case.
    /// Only one may be adopted: the registry keeps the first registration, so
    /// "registering" the second is a no-op that would still be reported as
    /// success while its memories were reindexed into the first's index.
    #[tokio::test]
    async fn colliding_clones_are_not_both_registered() {
        let tmp = TempDir::new().unwrap();
        for name in ["a-clone", "z-clone"] {
            let dir = fake_project(tmp.path(), name);
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            std::fs::write(
                dir.join(".git").join("config"),
                "[remote \"origin\"]\n\turl = git@github.com:acme/thing.git\n",
            )
            .unwrap();
        }
        let registry = InMemoryRegistry::new();

        let mut p = params(tmp.path());
        p.yes = true;
        let prompter = MockPrompter::new(vec![]);
        run(p, &registry, &prompter, false).await.unwrap();

        let reg = registry.load().await.unwrap();
        assert_eq!(
            reg.projects.len(),
            1,
            "two checkouts sharing one project ID must not both be adopted"
        );
        assert!(reg.projects[0].project_path.ends_with("a-clone"));

        // The registry count alone does NOT prove the fix: `update` keeps the
        // first registration, so it reads as 1 even when the second clone was
        // adopted too. `MemoryStore::init` writing a manifest is the part that
        // only happens when a project is actually put through adoption.
        assert!(
            !tmp.path()
                .join("z-clone")
                .join(".engramdb")
                .join("manifest.toml")
                .exists(),
            "the colliding clone must never be handed to MemoryStore::init"
        );
    }

    /// Pins `adopt`'s `--no-index` early return: without it the rebuild runs
    /// and the memory lands in the index anyway.
    #[tokio::test]
    async fn no_index_registers_without_rebuilding_the_index() {
        let tmp = TempDir::new().unwrap();
        let dir = fake_project_with_memory(tmp.path(), "proj");
        let registry = InMemoryRegistry::new();

        let mut p = params(tmp.path()); // no_index: true
        p.yes = true;
        run(p, &registry, &MockPrompter::new(vec![]), false)
            .await
            .unwrap();

        assert_eq!(registry.load().await.unwrap().projects.len(), 1);
        let store = MemoryStore::open(&dir).await.unwrap();
        assert!(
            store.list_summary().await.unwrap().is_empty(),
            "--no-index must leave the index untouched"
        );
    }

    /// A project that cannot be adopted must not abandon the batch, and the
    /// command must not exit 0 having failed to do what it was asked.
    #[tokio::test]
    async fn a_failing_project_is_reported_and_fails_the_command() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "healthy");
        // `.engramdb/` exists (so it is discovered) but `memories` is a FILE,
        // so `MemoryStore::init`'s `create_dir_all` fails.
        let broken = tmp.path().join("broken").join(".engramdb");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("memories"), "not a directory").unwrap();

        let registry = InMemoryRegistry::new();
        let mut p = params(tmp.path());
        p.yes = true;
        let err = run(p, &registry, &MockPrompter::new(vec![]), false)
            .await
            .expect_err("a failed registration must surface as a non-zero exit");
        assert!(format!("{err}").contains("could not be registered"));

        // ...and the healthy one was still adopted.
        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert!(reg.projects[0].project_path.ends_with("healthy"));
    }

    /// The prompt drives a destructive-ish default (`true`), so a prompt that
    /// cannot be answered must fail loudly rather than decline everything and
    /// report "Nothing registered" as success.
    #[tokio::test]
    async fn a_failing_prompt_errors_instead_of_silently_declining() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "proj");
        let registry = InMemoryRegistry::new();

        // Empty queue == the prompter cannot answer (stdin at EOF).
        let err = run(
            params(tmp.path()),
            &registry,
            &MockPrompter::new(vec![]),
            false,
        )
        .await
        .expect_err("an unanswerable prompt must not be treated as 'no'");
        assert!(format!("{err}").contains("no more responses"));
        assert!(registry.load().await.unwrap().projects.is_empty());
    }

    #[tokio::test]
    async fn nonexistent_root_errors() {
        let tmp = TempDir::new().unwrap();
        let mut p = params(&tmp.path().join("no-such-dir"));
        p.yes = true;
        let registry = InMemoryRegistry::new();
        assert!(run(p, &registry, &MockPrompter::new(vec![]), false)
            .await
            .is_err());
    }
}
