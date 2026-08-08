//! Handler for the `engramdb harvest` subcommand.
//!
//! Supplies `/engram:harvest` with its raw material. The binary's job stops
//! at *presenting* past sessions — deciding what is worth remembering is the
//! agent's, and saving is the user's. Nothing here writes a memory.

use crate::app::{HarvestCommand, LedgerCommand};
use crate::output::{HarvestSessionOutput, OutputFormatter};
use crate::prompter::Prompter;
use anyhow::{bail, Context, Result};
use engramdb::daemon::{DaemonCell, DaemonPolicy};
use engramdb::ops::harvest::{self, resolve_ledger_key};
use engramdb::ops::harvest_index;
use engramdb::retrieval::engine::RetrievalEngine;
use engramdb::storage::conversation_index::{ConversationHit, ConversationIndex, MatchedOn};
use engramdb::storage::harvest_state::{self, HarvestDecision, HarvestEntry};
use engramdb::storage::transcripts::{self, ParseOptions};
use engramdb::storage::{transcript_archive, MemoryStore, RegistryBackend};
use engramdb::types::{EmbeddingBackend, EngramConfig, HarvestConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Everything the index-backed subcommands need to reach a model.
///
/// Bundled rather than threaded as four more arguments: `list`, `show`,
/// `mark`, `reset` and every `ledger` subcommand never touch a model, and the
/// engine is built lazily so those paths still cost nothing.
pub struct HarvestEngineContext<'a> {
    pub backend: Option<EmbeddingBackend>,
    pub cell: &'a Arc<DaemonCell>,
    pub policy: DaemonPolicy,
}

