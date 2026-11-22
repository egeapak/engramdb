//! The per-event hook handlers (`process_*`) and the event-input helpers
//! they share.

use super::*;

/// Extract file_path from hook input JSON.
///
/// Returns `None` if the JSON is invalid or has no `tool_input.file_path`.
pub(super) fn extract_file_path(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    value
        .get("tool_input")
        .and_then(|ti| ti.get("file_path"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Suppress task-scoped memories from hook injection (§8.3): entries with
/// `generality == Task` are dropped unless the session's declared task
/// (§11.1 mapping) matches their `origin_task`. Absent a mapping,
/// task-scoped memories are suppressed from hooks but remain reachable by
/// explicit query.
///
/// Recorded deviation from §8.3: suppression runs on the MATERIALIZED
/// result (after the engine's `max_results` truncation), not at the index
/// level. Consequence: a top-k dominated by foreign task-scoped memories
/// yields fewer injected entries rather than backfilling project-wide ones.
/// Accepted because hooks request small k (5/10) against the same index the
/// engine already filtered, and the §16.4 hint tells the agent exactly how
/// to recover the hidden ones.
pub(super) fn suppress_task_scoped(
    memories: Vec<ScoredMemory>,
    current_task: Option<&str>,
) -> Vec<ScoredMemory> {
    memories
        .into_iter()
        .filter(|sm| {
            sm.memory.valid_while.as_ref().is_none_or(|v| {
                v.generality != Generality::Task
                    || (current_task.is_some() && v.origin_task.as_deref() == current_task)
            })
        })
        .collect()
}

/// The session's declared task, resolved from the hook event's session id
/// and the §11.1 mapping. Falls back to the freshest mapping from ANY
/// session when this session id has no entry: the MCP `task_current` tool
/// records the mapping under the server process's own session id, which
/// never matches the id in a hook event, so without the fallback the
/// hook-taught "declare task_current to surface yours" flow would be a
/// no-op under the default plugin install. `None` when the event carries no
/// session id or nothing fresh is mapped.
fn session_task_for(input: &str, dir: &Path) -> Option<String> {
    let session_id = extract_session_id(input)?;
    engramdb::storage::task_state::current_task_or_recent(dir, &session_id)
}

/// UserPromptSubmit injects this many memories, from a pool of up to
/// `PROMPT_POOL` so that memories this session already has can be replaced.
const PROMPT_MAX_RESULTS: usize = 5;
const PROMPT_POOL: usize = 10;

/// PreToolUse injects this many memories, chosen from a gated pool of up to
/// `PRE_TOOL_USE_POOL` by nearest enclosing scope.
pub(super) const PRE_TOOL_USE_MAX_RESULTS: usize = 5;
const PRE_TOOL_USE_POOL: usize = 25;

pub(super) const MEMORY_FILE_NOTE_PREFIX: &str = "[EngramDB] This is an EngramDB memory file";
const MEMORY_FILE_NOTE: &str = "`get` with this id returns the same content, and `query` finds \
related memories. To change it, use `update` or `challenge` rather than editing the file, so the \
index and its vectors stay current.";

/// The memory id, when `relative_path` is a shared memory file
/// (`.engramdb/memories/<slug>_<id>.md`) under the project.
///
/// Agents read these files directly when a query comes back empty; the note
/// points them back to the tools. Only an id that passes the same allowlist as
/// injected ids is returned, and the read itself is never blocked.
pub(super) fn memory_file_id(relative_path: &str) -> Option<String> {
    let path = Path::new(relative_path);
    let parent: Vec<_> = path.parent()?.components().map(|c| c.as_os_str()).collect();
    if parent
        != [
            std::ffi::OsStr::new(".engramdb"),
            std::ffi::OsStr::new("memories"),
        ]
    {
        return None;
    }
    if path.extension()? != "md" {
        return None;
    }
    let id = engramdb::storage::memory_file::extract_id_from_stem(path.file_stem()?.to_str()?);
    let looks_like_id = id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    looks_like_id.then(|| id.to_string())
}

/// Core hook logic: given input JSON, project dir, and store, retrieve and format.
///
/// Returns `Ok(Some(json))` if memories were found, `Ok(None)` if nothing to output.
#[cfg(test)]
pub(super) async fn process_hook_input(
    input: &str,
    dir: &Path,
    store: MemoryStore,
) -> Result<Option<String>> {
    process_hook_input_in(input, dir, store, &UsageSink::Log).await
}

pub(super) async fn process_hook_input_in(
    input: &str,
    dir: &Path,
    store: MemoryStore,
    usage: &UsageSink<'_>,
) -> Result<Option<String>> {
    let file_path = match extract_file_path(input) {
        Some(fp) => fp,
        None => return Ok(None),
    };

    let relative_path = relativize_path(&file_path, dir);

    if let Some(id) = memory_file_id(&relative_path) {
        let note = format!("{MEMORY_FILE_NOTE_PREFIX} (id: {id}). {MEMORY_FILE_NOTE}");
        return Ok(Some(build_hook_response("PreToolUse", &note)?));
    }

    let config_path = dir.join(".engramdb").join("config.toml");
    // No model providers: the query below has `query: None`, so retrieval is
    // scope_only and never embeds (see module docs).
    let engine = engramdb::ops::build_engine_without_providers(store, &config_path).await;
    // Read once: the engine skips loading these (they keep their place in
    // the pool, so the pool is what it would be without them), and the same
    // set drops them below.
    let seen = already_shown(input, dir);

    let query = RetrievalQuery {
        mode: RetrievalMode::Rank,
        // Hooks inject "relevant memories": never a weak fallback match.
        rank_fallback: Some(0),
        // Never expand links in a hook: an edges query per tool call.
        expand_links: Some(0),
        skip_load: (!seen.is_empty()).then(|| seen.clone()),
        path: Some(relative_path.clone()),
        logical: vec![],
        query: None,
        types: None,
        tags: None,
        min_criticality: None,
        // The whole gated pool: the cut to PRE_TOOL_USE_MAX_RESULTS is made
        // below by scope nearness, and a score-ordered top 5 has already
        // lost what it should keep.
        max_results: Some(PRE_TOOL_USE_POOL),
        include_expired: Some(false),
        // Content, not Summary: Summary makes the engine clear `content`, and
        // the body preview below then rendered as an empty line.
        detail_level: DetailLevel::Content,
        situation: Some(Situation::FileEdit),
        ..Default::default()
    };

    let result = match engramdb::ops::query_memories(&engine, &query).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Hook retrieval failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    // Task-scoped memories are suppressed from hook injection (§8.3) unless
    // the session declared their task. The rest are picked by nearest
    // enclosing scope: the file, its directory, then each ancestor;
    // sibling-file and root-wide scopes last. Stable, so score orders within
    // a depth. By score alone, a memory about another file in the same
    // directory (the same proximity as the directory itself) or a
    // high-criticality project-wide one took every slot from the directory's
    // own conventions. Declared loss: in a directory with 5+ scoped memories,
    // ancestor-scoped ones are not re-injected on each edit; SessionStart and
    // `query` still surface them.
    let current_task = session_task_for(input, dir);
    let memories = suppress_task_scoped(result.memories, current_task.as_deref());
    // The engine already left these out; kept as the authority.
    let mut memories = drop_seen(memories, &seen);
    memories.sort_by_key(|sm| {
        engramdb::scope::physical::enclosing_depth(&sm.memory.physical, &relative_path)
            .unwrap_or(usize::MAX)
    });
    memories.truncate(PRE_TOOL_USE_MAX_RESULTS);
    if memories.is_empty() {
        return Ok(None);
    }

    // §8.1/§8.2 class-grouped rendering (same formatter as SessionStart /
    // UserPromptSubmit): decisions carry their "— because {premise}" clause
    // atomically, facts show "revisit if" globs, groups get class headers,
    // and group_by_class puts hazards first among facts. `memories` stays in
    // selection order, which the body previews follow.
    let class_order = engine.config().hooks.class_order.clone();
    let budget = engine.config().hooks.prompt_context_budget;
    let context = format_class_context_with_budget(
        "[EngramDB] Relevant memories for this file:",
        &memories,
        budget,
        Some(Situation::FileEdit),
        class_order.as_deref(),
        engine.config().hooks.preview_chars,
    );
    let shown = record_shown(&memories, &context, input, dir);
    record_injected(
        &engine,
        &memories,
        &shown,
        input,
        Surface::HookPreTool,
        usage,
    );
    let json = build_hook_response("PreToolUse", &context)?;
    Ok(Some(json))
}

/// Core SessionStart logic: returns the JSON to print, or `None` if nothing
/// should be surfaced (store not initialized, no memories above threshold,
/// retrieval failed). Split from [`run_hook_session_start`] so the body is
/// unit-testable without stdout capture — mirrors how `process_hook_input`
/// backs `run_hook_pre_tool_use`.
#[cfg(test)]
pub(super) async fn process_session_start(
    dir: &Path,
    min_criticality: f64,
    input: &str,
) -> Result<Option<String>> {
    process_session_start_in(
        dir,
        min_criticality,
        input,
        &StoreSource::Fresh,
        &UsageSink::Log,
        None,
    )
    .await
}

/// Register an initialized project that the registry does not know, and
/// return the notice the agent should pass on to the user. Best-effort: a
/// failure is logged and costs only the notice.
pub(super) async fn registration_notice(
    dir: &Path,
    registry: Option<&dyn RegistryBackend>,
) -> Option<String> {
    let registry = registry?;
    match engramdb::ops::projects::ensure_registered(dir, registry).await {
        Ok(check) => {
            let notice = check.agent_notice()?;
            // A project that fell out of the registry may have had its data
            // directory swept, which leaves an empty index beside intact
            // memory files. The rebuild is too slow for a hook; say what to run.
            let empty = match MemoryStore::open(dir).await {
                Ok(store) => engramdb::ops::maintenance::empty_index_file_count(&store).await,
                Err(_) => None,
            };
            Some(match empty {
                Some(files) => format!(
                    "{notice} Its search index is empty although it has {files} memory \
                     file(s), so queries find nothing until the user runs `engramdb reindex`."
                ),
                None => notice,
            })
        }
        Err(e) => {
            tracing::warn!("engramdb hook: registry check failed (continuing): {e}");
            None
        }
    }
}

pub(super) async fn process_session_start_in(
    dir: &Path,
    min_criticality: f64,
    input: &str,
    stores: &StoreSource<'_>,
    usage: &UsageSink<'_>,
    registry: Option<&dyn RegistryBackend>,
) -> Result<Option<String>> {
    // Before the store opens, so the notice survives a failed open or query:
    // it reports a change already made to the registry.
    let notice = registration_notice(dir, registry).await;
    let notice_only = |notice: Option<String>| -> Result<Option<String>> {
        notice
            .map(|n| build_hook_response("SessionStart", &n))
            .transpose()
    };
    let store = match stores.open(dir).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Hook store open failed (non-fatal): {}", e);
            return notice_only(notice);
        }
    };

    let config_path = dir.join(".engramdb").join("config.toml");
    // No model providers: the query below has `query: None`, so retrieval is
    // scope_only and never embeds (see module docs).
    let engine = engramdb::ops::build_engine_without_providers(store, &config_path).await;

    let query = RetrievalQuery {
        mode: RetrievalMode::Rank,
        // Hooks inject "relevant memories": never a weak fallback match.
        rank_fallback: Some(0),
        // Never expand links in a hook: an edges query per tool call.
        expand_links: Some(0),
        path: None,
        logical: vec![],
        query: None,
        types: None,
        tags: None,
        min_criticality: Some(min_criticality),
        max_results: Some(10),
        include_expired: Some(false),
        // Content, not Summary: Summary makes the engine clear `content`, and
        // the body preview below then rendered as an empty line.
        detail_level: DetailLevel::Content,
        situation: Some(Situation::SessionStart),
        ..Default::default()
    };

    let result = match engramdb::ops::query_memories(&engine, &query).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Hook retrieval failed (non-fatal): {}", e);
            return notice_only(notice);
        }
    };

    // Always emit the reflection nudge, even when the store has no memories
    // yet — the agent should still be reminded to capture durable learnings.
    let current_task = session_task_for(input, dir);
    let before = result.memories.len();
    let memories = suppress_task_scoped(result.memories, current_task.as_deref());
    let suppressed = before - memories.len();
    let class_order = engine.config().hooks.class_order.clone();
    // §16.4 hint line: teach the task mechanism exactly when it becomes
    // relevant. Safe to advertise `task_current` — the tool ships in this
    // build (it is pinned into MCP_TOOL_SUFFIXES alongside this hint). Built
    // first so its length is reserved out of the 2000-char budget (§14.9).
    let hint = (suppressed > 0).then(|| {
        format!(
            "\n({suppressed} task-scoped memories hidden — declare task_current to surface yours.)"
        )
    });
    // The registration notice leads, and its length comes out of the same
    // budget as the hint's.
    let notice_prefix = notice.map(|n| format!("{n}\n\n"));
    let reserved = hint.as_ref().map(|h| h.len()).unwrap_or(0)
        + notice_prefix.as_ref().map(|n| n.len()).unwrap_or(0);
    let body = build_session_start_context_reserving(
        &memories,
        class_order.as_deref(),
        reserved,
        engine.config().hooks.preview_chars,
    );
    let mut context = notice_prefix.unwrap_or_default();
    context.push_str(&body);
    if let Some(hint) = hint {
        context.push_str(&hint);
    }
    // A new, cleared or compacted context holds nothing the earlier hooks
    // injected: start the record over from what this injection shows.
    forget_shown(input, dir);
    let shown = record_shown(&memories, &context, input, dir);
    record_injected(
        &engine,
        &memories,
        &shown,
        input,
        Surface::HookSessionStart,
        usage,
    );
    let json = build_hook_response("SessionStart", &context)?;
    Ok(Some(json))
}

