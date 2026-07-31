//! Handler for the `engramdb harvest` subcommand.
//!
//! Supplies `/engram:harvest` with its raw material. The binary's job stops
//! at *presenting* past sessions — deciding what is worth remembering is the
//! agent's, and saving is the user's. Nothing here writes a memory.

use crate::app::HarvestCommand;
use crate::output::{HarvestSessionOutput, OutputFormatter};
use anyhow::{bail, Result};
use engramdb::ops::harvest;
use engramdb::storage::transcripts::ParseOptions;
use engramdb::storage::{harvest_state, RegistryBackend};
use std::path::Path;

/// Run the `harvest` command.
pub async fn run_harvest(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: HarvestCommand,
    formatter: &OutputFormatter,
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
            let selected = resolve_session(dir, registry, &session_id, all_projects).await?;
            let params = harvest::DigestParams {
                parse: ParseOptions {
                    include_thinking,
                    include_sidechains,
                    include_tools: !no_tools,
                },
                max_chars,
            };
            let digest = harvest::digest_session(&selected.transcript_path, params)?;
            let markdown = harvest::render_digest_markdown(&digest);

            if formatter.is_json() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session_id": digest.summary.session_id,
                        "cwd": digest.summary.cwd,
                        "git_branch": digest.summary.git_branch,
                        "started_at": digest.summary.started_at,
                        "ended_at": digest.summary.ended_at,
                        "user_turns": digest.summary.user_turns,
                        "assistant_turns": digest.summary.assistant_turns,
                        "complete": digest.is_complete(),
                        "dropped_classes": digest.dropped_classes,
                        "truncated_events": digest.truncated_events,
                        "events": digest.events,
                        "markdown": markdown,
                    }))?
                );
            } else {
                println!("{markdown}");
            }
        }

        HarvestCommand::Mark {
            session_id,
            memory_ids,
            all_projects,
        } => {
            // `--all-projects` must mirror `show`: a session the user was able
            // to digest has to be a session they can mark as reviewed, or the
            // ledger silently re-offers it forever.
            let selected = resolve_session(dir, registry, &session_id, all_projects).await?;
            let entry = harvest_state::mark_harvested(dir, &selected.session_id, &memory_ids)?;
            if formatter.is_json() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session_id": selected.session_id,
                        "harvested_at": entry.harvested_at,
                        "memories_created": entry.memories_created,
                        "memory_ids": entry.memory_ids,
                    }))?
                );
            } else if entry.memories_created == 0 {
                formatter.print_success(&format!(
                    "Marked session {} as reviewed (no memories saved).",
                    selected.session_id
                ));
            } else {
                formatter.print_success(&format!(
                    "Marked session {} as harvested ({} memor{} saved).",
                    selected.session_id,
                    entry.memories_created,
                    if entry.memories_created == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                ));
            }
        }

        HarvestCommand::Reset { session_id } => {
            // Match against the ledger, not the transcripts: a session whose
            // transcript has since been deleted must still be clearable.
            let ledger = harvest_state::read_harvested(dir);
            let matches: Vec<&String> = ledger
                .keys()
                .filter(|id| id.starts_with(&session_id))
                .collect();
            let resolved = match matches.as_slice() {
                [] => bail!("No harvest record matching '{session_id}'"),
                [one] => (*one).clone(),
                many => bail!(
                    "Ambiguous session id '{}' — matches {} records: {}",
                    session_id,
                    many.len(),
                    many.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            harvest_state::clear_harvested(dir, &resolved)?;
            formatter.print_success(&format!(
                "Cleared harvest record for session {resolved}; it will be offered again."
            ));
        }
    }
    Ok(())
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
