//! Handler for the `engramdb harvest` subcommand.
//!
//! Supplies `/engram:harvest` with its raw material. The binary's job stops
//! at *presenting* past sessions — deciding what is worth remembering is the
//! agent's, and saving is the user's. Nothing here writes a memory.

use crate::app::{HarvestCommand, LedgerCommand};
use crate::output::{HarvestSessionOutput, OutputFormatter};
use crate::prompter::Prompter;
use anyhow::{bail, Result};
use engramdb::ops::harvest;
use engramdb::storage::harvest_state::{self, HarvestDecision, HarvestEntry};
use engramdb::storage::transcripts::{self, ParseOptions};
use engramdb::storage::{transcript_archive, RegistryBackend};
use engramdb::types::HarvestConfig;
use std::path::{Path, PathBuf};

/// Run the `harvest` command.
pub async fn run_harvest(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: HarvestCommand,
    config: &HarvestConfig,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
) -> Result<()> {
    match command {
        HarvestCommand::List {
            since,
            limit,
            include_harvested,
            include_empty,
            all_projects,
            exclude_session,
        } => {
            let scope = harvest::session_scope(dir, registry).await?;
            let params = harvest::SelectParams {
                since: since.as_deref().map(harvest::parse_since).transpose()?,
                limit,
                exclude_session,
                include_harvested,
                all_projects,
                skip_empty: !include_empty,
            };
            let sessions = harvest::select_sessions(&scope, dir, &params)?;

            let output: Vec<HarvestSessionOutput> = sessions
                .iter()
                .map(|s| HarvestSessionOutput {
                    session_id: s.summary.session_id.clone(),
                    cwd: s.summary.cwd.clone(),
                    git_branch: s.summary.git_branch.clone(),
                    started_at: s.summary.started_at,
                    ended_at: s.summary.ended_at,
                    user_turns: s.summary.user_turns,
                    assistant_turns: s.summary.assistant_turns,
                    bytes: s.summary.bytes,
                    first_prompt: s.summary.first_prompt.clone(),
                    already_harvested: s.already_harvested,
                })
                .collect();
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
                match resolve_session(dir, registry, &session_id, all_projects).await {
                    Ok(selected) => (selected.transcript_path, None),
                    Err(live_err) => {
                        match restore_archived_session(dir, registry, &session_id).await? {
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
                    include_tools: !no_tools,
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
        } => {
            // `mark` must reach every session `show` can, or the ledger
            // silently re-offers one forever. That now includes sessions whose
            // live transcript is gone and which `show` reads from an archive,
            // so fall back to the ledger exactly as `reset` does.
            let resolved = match resolve_session(dir, registry, &session_id, all_projects).await {
                Ok(selected) => selected.session_id,
                Err(live_err) => resolve_ledger_key(dir, &session_id).map_err(|_| live_err)?,
            };
            let decision = if defer {
                HarvestDecision::Deferred
            } else if memory_ids.is_empty() {
                HarvestDecision::Skipped
            } else {
                HarvestDecision::Harvested
            };
            let entry = harvest_state::mark_harvested(dir, &resolved, &memory_ids, decision, note)?;
            if formatter.is_json() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&entry_json(&resolved, &entry))?
                );
            } else {
                formatter.print_success(&describe_mark(&resolved, &entry));
            }
        }

        HarvestCommand::Reset { session_id } => {
            let resolved = resolve_ledger_key(dir, &session_id)?;
            harvest_state::clear_harvested(dir, &resolved)?;
            formatter.print_success(&format!(
                "Cleared harvest record for session {resolved}; it will be offered again."
            ));
        }

        HarvestCommand::Ledger { command } => {
            run_ledger(dir, registry, command, config, formatter, prompter).await?;
        }
    }
    Ok(())
}