// ---------------------------------------------------------------------------
// New hook events (§8.5)
// ---------------------------------------------------------------------------

/// Extract the `session_id` field every hook event carries.
pub(super) fn extract_session_id(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    value
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// The id a per-conversation record is kept under: the session id, plus the
/// subagent's `agent_id` when the event comes from a subagent. Claude Code
/// gives a subagent's events the parent's `session_id`, but the subagent's
/// context holds none of what the parent was shown, so the two must not
/// share a record.
pub(super) fn extract_context_id(input: &str) -> Option<String> {
    let session_id = extract_session_id(input)?;
    let agent_id = serde_json::from_str::<serde_json::Value>(input)
        .ok()?
        .get("agent_id")
        .and_then(|v| v.as_str())
        .filter(|a| !a.is_empty())
        .map(str::to_string);
    Some(match agent_id {
        Some(agent) => format!("{session_id}_{agent}"),
        None => session_id,
    })
}

/// Drop the memories this session's hooks already injected: the
/// conversation still holds them, and a repeat is re-read on every later
/// turn for nothing (see `engramdb::storage::hook_seen`).
fn drop_already_shown(memories: Vec<ScoredMemory>, input: &str, dir: &Path) -> Vec<ScoredMemory> {
    drop_seen(memories, &already_shown(input, dir))
}

/// The ids this session's hooks already injected (empty without a session).
fn already_shown(input: &str, dir: &Path) -> HashSet<String> {
    match extract_context_id(input) {
        Some(session_id) => engramdb::storage::hook_seen::seen_ids(dir, &session_id),
        None => HashSet::new(),
    }
}

fn drop_seen(memories: Vec<ScoredMemory>, seen: &HashSet<String>) -> Vec<ScoredMemory> {
    if seen.is_empty() {
        return memories;
    }
    memories
        .into_iter()
        .filter(|sm| !seen.contains(&sm.memory.id))
        .collect()
}

/// Record the memories `context` actually rendered (an entry the budget left
/// out was not shown, so a later hook may still show it), and return their
/// ids.
fn record_shown(memories: &[ScoredMemory], context: &str, input: &str, dir: &Path) -> Vec<String> {
    let ids: Vec<String> = memories
        .iter()
        .filter(|sm| context.contains(&format!("id: {}", sm.memory.id)))
        .map(|sm| sm.memory.id.clone())
        .collect();
    let Some(session_id) = extract_context_id(input) else {
        return ids;
    };
    if let Err(e) = engramdb::storage::hook_seen::mark_seen(dir, &session_id, &ids) {
        tracing::debug!("hook record write failed (non-fatal): {e}");
    }
    ids
}

/// Where a hook's `injected` usage events go.
pub enum UsageSink<'a> {
    /// Append to the store's JSONL log (a hook in its own process).
    Log,
    /// Hand them to the daemon, which buffers and flushes them with its hook
    /// events.
    Collect(&'a std::sync::Mutex<Vec<engramdb::ops::usage::MemoryEvent>>),
}

/// Record `injected` for the memories a hook rendered (`shown`, from
/// [`record_shown`]): only what fit the budget, never the candidate pool.
/// Hooks record no other read signal — a `returned` per hook would count the
/// whole pool on every Read or Edit. `rank` is the position in what was
/// shown. Gated on `[stats].enabled`.
fn record_injected(
    engine: &engramdb::retrieval::engine::RetrievalEngine,
    memories: &[ScoredMemory],
    shown: &[String],
    input: &str,
    surface: Surface,
    usage: &UsageSink<'_>,
) {
    if shown.is_empty() || !engine.config().stats.enabled {
        return;
    }
    let store = engine.store();
    let session = extract_session_id(input);
    let events: Vec<engramdb::ops::usage::MemoryEvent> = memories
        .iter()
        .filter(|sm| shown.contains(&sm.memory.id))
        .enumerate()
        .map(|(i, sm)| {
            engramdb::ops::usage::MemoryEvent::new(
                &sm.memory.id,
                &store.project_id,
                engramdb::ops::UsageKind::Injected,
                surface,
            )
            .with_session(session.as_deref())
            .with_rank(i + 1, sm.score)
        })
        .collect();
    match usage {
        UsageSink::Log => {
            engramdb::ops::usage::write_events(&engramdb::ops::StoreRef::of(store), &events)
        }
        UsageSink::Collect(buf) => buf.lock().unwrap_or_else(|e| e.into_inner()).extend(events),
    }
}

/// Forget what this session's hooks injected: its context was reset.
pub(super) fn forget_shown(input: &str, dir: &Path) {
    if let Some(session_id) = extract_context_id(input) {
        if let Err(e) = engramdb::storage::hook_seen::clear_seen(dir, &session_id) {
            tracing::debug!("hook record clear failed (non-fatal): {e}");
        }
    }
}

/// Extract the submitted prompt text from a UserPromptSubmit event.
pub(super) fn extract_prompt(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    value
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
}

/// Where the UserPromptSubmit hook gets an embedding model for the situation
/// classifier.
pub enum SituationModel {
    /// A shared daemon that is already running, else the embedding model
    /// loaded in this process (production). Never spawns a daemon.
    DaemonOrInProcess,
    /// The embedding model loaded in this process, never the daemon
    /// (`--in-process` / `ENGRAMDB_IN_PROCESS`).
    InProcess,
    /// Keyword tables only: the daemon when no embedding model resolves, and
    /// tests that must not reach a daemon or load a model on the developer's
    /// machine.
    KeywordOnly,
    /// A given provider: the daemon's own loaded model, or a test stub.
    Provider(std::sync::Arc<dyn engramdb::embeddings::EmbeddingProvider>),
    /// A provider and seeds already embedded with it (the daemon's
    /// [`SeedCache`]): only the prompt is embedded.
    Cached {
        provider: std::sync::Arc<dyn engramdb::embeddings::EmbeddingProvider>,
        classifier: std::sync::Arc<engramdb::prompt_intent::PrototypeClassifier>,
    },
}

/// The embedding model alone, loaded in this process — only when its ONNX
/// files are already cached ([`engramdb::ops::cached_onnx_embedding_provider`]).
/// Never downloads a model and never falls back to Ollama, either of which
/// could stall the prompt for seconds; `None` sends the hook to the keyword
/// tables instead.
async fn in_process_embedder(
    config: &engramdb::types::EngramConfig,
) -> Option<std::sync::Arc<dyn engramdb::embeddings::EmbeddingProvider>> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || engramdb::ops::cached_onnx_embedding_provider(&config))
        .await
        .ok()
        .flatten()
}