/// Run the `harvest` command.
#[allow(clippy::too_many_arguments)]
pub async fn run_harvest(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: HarvestCommand,
    full_config: &EngramConfig,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
    engine_ctx: HarvestEngineContext<'_>,
) -> Result<()> {
    let config = &full_config.harvest;
    // Resolved once, up front: every branch below touches the ledger, and the
    // ledger belongs to the **root** project — the same root the archives are
    // keyed by. `resolve_project_root` only rewrites `dir` for git worktrees,
    // so for a project linked with `projects link` this is the only thing that
    // keeps the two halves pointing at the same place.
    let scope = harvest::session_scope(dir, registry).await?;
    let ledger_dir = scope.root_dir.as_path();
    match command {
        HarvestCommand::List {
            since,
            limit,
            include_harvested,
            include_empty,
            all_projects,
            exclude_session,
        } => {
            let params = harvest::SelectParams {
                since: since.as_deref().map(harvest::parse_since).transpose()?,
                limit,
                exclude_session,
                include_harvested,
                all_projects,
                skip_empty: !include_empty,
            };
            let sessions = harvest::select_sessions(&scope, ledger_dir, &params)?;

            let output: Vec<HarvestSessionOutput> = sessions.iter().map(harvest_row).collect();
            formatter.print_harvest_sessions(&output, &scope.paths);
        }

        HarvestCommand::Show {
            session_id,
            max_chars,
            include_thinking,
            include_sidechains,
            no_tools,
            all_projects,
        } => {
            // A live transcript is preferred, but Claude Code prunes its own —
            // and reading a *pruned* session is the entire reason archives
            // exist. `_restored` holds the temp file alive for the digest.
            let (transcript_path, _restored) =
                match resolve_session(&scope, &session_id, all_projects) {
                    Ok(selected) => (selected.transcript_path, None),
                    Err(live_err) => {
                        match harvest::restore_archived_session(&scope, &session_id)? {
                            Some((guard, path)) => (path, Some(guard)),
                            None => return Err(live_err),
                        }
                    }
                };
            let params = harvest::DigestParams {
                parse: ParseOptions {
                    // Flags turn features *on*; config supplies the baseline,
                    // so `--include-thinking` on a config that already enables
                    // it is a no-op rather than a toggle-off.
                    include_thinking: include_thinking || config.include_thinking,
                    include_sidechains: include_sidechains || config.include_sidechains,
                    tools: if no_tools {
                        transcripts::ToolDetail::None
                    } else {
                        transcripts::ToolDetail::All
                    },
                },
                max_chars: match max_chars {
                    Some(0) => usize::MAX,
                    Some(n) => n,
                    None => config.effective_digest_budget(),
                },
            };
            let digest = harvest::digest_session(&transcript_path, params)?;
            let (markdown, fence) = harvest::render_digest_markdown_traced(&digest);

            if formatter.is_json() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&harvest::DigestJson::new(
                        &digest, &fence, markdown
                    ))?
                );
            } else {
                println!("{markdown}");
            }
        }

        HarvestCommand::Mark {
            session_id,
            memory_ids,
            all_projects,
            defer,
            note,
            summary,
        } => {
            // `mark` must reach every session `show` can, or the ledger
            // silently re-offers one forever. That now includes sessions whose
            // live transcript is gone and which `show` reads from an archive,
            // so fall back to the ledger exactly as `reset` does.
            let resolved = match resolve_session(&scope, &session_id, all_projects) {
                Ok(selected) => selected.session_id,
                Err(live_err) => {
                    resolve_ledger_key(ledger_dir, &session_id).map_err(|_| live_err)?
                }
            };
            let decision = if defer {
                HarvestDecision::Deferred
            } else if memory_ids.is_empty() {
                HarvestDecision::Skipped
            } else {
                HarvestDecision::Harvested
            };
            let entry =
                harvest_state::mark_harvested(ledger_dir, &resolved, &memory_ids, decision, note)?;
            // After the decision is recorded, never before: the ledger write
            // is the thing that must not be lost, and attaching a summary
            // needs a model that may not load. A session with a decision and
            // no summary is a normal state; one with a summary and no
            // decision would keep being re-offered.
            if let Some(summary) = summary {
                write_summary(
                    dir,
                    full_config,
                    &scope,
                    &engine_ctx,
                    &resolved,
                    &summary,
                    formatter,
                )
                .await?;
            }
            if formatter.is_json() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&entry_json(&resolved, &entry))?
                );
            } else {
                formatter.print_success(&describe_mark(&resolved, &entry));
            }
        }

        HarvestCommand::Index {
            session_id,
            all,
            force,
        } => {
            let (index, engine) = open_engine(dir, full_config, &scope, &engine_ctx).await?;
            let ids = match (session_id, all) {
                (Some(prefix), _) => vec![resolve_indexable(&scope, &prefix)?],
                (None, true) => harvest_index::all_indexable(&scope)?,
                (None, false) => bail!(
                    "Name a session to index, or pass --all. \
`engramdb harvest ledger list` shows which sessions have bytes behind them."
                ),
            };
            let report =
                harvest_index::index_sessions(&scope, &index, &engine, &ids, force).await?;
            print_index_report(&report, formatter)?;
        }

        HarvestCommand::Search {
            query,
            limit,
            since,
            all_projects,
        } => {
            let since = since.as_deref().map(harvest::parse_since).transpose()?;
            let (index, engine) = open_engine(dir, full_config, &scope, &engine_ctx).await?;
            let mut hits = harvest_index::search(&index, &engine, &query, limit, since).await?;
            if all_projects {
                hits = search_all_projects(
                    registry,
                    full_config,
                    &engine,
                    &scope,
                    &query,
                    limit,
                    since,
                    hits,
                )
                .await?;
            }
            print_search_hits(&hits, formatter)?;
        }

        HarvestCommand::Summary {
            session_id,
            text,
            editor,
            from_file,
        } => {
            let resolved = resolve_ledger_key(ledger_dir, &session_id)
                .or_else(|_| resolve_indexable(&scope, &session_id))?;
            let body = read_summary_text(text, editor, from_file, prompter)?;
            write_summary(
                dir,
                full_config,
                &scope,
                &engine_ctx,
                &resolved,
                &body,
                formatter,
            )
            .await?;
        }

        HarvestCommand::Reset { session_id } => {
            let resolved = resolve_ledger_key(ledger_dir, &session_id)?;
            let outcome = harvest_state::clear_harvested(ledger_dir, &resolved)?;
            formatter.print_success(&describe_reset(&resolved, outcome));
        }

        HarvestCommand::Ledger { command } => {
            run_ledger(&scope, command, config, formatter, prompter).await?;
        }
    }
    Ok(())
}

/// Open the conversation table and a model-backed engine for this scope.
///
/// Both at once, because every command that needs one needs the other and the
/// failure modes read the same: no model means nothing to embed, and no table
/// means nothing to embed *into*.
async fn open_engine(
    dir: &Path,
    config: &EngramConfig,
    scope: &harvest::SessionScope,
    ctx: &HarvestEngineContext<'_>,
) -> Result<(ConversationIndex, RetrievalEngine)> {
    let index = harvest_index::open_index(scope, config.embeddings.dimensions).await?;
    let store = MemoryStore::open(dir)
        .await
        .context("open the memory store for conversation indexing")?;
    let engine = crate::engine::engine_for(store, ctx.backend, ctx.cell, ctx.policy).await;
    if !engine.embeddings_available() {
        bail!(
            "No embedding provider is available, so conversations cannot be indexed or \
searched. Run `engramdb doctor` to see why the model did not load."
        );
    }
    Ok((index, engine))
}

/// Resolve a session-id prefix for the index commands.
///
/// Wider than [`resolve_ledger_key`] on purpose: a session with a live
/// transcript and no ledger entry at all is perfectly indexable, and refusing
/// it would mean nothing could be indexed before it was reviewed.
fn resolve_indexable(scope: &harvest::SessionScope, prefix: &str) -> Result<String> {
    let mut matches: Vec<String> = harvest_index::all_indexable(scope)?
        .into_iter()
        .filter(|id| id.starts_with(prefix))
        .collect();
    matches.sort();
    matches.dedup();
    match matches.len() {
        0 => bail!(
            "No session matching '{prefix}' has any bytes behind it — neither a live transcript \
nor a collected copy. `engramdb harvest ledger list --with-archive` shows which sessions do."
        ),
        1 => Ok(matches.remove(0)),
        n => bail!(
            "Ambiguous session id '{prefix}' — matches {n} sessions: {}",
            matches.join(", ")
        ),
    }
}