/// Human-readable one-liner for a recorded decision.
fn describe_mark(session_id: &str, entry: &HarvestEntry) -> String {
    match entry.decision() {
        HarvestDecision::Deferred => {
            format!("Deferred session {session_id}; it will keep appearing in `harvest list`.")
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

fn entry_json(session_id: &str, entry: &HarvestEntry) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "decision": entry.decision(),
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

async fn run_ledger(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: LedgerCommand,
    config: &HarvestConfig,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
) -> Result<()> {
    match command {
        LedgerCommand::List {
            decision,
            with_archive,
        } => {
            let wanted = decision.as_deref().map(parse_decision).transpose()?;
            let ledger = harvest_state::read_harvested(dir);
            let mut rows: Vec<(String, HarvestEntry)> = ledger
                .into_iter()
                .filter(|(_, e)| wanted.is_none_or(|w| e.decision() == w))
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
                        "{}  {:?}  {}  {} memor{}{}",
                        crate::output::short_id(id),
                        e.decision(),
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
            // `resolve_ledger_key` read the ledger separately and unlocked, so
            // a concurrent SessionEnd hook or `harvest reset` can drop the key
            // in between. Indexing a `HashMap` would panic on that race.
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
            let project_id = archive_project_id(dir, registry).await?;
            // The ledger can outlive the file: eviction on another machine, a
            // restored backup, or a manual cleanup all strand the reference.
            // Say so plainly rather than surfacing a bare "no such file".
            if !transcript_archive::archive_path(&project_id, &key)?.exists() {
                bail!(
                    "Session {key} has a recorded archive ({}) but the file is gone — it was \
most likely evicted by `harvest ledger prune` or the `[harvest] archive_*` budgets.",
                    archive.file_name
                );
            }
            let dest = output.unwrap_or_else(|| PathBuf::from(format!("{key}.jsonl")));
            let sha = transcript_archive::export_archive(&project_id, &key, &dest)?;
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
            let project_id = archive_project_id(dir, registry).await?;
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

            let removed_archive = transcript_archive::remove_archive(&project_id, &key)?;
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
            let project_id = archive_project_id(dir, registry).await?;
            let outcome = transcript_archive::prune_archives(&project_id, retention, cap, !apply)?;
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

/// Project id whose archive directory holds this project's transcripts.
///
/// Archives are keyed by the **root** project, matching the ledger, so a
/// worktree and its main checkout share one archive directory.
async fn archive_project_id(dir: &Path, registry: &dyn RegistryBackend) -> Result<String> {
    Ok(harvest::session_scope(dir, registry).await?.root_project_id)
}

fn parse_decision(value: &str) -> Result<HarvestDecision> {
    match value.to_ascii_lowercase().as_str() {
        "harvested" => Ok(HarvestDecision::Harvested),
        "skipped" => Ok(HarvestDecision::Skipped),
        "deferred" => Ok(HarvestDecision::Deferred),
        other => bail!("unknown decision '{other}' (expected harvested, skipped, or deferred)"),
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

/// Resolve a session-id prefix against the **ledger**, not the transcripts.
///
/// A session whose transcript has since been pruned by Claude Code must still
/// be manageable — that is the whole reason its archive exists.
fn resolve_ledger_key(dir: &Path, prefix: &str) -> Result<String> {
    let ledger = harvest_state::read_harvested(dir);
    let matches: Vec<&String> = ledger.keys().filter(|id| id.starts_with(prefix)).collect();
    match matches.as_slice() {
        [] => bail!("No harvest record matching '{prefix}'"),
        [one] => Ok((*one).clone()),
        many => bail!(
            "Ambiguous session id '{}' — matches {} records: {}",
            prefix,
            many.len(),
            many.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
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

/// Decompress an archived session to a temp file so it can be digested.
///
/// Returns `None` when no archive is held for `prefix`, leaving the caller to
/// report the original "no such live session" error. This is what makes the
/// archive worth taking: once Claude Code prunes a transcript, this is the
/// only remaining path from a session id to its conversation, and reading the
/// raw `.jsonl` by hand is exactly what the harvest docs tell agents never to
/// do (it is ~99% tool payload).
async fn restore_archived_session(
    dir: &Path,
    registry: &dyn RegistryBackend,
    prefix: &str,
) -> Result<Option<(tempfile::TempDir, PathBuf)>> {
    let Ok(key) = resolve_ledger_key(dir, prefix) else {
        return Ok(None);
    };
    let ledger = harvest_state::read_harvested(dir);
    let Some(archive) = ledger.get(&key).and_then(|e| e.archive.clone()) else {
        return Ok(None);
    };
    let project_id = archive_project_id(dir, registry).await?;
    if !transcript_archive::archive_path(&project_id, &key)?.exists() {
        return Ok(None);
    }

    // Restore under the session's real name, not a random temp name:
    // `parse_session` derives `session_id` from the file stem, so a
    // `NamedTempFile` would head the digest with `.tmpAbC123` and hand an
    // agent a session id that does not exist.
    let tmp = tempfile::TempDir::new()?;
    let restored = tmp.path().join(format!("{key}.jsonl"));
    let sha = transcript_archive::export_archive(&project_id, &key, &restored)?;
    if sha != archive.sha256 {
        bail!(
            "Archive for session {key} does not match the checksum recorded when it was \
written — the archive is corrupt. `harvest ledger rm {key} --archive-only` will drop it."
        );
    }
    Ok(Some((tmp, restored)))
}

/// Find the one in-scope session whose id starts with `prefix`.
///
/// Already-harvested sessions are included: `show` on a session you have
/// reviewed before is a legitimate thing to want, and `mark` operates on
/// exactly those.
async fn resolve_session(
    dir: &Path,
    registry: &dyn RegistryBackend,
    prefix: &str,
    all_projects: bool,
) -> Result<engramdb::storage::transcripts::SessionSummary> {
    let scope = harvest::session_scope(dir, registry).await?;
    let params = harvest::SelectParams {
        include_harvested: true,
        all_projects,
        ..Default::default()
    };
    let sessions = harvest::select_sessions(&scope, dir, &params)?;

    let mut matches: Vec<_> = sessions
        .into_iter()
        .filter(|s| s.summary.session_id.starts_with(prefix))
        .map(|s| s.summary)
        .collect();

    match matches.len() {
        0 => bail!(
            "No session matching '{prefix}' in this project or its sub-projects. \
Run `engramdb harvest list` to see what is available, or pass --all-projects."
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