/// The prompt's situation: the embedding classifier when a model is at hand
/// and `[hooks].situation_classifier` is on, else the keyword tables. Every
/// failure falls back to the keyword tables; none reaches the caller.
pub(super) async fn prompt_situation(
    prompt: &str,
    config: &engramdb::types::EngramConfig,
    model: &SituationModel,
) -> Option<Situation> {
    use engramdb::prompt_intent::{keyword_situation, PrototypeClassifier, PrototypeParams};
    if !config.hooks.situation_classifier {
        return keyword_situation(prompt);
    }
    if let SituationModel::Cached {
        provider,
        classifier,
    } = model
    {
        match classifier.classify_with(provider.as_ref(), prompt).await {
            Ok(d) if d.needs_context => return d.situation,
            Ok(_) => return keyword_situation(prompt),
            // Another model than the seeds' (cannot happen through the
            // cache's key) or an embed failure: classify the old way.
            Err(e) => tracing::debug!("cached situation classifier refused: {e}"),
        }
    }
    let provider = match model {
        SituationModel::Cached { provider, .. } => Some(std::sync::Arc::clone(provider)),
        SituationModel::DaemonOrInProcess => {
            let cell = engramdb::daemon::DaemonCell::new();
            match engramdb::daemon::resolve_daemon_providers(&cell, config, None)
                .await
                .and_then(|p| p.embedding)
            {
                Some(p) => Some(p),
                None => in_process_embedder(config).await,
            }
        }
        SituationModel::InProcess => in_process_embedder(config).await,
        SituationModel::KeywordOnly => None,
        SituationModel::Provider(p) => Some(std::sync::Arc::clone(p)),
    };
    let Some(provider) = provider else {
        return keyword_situation(prompt);
    };
    // Seeds and prompt in one batch, nothing cached between calls.
    match PrototypeClassifier::classify_once(provider.as_ref(), PrototypeParams::default(), prompt)
        .await
    {
        // The hook searches every prompt whatever the gate says (see
        // `prompt_intent`'s module docs). A prompt the gate would skip
        // embeds near the acknowledgement seeds, where the classifier has no
        // situation evidence, so the keyword tables decide it: "ok, it still
        // panics" stays Debugging, "ok thanks" stays neutral.
        Ok(d) if d.needs_context => d.situation,
        Ok(_) => keyword_situation(prompt),
        Err(e) => {
            tracing::debug!("prompt situation classifier failed (non-fatal): {e}");
            keyword_situation(prompt)
        }
    }
}