/// Where the summary text comes from: an argument, `$EDITOR`, or a file.
fn read_summary_text(
    text: Option<String>,
    editor: bool,
    from_file: Option<PathBuf>,
    _prompter: &dyn Prompter,
) -> Result<String> {
    if let Some(path) = from_file {
        // `-` reads stdin, so a summary can be piped in without a temp file.
        if path == Path::new("-") {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                .context("read the summary from stdin")?;
            return Ok(buf);
        }
        return std::fs::read_to_string(&path)
            .with_context(|| format!("read the summary from {}", path.display()));
    }
    if editor {
        return compose_in_editor();
    }
    text.ok_or_else(|| {
        anyhow::anyhow!(
            "Pass the summary text, --editor, or --from-file. An empty string clears the \
summary and its vector."
        )
    })
}

/// Compose a summary in `$EDITOR`, mirroring `add --editor`.
fn compose_in_editor() -> Result<String> {
    let temp = tempfile::Builder::new()
        .prefix("engramdb-summary-")
        .suffix(".md")
        .tempfile()
        .context("create the editor scratch file")?;
    std::fs::write(
        temp.path(),
        "# One or two sentences about what this conversation settled.\n\
         # Lines starting with '#' are ignored.\n",
    )?;
    let editor_raw = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let parts = shell_words::split(&editor_raw)
        .map_err(|e| anyhow::anyhow!("Invalid EDITOR value '{editor_raw}': {e}"))?;
    let (cmd, args) = parts
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("EDITOR environment variable is empty"))?;
    let status = std::process::Command::new(cmd)
        .args(args)
        .arg(temp.path())
        .status()
        .with_context(|| format!("Failed to launch editor '{cmd}'"))?;
    if !status.success() {
        bail!("Editor exited with non-zero status");
    }
    let body = std::fs::read_to_string(temp.path()).context("read the edited summary")?;
    Ok(body
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Attach (or clear) a curated summary, re-embedding only `summary_vec`.
async fn write_summary(
    dir: &Path,
    config: &EngramConfig,
    scope: &harvest::SessionScope,
    ctx: &HarvestEngineContext<'_>,
    session_id: &str,
    body: &str,
    formatter: &OutputFormatter,
) -> Result<()> {
    let (index, engine) = open_engine(dir, config, scope, ctx).await?;
    // A summary for a session with no row has nowhere to go, and silently
    // indexing here would make `harvest summary` an embedding of the whole
    // conversation rather than of two sentences. Index first, explicitly.
    if index.fetch(session_id).await?.is_none() {
        harvest_index::index_session(scope, &index, &engine, session_id, false).await?;
    }
    harvest_index::set_summary(&index, &engine, session_id, body).await?;
    if body.trim().is_empty() {
        formatter.print_success(&format!("Cleared the summary for session {session_id}."));
    } else {
        formatter.print_success(&format!("Summary recorded for session {session_id}."));
    }
    Ok(())
}

/// Fold in every *other* root project's conversations.
///
/// Only projects that already have a table are opened: opening one creates it,
/// and a machine-wide search must not leave an empty table behind in every
/// project it merely looked at.
#[allow(clippy::too_many_arguments)]
async fn search_all_projects(
    registry: &dyn RegistryBackend,
    config: &EngramConfig,
    engine: &RetrievalEngine,
    own: &harvest::SessionScope,
    query: &str,
    limit: usize,
    since: Option<chrono::DateTime<chrono::Utc>>,
    mut hits: Vec<ConversationHit>,
) -> Result<Vec<ConversationHit>> {
    let data = registry.load().await?;
    let mut seen: Vec<String> = vec![own.root_project_id.clone()];
    for entry in &data.projects {
        // Project *ids*, never paths: two registry entries can name the same
        // checkout through a symlink, and one of them would then be searched
        // twice while the dedupe silently failed.
        let root = engramdb::storage::resolve_root_project_id(&data, &entry.project_id);
        if seen.contains(&root) || !ConversationIndex::exists(&root) {
            continue;
        }
        seen.push(root.clone());
        let index = ConversationIndex::open(&root, config.embeddings.dimensions).await?;
        hits.extend(harvest_index::search(&index, engine, query, limit, since).await?);
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    hits.truncate(limit);
    Ok(hits)
}

fn print_index_report(
    report: &harvest_index::IndexReport,
    formatter: &OutputFormatter,
) -> Result<()> {
    if formatter.is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "indexed": report.indexed,
                "unchanged": report.unchanged,
                "skipped": report.skipped.iter().map(|s| serde_json::json!({
                    "session_id": s.session_id,
                    "reason": s.reason,
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    formatter.print_success(&format!(
        "Indexed {} conversation(s); {} already current.",
        report.indexed.len(),
        report.unchanged.len()
    ));
    // Named individually rather than counted: a conversation missing from
    // search is indistinguishable from one that never mentioned the topic.
    for skipped in &report.skipped {
        formatter.print_warning(&format!(
            "{}: {}",
            crate::output::short_id(&skipped.session_id),
            skipped.reason
        ));
    }
    Ok(())
}

/// The human-readable search listing, built as text so it can be asserted on
/// without capturing stdout.
fn render_search_hits(hits: &[ConversationHit]) -> String {
    if hits.is_empty() {
        return "No indexed conversation matched. Sessions become searchable at harvest or after \
`[harvest] index_after_hours`; `engramdb harvest index --all` indexes them now.\n"
            .to_string();
    }
    let mut out = String::new();
    for hit in hits {
        out.push_str(&format!(
            "{}  {:.3} ({})  {}\n",
            crate::output::short_id(&hit.session_id),
            hit.score,
            match hit.matched_on {
                MatchedOn::Digest => "digest",
                MatchedOn::Summary => "summary",
            },
            hit.ended_at
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "unknown date".into())
        ));
        // Both strings are transcript-derived and already sanitized on the
        // way into the table; re-sanitizing costs nothing and keeps this
        // renderer safe if the row ever gains another writer.
        if let Some(summary) = &hit.summary {
            out.push_str(&format!(
                "    {}\n",
                transcripts::sanitize_one_line(summary)
            ));
        } else if let Some(prompt) = &hit.first_prompt {
            out.push_str(&format!("    {}\n", transcripts::sanitize_one_line(prompt)));
        }
        // A partial row is a session whose tail was never embedded, so a miss
        // against it is not evidence the topic was absent.
        if !hit.indexed_complete {
            out.push_str("    (partial: only the head of this conversation is indexed)\n");
        }
    }
    out.push_str("\nRead one with `engramdb harvest show <id>`.\n");
    out
}

fn print_search_hits(hits: &[ConversationHit], formatter: &OutputFormatter) -> Result<()> {
    if formatter.is_json() {
        let out: Vec<_> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "session_id": h.session_id,
                    "project_id": h.project_id,
                    "score": (h.score * 1000.0).round() / 1000.0,
                    "matched_on": match h.matched_on {
                        MatchedOn::Digest => "digest",
                        MatchedOn::Summary => "summary",
                    },
                    "cwd": h.cwd,
                    "git_branch": h.git_branch,
                    "started_at": h.started_at,
                    "ended_at": h.ended_at,
                    "first_prompt": h.first_prompt,
                    "summary": h.summary,
                    "indexed_complete": h.indexed_complete,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    print!("{}", render_search_hits(hits));
    Ok(())
}

/// One `harvest list` row, with every transcript-derived string sanitized.
///
/// Sanitizing here rather than in the pretty renderer is deliberate: these
/// three fields are bytes another program wrote and third parties fed, and
/// `--format json` used to emit them verbatim. `serde_json` escapes C0
/// controls, so the terminal-rewriting characters the pretty path guards
/// against were covered by accident — but bidi overrides, zero-width joiners
/// and the rest of the invisible set are ordinary Unicode to a JSON encoder
/// and passed straight through, into whatever renders the JSON next. The MCP
/// listing has always sanitized the same three fields; this is the CLI
/// catching up.
fn harvest_row(s: &harvest::SelectedSession) -> HarvestSessionOutput {
    let clean = |v: &Option<String>| {
        v.as_deref()
            .map(|t| transcripts::sanitize_one_line(t).into_owned())
    };
    HarvestSessionOutput {
        session_id: s.summary.session_id.clone(),
        cwd: clean(&s.summary.cwd),
        git_branch: clean(&s.summary.git_branch),
        started_at: s.summary.started_at,
        ended_at: s.summary.ended_at,
        user_turns: s.summary.user_turns,
        assistant_turns: s.summary.assistant_turns,
        bytes: s.summary.bytes,
        first_prompt: clean(&s.summary.first_prompt),
        already_harvested: s.already_harvested,
    }
}

/// Human-readable one-liner for a recorded decision.
fn describe_mark(session_id: &str, entry: &HarvestEntry) -> String {
    match entry.decision() {
        HarvestDecision::Deferred => {
            format!("Deferred session {session_id}; it will keep appearing in `harvest list`.")
        }
        // Not reachable from `mark`, which always records a review — the hook
        // is the only writer of this decision.
        HarvestDecision::Unreviewed => {
            format!("Session {session_id} is recorded but not yet reviewed.")
        }
        HarvestDecision::Skipped => {
            format!("Marked session {session_id} as reviewed (no memories saved).")
        }
        HarvestDecision::Harvested => format!(
            "Marked session {} as harvested ({} memor{} saved).",
            session_id,
            entry.memories_created,
            if entry.memories_created == 1 {
                "y"
            } else {
                "ies"
            }
        ),
    }
}

/// Human-readable one-liner for `harvest reset`.
///
/// Two outcomes, so two messages. The old single line said the session "will
/// be offered again" either way, and `harvest list` reads *live* transcripts
/// only: for a session Claude Code has already pruned, nothing offers it. The
/// archived case now says where the transcript actually is, since resetting no
/// longer discards the entry that points at it.
fn describe_reset(session_id: &str, outcome: harvest_state::ClearOutcome) -> String {
    if outcome.kept_archive() {
        format!(
            "Cleared the review of session {session_id}; it is unreviewed again. Its archived \
transcript is kept — `harvest ledger list` finds it and `harvest show` digests it."
        )
    } else {
        format!(
            "Cleared the harvest record for session {session_id}. No archived transcript is held \
for it, so `harvest list` offers it again only while Claude Code still has the live one."
        )
    }
}

fn entry_json(session_id: &str, entry: &HarvestEntry) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "decision": entry.decision(),
        "stage": entry.stage,
        "harvested_at": entry.harvested_at,
        "memories_created": entry.memories_created,
        "memory_ids": entry.memory_ids,
        "note": entry.note,
        "archive": entry.archive.as_ref().map(|a| serde_json::json!({
            "file_name": a.file_name,
            "bytes": a.bytes,
            "original_bytes": a.original_bytes,
            "ratio": (a.ratio() * 10.0).round() / 10.0,
            "sha256": a.sha256,
            "archived_at": a.archived_at,
        })),
    })
}

/// Ledger subcommands, driven entirely off the resolved scope.
///
/// Takes the scope rather than the invoking directory so the ledger it reads
/// and the archive directory it deletes from are, by construction, the same
/// project's — the pairing whose absence let a prune from a sub-project erase
/// archives its parent's ledger still pointed at.
async fn run_ledger(
    scope: &harvest::SessionScope,
    command: LedgerCommand,
    config: &HarvestConfig,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
) -> Result<()> {
    let dir = scope.root_dir.as_path();
    let project_id = scope.root_project_id.as_str();
    match command {
        LedgerCommand::List {
            decision,
            stage,
            with_archive,
        } => {
            let wanted = decision.as_deref().map(parse_decision).transpose()?;
            let wanted_stage = stage.as_deref().map(parse_stage).transpose()?;
            let ledger = harvest_state::read_harvested(dir);
            let mut rows: Vec<(String, HarvestEntry)> = ledger
                .into_iter()
                .filter(|(_, e)| wanted.is_none_or(|w| e.decision() == w))
                .filter(|(_, e)| wanted_stage.is_none_or(|w| e.stage == w))
                .filter(|(_, e)| !with_archive || e.archive.is_some())
                .collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.1.harvested_at));

            if formatter.is_json() {
                let out: Vec<_> = rows.iter().map(|(id, e)| entry_json(id, e)).collect();
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else if rows.is_empty() {
                println!("Ledger is empty.");
            } else {
                for (id, e) in &rows {
                    let archive = e
                        .archive
                        .as_ref()
                        .map(|a| format!("  archive {}", human_bytes(a.bytes)))
                        .unwrap_or_default();
                    println!(
                        "{}  {:?}/{:?}  {}  {} memor{}{}",
                        crate::output::short_id(id),
                        e.decision(),
                        e.stage,
                        e.harvested_at.format("%Y-%m-%d %H:%M"),
                        e.memories_created,
                        if e.memories_created == 1 { "y" } else { "ies" },
                        archive
                    );
                    if let Some(note) = &e.note {
                        println!("    {}", transcripts::sanitize_one_line(note));
                    }
                }
            }
        }

        LedgerCommand::Show { session_id } => {
            let key = resolve_ledger_key(dir, &session_id)?;
            let ledger = harvest_state::read_harvested(dir);
            // `resolve_ledger_key` read the ledger separately, so a concurrent
            // SessionEnd hook or `harvest reset` can drop the key in between.
            // Indexing a `HashMap` would panic on that race.
            let entry = ledger
                .get(&key)
                .ok_or_else(|| anyhow::anyhow!("No harvest record for session {key}"))?;
            if formatter.is_json() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&entry_json(&key, entry))?
                );
            } else {
                println!("Session:   {key}");
                println!("Decision:  {:?}", entry.decision());
                println!("Stage:     {:?}", entry.stage);
                println!(
                    "Recorded:  {}",
                    entry.harvested_at.format("%Y-%m-%d %H:%M UTC")
                );
                println!("Memories:  {}", entry.memories_created);
                if !entry.memory_ids.is_empty() {
                    println!("           {}", entry.memory_ids.join(", "));
                }
                if let Some(note) = &entry.note {
                    println!("Note:      {}", transcripts::sanitize_one_line(note));
                }
                match &entry.archive {
                    Some(a) => {
                        println!(
                            "Archive:   {} ({} from {}, {:.1}x)",
                            a.file_name,
                            human_bytes(a.bytes),
                            human_bytes(a.original_bytes),
                            a.ratio()
                        );
                        println!("           sha256 {}", a.sha256);
                    }
                    None => println!("Archive:   none"),
                }
            }
        }

        LedgerCommand::Export { session_id, output } => {
            let key = resolve_ledger_key(dir, &session_id)?;
            let ledger = harvest_state::read_harvested(dir);
            let Some(archive) = ledger.get(&key).and_then(|e| e.archive.clone()) else {
                bail!(
                    "Session {key} has no archived transcript. Archiving is controlled by \
`[harvest] archive` and only captures sessions that ended after it was enabled."
                );
            };
            // The ledger can outlive the file: eviction on another machine, a
            // restored backup, or a manual cleanup all strand the reference.
            // Say so plainly rather than surfacing a bare "no such file".
            if !transcript_archive::archive_path(project_id, &key)?.exists() {
                bail!(
                    "Session {key} has a recorded archive ({}) but the file is gone — it was \
most likely evicted by `harvest ledger prune` or the `[harvest] archive_*` budgets.",
                    archive.file_name
                );
            }
            let dest = output.unwrap_or_else(|| PathBuf::from(format!("{key}.jsonl")));
            // The ledger recorded the plaintext size at archive time, so the
            // decompression bound here is exact rather than a backstop.
            let sha = transcript_archive::export_archive_bounded(
                project_id,
                &key,
                &dest,
                Some(archive.original_bytes),
            )?;
            if sha != archive.sha256 {
                bail!(
                    "Exported {} but its checksum does not match the one recorded at archive \
time — the archive is corrupt.",
                    dest.display()
                );
            }
            formatter.print_success(&format!(
                "Exported {} ({}), checksum verified.",
                dest.display(),
                human_bytes(archive.original_bytes)
            ));
        }

        LedgerCommand::Rm {
            session_id,
            archive_only,
            force,
        } => {
            let key = resolve_ledger_key(dir, &session_id)?;
            // Read the archive metadata *before* deleting, so the prompt can
            // say how much conversation is about to go.
            let archive = harvest_state::read_harvested(dir)
                .get(&key)
                .and_then(|e| e.archive.clone());

            if !force {
                // Follows `delete` / `projects delete` rather than the
                // `--apply` sweeps: this names one target, so a confirmation
                // is the right guard and the preview *is* the dry run. What
                // it destroys is unrecoverable — once Claude Code prunes its
                // own transcript, the archive is the only remaining copy.
                if formatter.is_json() {
                    bail!(
                        "removing a ledger entry requires confirmation; re-run with --force \
in JSON mode"
                    );
                }
                formatter.print_warning(&match (archive_only, &archive) {
                    (true, Some(a)) => format!(
                        "This deletes the archived transcript for session {key} ({}) — the \
only remaining copy, since Claude Code prunes its own.",
                        human_bytes(a.original_bytes)
                    ),
                    (true, None) => {
                        format!("Session {key} has no archived transcript; nothing to delete.")
                    }
                    (false, Some(a)) => format!(
                        "This deletes the harvest record for session {key} AND its archived \
transcript ({}), the only remaining copy. The session will be offered again.",
                        human_bytes(a.original_bytes)
                    ),
                    (false, None) => format!(
                        "This deletes the harvest record for session {key}. The session will \
be offered again by `harvest list`."
                    ),
                });
                if !prompter.confirm("Continue?", false).unwrap_or(false) {
                    formatter.print_message("Aborted.");
                    return Ok(());
                }
            }

            let removed_archive = transcript_archive::remove_archive(project_id, &key)?;
            if archive_only {
                // Keep the review record, drop the now-dangling file pointer.
                harvest_state::clear_archive_refs(dir, std::slice::from_ref(&key))?;
                // Honor the bool: without this, a second run reports success
                // over a file that was already gone.
                if removed_archive {
                    formatter.print_success(&format!("Removed archive for session {key}."));
                } else {
                    formatter
                        .print_message(&format!("Session {key} had no archive; nothing removed."));
                }
            } else {
                // Drop the reference first: `clear_harvested` keeps an entry
                // that still names an archive, and here the file has just been
                // deleted — nothing to strand, so the entry must go too.
                harvest_state::clear_archive_refs(dir, std::slice::from_ref(&key))?;
                harvest_state::clear_harvested(dir, &key)?;
                formatter.print_success(&format!(
                    "Removed {} for session {key}.",
                    if removed_archive {
                        "ledger entry and archive"
                    } else {
                        "ledger entry"
                    }
                ));
            }
        }

        LedgerCommand::Prune {
            older_than,
            max_bytes,
            apply,
        } => {
            let retention = match older_than.as_deref() {
                Some(spec) => Some(parse_days(spec)?),
                None => config.archive_retention_days,
            };
            let cap = max_bytes.unwrap_or(config.archive_max_bytes);
            let outcome = transcript_archive::prune_archives(project_id, retention, cap, !apply)?;
            if apply {
                // The files are gone; the ledger must stop pointing at them or
                // `show` advertises an export that cannot succeed.
                harvest_state::clear_archive_refs(dir, &outcome.removed)?;
            }

            if formatter.is_json() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "dry_run": !apply,
                        "removed": outcome.removed,
                        "bytes_freed": outcome.bytes_freed,
                        "bytes_remaining": outcome.bytes_remaining,
                    }))?
                );
            } else if outcome.removed.is_empty() {
                println!(
                    "Nothing to prune ({} held).",
                    human_bytes(outcome.bytes_remaining)
                );
            } else {
                println!(
                    "{} {} archive(s), freeing {} ({} {} remain).",
                    if apply { "Removed" } else { "Would remove" },
                    outcome.removed.len(),
                    human_bytes(outcome.bytes_freed),
                    human_bytes(outcome.bytes_remaining),
                    if apply { "now" } else { "would" }
                );
                if !apply {
                    println!("Re-run with --apply to delete.");
                }
            }
        }
    }
    Ok(())
}