/// The last `max_chars` characters of the agent's previous message, read from
/// the transcript the event names, starting at a word boundary. `None` when
/// disabled (`0`), when the event has no transcript, or when the transcript
/// holds no assistant text near its end.
fn previous_message_tail(input: &str, max_chars: usize) -> Option<String> {
    use engramdb::storage::transcripts::{last_assistant_text, LAST_ASSISTANT_TAIL_BYTES};
    if max_chars == 0 {
        return None;
    }
    let path = extract_transcript_path(input)?;
    let text = last_assistant_text(Path::new(&path), LAST_ASSISTANT_TAIL_BYTES)?;
    Some(text_tail(&text, max_chars).to_string())
}

/// The last `max_chars` characters of `text`; when that cuts a word, the
/// partial word is dropped.
pub(super) fn text_tail(text: &str, max_chars: usize) -> &str {
    let count = text.chars().count();
    if count <= max_chars {
        return text.trim();
    }
    let start = text
        .char_indices()
        .nth(count - max_chars)
        .map_or(0, |(i, _)| i);
    let cut = &text[start..];
    let cut_mid_word = !text[..start].ends_with(char::is_whitespace);
    match cut.find(char::is_whitespace) {
        Some(i) if cut_mid_word => cut[i..].trim(),
        _ => cut.trim(),
    }
}

/// Core UserPromptSubmit logic (§8.5.1): Filter-mode query with the prompt as
/// query text plus inferred situation; injects top-k under
/// `[hooks].prompt_context_budget` with per-class rendering.
///
/// Every prompt is searched; there is no skip decision. When the prompt's own
/// matches leave room, a second Filter query on the tail of the agent's
/// previous message (`[hooks].previous_message_chars`) fills it, so a short
/// reply such as "push it" finds memories about what it refers to. The
/// prompt's matches always rank first.
///
/// Retrieval is keyword-only: the engine has no providers. The one model use
/// is the optional situation classifier, which uses a daemon that is already
/// running or else loads the embedding model in this process (see
/// [`SituationModel`]).
#[cfg(test)]
pub(super) async fn process_user_prompt_submit(input: &str, dir: &Path) -> Result<Option<String>> {
    process_user_prompt_submit_with(input, dir, &SituationModel::KeywordOnly).await
}

#[cfg(test)]
async fn process_user_prompt_submit_with(
    input: &str,
    dir: &Path,
    model: &SituationModel,
) -> Result<Option<String>> {
    process_user_prompt_submit_in(input, dir, model, &StoreSource::Fresh, &UsageSink::Log).await
}

pub(super) async fn process_user_prompt_submit_in(
    input: &str,
    dir: &Path,
    model: &SituationModel,
    stores: &StoreSource<'_>,
    usage: &UsageSink<'_>,
) -> Result<Option<String>> {
    let prompt = match extract_prompt(input) {
        Some(p) => p,
        None => return Ok(None),
    };

    let store = match stores.open(dir).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Hook store open failed (non-fatal): {}", e);
            return Ok(None);
        }
    };
    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = engramdb::ops::build_engine_without_providers(store, &config_path).await;

    let situation = prompt_situation(&prompt, engine.config(), model).await;
    let previous = previous_message_tail(input, engine.config().hooks.previous_message_chars);
    let query = RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some(prompt),
        // Room to replace memories this session already has (dropped below).
        max_results: Some(PROMPT_POOL),
        include_expired: Some(false),
        // Content, not Summary: Summary makes the engine clear `content`, and
        // the body preview below then rendered as an empty line.
        detail_level: DetailLevel::Content,
        situation,
        // Never expand links in a hook (latency budget is per prompt).
        expand_links: Some(0),
        ..Default::default()
    };

    let current_task = session_task_for(input, dir);
    // Task-scoped and already-shown memories are dropped from each result set
    // before the slots are counted, so a prompt whose matches were all shown
    // earlier in the session still gets the previous message's matches.
    let visible = |memories: Vec<ScoredMemory>| {
        drop_already_shown(
            suppress_task_scoped(memories, current_task.as_deref()),
            input,
            dir,
        )
    };
    let mut memories = match engramdb::ops::query_memories(&engine, &query).await {
        Ok(r) => visible(r.memories),
        Err(e) => {
            tracing::debug!("Hook retrieval failed (non-fatal): {}", e);
            return Ok(None);
        }
    };
    if let Some(previous) = previous.filter(|_| memories.len() < PROMPT_MAX_RESULTS) {
        let context_query = RetrievalQuery {
            query: Some(previous),
            ..query.clone()
        };
        match engramdb::ops::query_memories(&engine, &context_query).await {
            Ok(r) => {
                for m in visible(r.memories) {
                    if memories.len() >= PROMPT_MAX_RESULTS {
                        break;
                    }
                    if !memories.iter().any(|f| f.memory.id == m.memory.id) {
                        memories.push(m);
                    }
                }
            }
            Err(e) => tracing::debug!("Hook previous-message retrieval failed (non-fatal): {e}"),
        }
    }
    memories.truncate(PROMPT_MAX_RESULTS);
    if memories.is_empty() {
        return Ok(None);
    }

    let budget = engine.config().hooks.prompt_context_budget;
    let class_order = engine.config().hooks.class_order.clone();
    let context = format_class_context_with_budget(
        "[EngramDB] Memories relevant to this prompt:",
        &memories,
        budget,
        situation,
        class_order.as_deref(),
        engine.config().hooks.preview_chars,
    );
    let shown = record_shown(&memories, &context, input, dir);
    record_injected(
        &engine,
        &memories,
        &shown,
        input,
        Surface::HookPrompt,
        usage,
    );
    let json = build_hook_response("UserPromptSubmit", &context)?;
    Ok(Some(json))
}