fn parse_decision(value: &str) -> Result<HarvestDecision> {
    match value.to_ascii_lowercase().as_str() {
        "harvested" => Ok(HarvestDecision::Harvested),
        "skipped" => Ok(HarvestDecision::Skipped),
        "deferred" => Ok(HarvestDecision::Deferred),
        "unreviewed" => Ok(HarvestDecision::Unreviewed),
        other => bail!(
            "unknown decision '{other}' (expected harvested, skipped, deferred, or unreviewed)"
        ),
    }
}

fn parse_stage(value: &str) -> Result<harvest_state::HarvestStage> {
    use harvest_state::HarvestStage;
    match value.to_ascii_lowercase().as_str() {
        "collected" => Ok(HarvestStage::Collected),
        "indexed" => Ok(HarvestStage::Indexed),
        "compressed" => Ok(HarvestStage::Compressed),
        other => bail!("unknown stage '{other}' (expected collected, indexed, or compressed)"),
    }
}

/// Parse a `--older-than` value like `90d` into a day count.
fn parse_days(spec: &str) -> Result<u64> {
    let trimmed = spec.trim().trim_end_matches('d');
    let days = trimmed.parse::<u64>().map_err(|_| {
        anyhow::anyhow!("invalid --older-than value '{spec}' (expected e.g. `90d`)")
    })?;
    // Same rejection `HarvestConfig::validate` applies to the config field.
    // Going through the flag must not be a way around it: `0d` means "older
    // than now", which evicts every archive — never what someone typing a
    // retention window intends.
    if days == 0 {
        bail!(
            "invalid --older-than value '{spec}': 0 is ambiguous — it would evict every \
archive immediately. Use `--max-bytes 0 --apply` if you really mean to drop them all."
        );
    }
    if days > 3650 {
        bail!("invalid --older-than value '{spec}': must be <= 3650 days");
    }
    Ok(days)
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Find the one in-scope session whose id starts with `prefix`.
///
/// Already-harvested sessions are included: `show` on a session you have
/// reviewed before is a legitimate thing to want, and `mark` operates on
/// exactly those.
fn resolve_session(
    scope: &harvest::SessionScope,
    prefix: &str,
    all_projects: bool,
) -> Result<engramdb::storage::transcripts::SessionSummary> {
    let params = harvest::SelectParams {
        include_harvested: true,
        all_projects,
        ..Default::default()
    };
    let sessions = harvest::select_sessions(scope, &scope.root_dir, &params)?;

    let mut matches: Vec<_> = sessions
        .into_iter()
        .filter(|s| s.summary.session_id.starts_with(prefix))
        .map(|s| s.summary)
        .collect();

    match matches.len() {
        0 => bail!(
            "No session matching '{prefix}' in this project's scope — the root of its \
hierarchy plus every registered worktree and linked sub-project. Run \
`engramdb harvest list` to see what is available, or pass --all-projects. If Claude Code \
has already pruned the transcript the session is gone from that list but may still be \
archived: `engramdb harvest ledger list` finds it and `engramdb harvest show` digests it \
straight from the archive."
        ),
        1 => Ok(matches.remove(0)),
        n => bail!(
            "Ambiguous session id '{}' — matches {} sessions: {}",
            prefix,
            n,
            matches
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engramdb::storage::harvest_state::ClearOutcome;

    /// `--format json` serialized the transcript-derived strings verbatim.
    /// `serde_json` escapes C0, which hid the problem for control characters,
    /// but bidi overrides and the rest of the invisible set are ordinary
    /// Unicode to it and reached the consumer intact.
    #[test]
    fn json_rows_sanitize_transcript_derived_strings() {
        let hostile = "look\u{202e}gnp.exe\u{200b} here\nsecond line";
        let selected = harvest::SelectedSession {
            summary: engramdb::storage::transcripts::SessionSummary {
                session_id: "abc123".into(),
                transcript_path: PathBuf::from("/tmp/abc123.jsonl"),
                cwd: Some(format!("/repo/{hostile}")),
                git_branch: Some(hostile.to_string()),
                started_at: None,
                ended_at: None,
                user_turns: 1,
                assistant_turns: 1,
                bytes: 10,
                first_prompt: Some(hostile.to_string()),
                skipped_records: 0,
            },
            already_harvested: false,
        };

        let row = harvest_row(&selected);
        let json = serde_json::to_string(&row).unwrap();
        for (label, value) in [
            ("cwd", &row.cwd),
            ("git_branch", &row.git_branch),
            ("first_prompt", &row.first_prompt),
        ] {
            let value = value.as_deref().unwrap();
            assert!(
                !value.contains('\u{202e}') && !value.contains('\u{200b}'),
                "{label} kept an invisible character: {value:?}"
            );
            assert!(!value.contains('\n'), "{label} kept a newline: {value:?}");
        }
        // The escaping encoder would have hidden a raw newline; the invisible
        // characters it would not have.
        assert!(!json.contains('\u{202e}'), "{json}");
        assert!(
            json.contains("look"),
            "the visible text must survive: {json}"
        );
    }

    /// Control for the behavioral fix in `harvest_state::clear_harvested`:
    /// the two outcomes must not share one message, because the promise that
    /// used to be made unconditionally is only true in one of them.
    #[test]
    fn reset_reports_what_it_actually_did() {
        let kept = describe_reset("s1", ClearOutcome::ResetToUnreviewed);
        assert!(kept.contains("archived transcript is kept"), "{kept}");
        assert!(kept.contains("harvest show"), "{kept}");

        let removed = describe_reset("s1", ClearOutcome::Removed);
        assert!(removed.contains("No archived transcript"), "{removed}");
        assert!(
            removed.contains("only while Claude Code still has the live one"),
            "the unconditional promise is false for a pruned session: {removed}"
        );
        assert_ne!(kept, removed);
    }

    /// `--from-file` reads a path; the positional argument is only one of
    /// three sources, and none of them may be silently defaulted.
    #[test]
    fn summary_text_comes_from_an_argument_or_a_file() {
        use crate::prompter::MockPrompter;
        let prompter = MockPrompter::new(vec![]);

        assert_eq!(
            read_summary_text(Some("inline".into()), false, None, &prompter).unwrap(),
            "inline"
        );

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "from a file\n").unwrap();
        assert_eq!(
            read_summary_text(None, false, Some(tmp.path().to_path_buf()), &prompter).unwrap(),
            "from a file\n"
        );

        // With no source at all the caller is told the three that exist,
        // rather than getting an empty summary that silently clears the row.
        let err = read_summary_text(None, false, None, &prompter)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--editor") && err.contains("--from-file"),
            "{err}"
        );
    }

    fn hit(first_prompt: &str, complete: bool) -> ConversationHit {
        ConversationHit {
            session_id: "abcdef123456".into(),
            project_id: "p".into(),
            cwd: None,
            git_branch: None,
            started_at: None,
            ended_at: None,
            first_prompt: Some(first_prompt.to_string()),
            summary: None,
            indexed_complete: complete,
            score: 0.9,
            matched_on: MatchedOn::Digest,
        }
    }

    /// A run that matched nothing must say why a session might be missing.
    /// "No results" alone reads as "we never discussed it", which is exactly
    /// the wrong conclusion for a session that was simply never indexed.
    #[test]
    fn an_empty_search_explains_how_sessions_become_searchable() {
        let out = render_search_hits(&[]);
        assert!(out.contains("harvest index --all"), "{out}");
        assert!(out.contains("index_after_hours"), "{out}");
    }

    /// A partial row is a session whose tail was never embedded, so a miss
    /// against it is not evidence the topic was absent.
    #[test]
    fn a_partially_indexed_hit_says_so() {
        let partial = render_search_hits(&[hit("why is the build failing", false)]);
        assert!(partial.contains("partial"), "{partial}");

        let whole = render_search_hits(&[hit("why is the build failing", true)]);
        assert!(
            !whole.contains("partial"),
            "the notice must not fire on a whole session: {whole}"
        );
    }

    /// Search hits carry transcript-derived strings into a terminal.
    #[test]
    fn search_hits_sanitize_transcript_derived_strings() {
        let out = render_search_hits(&[hit("look\u{202e}gnp.exe here\nsecond line", true)]);
        assert!(!out.contains('\u{202e}'), "{out:?}");
        assert!(
            !out.contains("here\nsecond"),
            "a forged extra row survived: {out:?}"
        );
        assert!(
            out.contains("look"),
            "the visible text must survive: {out:?}"
        );
    }
}