/// Core PostToolUse logic (§8.5.2): after a file mutation, match the edited
/// path against the `watch_paths` index column, restricted to currently-valid
/// memories (this hook bypasses the query path, so it applies the §2.4
/// default exclusion itself — an invalidated memory must not keep warning).
/// Index-only: no memory files are loaded.
#[cfg(test)]
pub(super) async fn process_post_tool_use(input: &str, dir: &Path) -> Result<Option<String>> {
    process_post_tool_use_in(input, dir, &StoreSource::Fresh).await
}

pub(super) async fn process_post_tool_use_in(
    input: &str,
    dir: &Path,
    stores: &StoreSource<'_>,
) -> Result<Option<String>> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(input) {
        if value.get("tool_name").and_then(|v| v.as_str()) == Some("Bash") {
            return Ok(process_bash_retry(&value, dir));
        }
        // An edit between a failed command and its next success means the fix
        // was a code change (the normal red-green loop), not an undocumented
        // cause worth saving: settle the pending failures.
        if let Some(context_id) = extract_context_id(input) {
            if let Err(e) = engramdb::storage::bash_retry::settle_pending(dir, &context_id) {
                tracing::debug!("retry record settle failed (non-fatal): {e}");
            }
        }
    }
    let file_path = match extract_file_path(input) {
        Some(fp) => fp,
        None => return Ok(None),
    };
    if let Some(name) = auto_memory_note_name(input, &file_path, dir) {
        let context = format!("{AUTO_MEMORY_NOTE_PREFIX} ({name}). {AUTO_MEMORY_NOTE}");
        return Ok(Some(build_hook_response("PostToolUse", &context)?));
    }
    let relative_path = relativize_path(&file_path, dir);
    if relative_path.is_empty() {
        return Ok(None);
    }

    let store = match stores.open(dir).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Hook store open failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    // Three columns of the rows that watch anything, not the whole filtering
    // projection: this runs on every edit.
    let entries = match store.list_watched().await {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("Hook index read failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    let now = chrono::Utc::now();
    let matched_ids: Vec<&str> = entries
        .iter()
        .filter(|entry| {
            // §2.4 validity guard: closed windows never warn (§14.14). The
            // projection's pushdown is a narrowing; this filter is the gate.
            entry.invalidated_at.is_none_or(|t| t > now)
                && !entry.watch_paths.is_empty()
                && engramdb::scope::physical::matches(&entry.watch_paths, &relative_path)
        })
        .map(|entry| entry.id.as_str())
        .collect();
    if matched_ids.is_empty() {
        // The common case: one index scan, zero file loads, no output.
        return Ok(None);
    }

    // Load ONLY the matched memories (rare, bounded) for their summaries.
    let matched = store.get_batch(&matched_ids).await.unwrap_or_default();
    let warnings: Vec<String> = matched
        .iter()
        .map(|(id, m)| {
            format!(
                "⚠ this edit may invalidate memory {} ('{}') — verify it or update/invalidate it",
                crate::output::short_id(id),
                m.summary
            )
        })
        .collect();
    if warnings.is_empty() {
        return Ok(None);
    }
    let context = format!("[EngramDB]\n{}", warnings.join("\n"));
    let json = build_hook_response("PostToolUse", &context)?;
    Ok(Some(json))
}

/// The SessionEnd hook (§8.5.3): housekeeping only, no context
/// output, must never block session teardown. Clears the session→task
/// mapping (§11.1) and, when `[epistemic].demote_on_session_end` is set and
/// a mapping existed, runs the §11.2 demotion for that task. Best-effort:
/// every failure is logged and swallowed. (Telemetry flushes live in the
/// long-running MCP/daemon processes; a one-shot hook has none.)
async fn session_end(
    input: &str,
    dir: &Path,
    registry: &dyn RegistryBackend,
    stores: &StoreSource<'_>,
) -> Result<()> {
    let Some(session_id) = extract_session_id(input) else {
        return Ok(());
    };

    // Every other hook handler bails when the project was never `init`ed (see
    // `run_hook_pre_tool_use`); SessionEnd must too, and more so, because it
    // *writes*. Key off `manifest.toml` rather than `.engramdb/`: the latter
    // is created by this very hook's own state writes, so it would
    // self-satisfy on the second run. The plugin registers SessionEnd
    // machine-wide, so without this every directory Claude Code is ever
    // started in collects a `state/` tree and a permanent transcript archive.
    // `manifest.toml` OR `memories/`: `MemoryStore` self-heals a missing
    // manifest (it recreates one on open), so keying on the manifest alone
    // would be stricter than every other handler and would silently disable
    // SessionEnd for a store that is live everywhere else.
    let project_dir = engramdb::storage::paths::project_dir(dir);
    if !project_dir.join("manifest.toml").exists() && !project_dir.join("memories").is_dir() {
        return Ok(());
    }

    forget_shown(input, dir);
    engramdb::storage::hook_seen::prune_stale(dir);
    if let Err(e) = engramdb::storage::bash_retry::clear(dir, &session_id) {
        tracing::debug!("SessionEnd retry record clear failed (non-fatal): {e}");
    }
    engramdb::storage::bash_retry::prune_stale(dir);
    let ended_task = match engramdb::storage::task_state::clear_session_task(dir, &session_id) {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!("SessionEnd mapping clear failed (non-fatal): {e}");
            None
        }
    };

    if let Some(task) = ended_task {
        let config_path = dir.join(".engramdb").join("config.toml");
        let config = engramdb::storage::config::load_config_or_default(&config_path).await;
        if config.epistemic.demote_on_session_end {
            match stores.open(dir).await {
                Ok(store) => {
                    if let Err(e) =
                        engramdb::ops::task_complete(&store, &task, &config.epistemic).await
                    {
                        tracing::debug!("SessionEnd demotion failed (non-fatal): {e}");
                    }
                }
                Err(e) => tracing::debug!("SessionEnd store open failed (non-fatal): {e}"),
            }
        }
    }

    // Last, deliberately: archiving is the slowest thing on this path
    // (compression is proportional to transcript size), and if the hook is
    // killed by a timeout the task-mapping clear above must already have
    // happened. Bounded by `archive_max_transcript_bytes` as well.
    engramdb::ops::archive_session_end(
        dir,
        &session_id,
        extract_transcript_path(input).map(std::path::PathBuf::from),
        registry,
    )
    .await;
    Ok(())
}

pub(super) const AUTO_MEMORY_NOTE_PREFIX: &str =
    "[EngramDB] You saved this to Claude Code's auto-memory";
const AUTO_MEMORY_NOTE: &str = "Auto-memory is private to this machine and invisible to EngramDB \
lookups and to collaborators. If it is a project fact, convention, hazard or decision, also record \
it in EngramDB so every session and teammate gets it; keep auto-memory for personal preferences.";

/// The file name, when this edit wrote a note into Claude Code's auto-memory.
///
/// Claude Code keeps auto-memory in `memory/` beside the session transcript
/// (`<config>/projects/<encoded-cwd>/memory/*.md`, indexed by `MEMORY.md`).
/// Its system prompt tells the model to save there whenever the user says
/// "remember", so project knowledge lands where EngramDB never sees it; the
/// memory-tool-use eval measured that in 16 of 24 Sonnet runs. Only a
/// project with an EngramDB store gets the note, the index file itself is
/// skipped, and paths are compared canonicalized, never textually.
fn auto_memory_note_name(input: &str, file_path: &str, dir: &Path) -> Option<String> {
    if !dir.join(".engramdb").is_dir() {
        return None;
    }
    let transcript = extract_transcript_path(input)?;
    let memory_dir = std::fs::canonicalize(Path::new(&transcript).parent()?.join("memory")).ok()?;
    let file = std::fs::canonicalize(file_path).ok()?;
    let name = file.file_name()?.to_str()?.to_string();
    (file.starts_with(&memory_dir) && name.ends_with(".md") && name != "MEMORY.md").then_some(name)
}

/// Extract `transcript_path`, which every hook event carries.
fn extract_transcript_path(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    value
        .get("transcript_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Static PreCompact reminder (§8.5.4): context loss is a memory system's
/// worst enemy; intercept right before compaction.
const PRE_COMPACT_REMINDER: &str = "[EngramDB] Context is about to be compacted. Store durable \
discoveries first: decisions (with their premise), hazards, and verified observations via the \
memory create tool.";

/// The PreCompact hook (§8.5.4): inject a short static reminder.
/// Same additionalContext contract as the other hooks; if the runtime
/// ignores it for PreCompact events, the output is harmlessly dropped.
pub(super) fn pre_compact(input: &str, dir: &Path) -> Result<Option<String>> {
    // Compaction drops the injected memories from the context, so the hooks
    // may show them again.
    forget_shown(input, dir);
    Ok(Some(build_hook_response(
        "PreCompact",
        PRE_COMPACT_REMINDER,
    )?))
}

/// Run SessionEnd on its own thread, with its own single-threaded runtime.
///
/// Two reasons. Its future is not `Send` — rustc's higher-ranked `Send` check
/// fails somewhere inside the harvest helpers it calls — and the daemon needs
/// a `Send` future for every other hook. And it is the slow hook (it
/// compresses the transcript), which should not occupy a daemon worker thread
/// while other sessions' hooks wait. The cost is one thread per session end.
/// It always opens a fresh store and the global registry: the store is only
/// needed for the optional demotion, so a warm one buys nothing.
pub(super) async fn run_session_end_isolated(input: &str, dir: &Path) -> Result<()> {
    let input = input.to_string();
    let dir = dir.to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("engramdb-session-end".to_string())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                rt.block_on(async {
                    let registry = engramdb::storage::FileRegistry::global()?;
                    session_end(&input, &dir, &registry, &StoreSource::Fresh).await
                })
            })();
            let _ = tx.send(result);
        })?;
    rx.await
        .map_err(|_| anyhow::anyhow!("session-end thread ended without a result"))?
}
