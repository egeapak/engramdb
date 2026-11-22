//! The tools' parameter structs (`*Input`), deserialized from a call's
//! arguments. Their JSON schemas are what `tools/list` serves; `schemars`
//! names each definition after the type, not this module.

use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Input parameter structs for tool aggregation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct CreateInput {
    #[serde(rename = "type")]
    #[schemars(
        description = "Memory type: decision, convention, hazard, context, intent, relationship, debug, or preference"
    )]
    pub(super) type_: String,

    #[schemars(
        description = "Core knowledge to store (max ~500 tokens). For decisions, state what was chosen, over what alternatives, and why."
    )]
    pub(super) content: String,

    #[schemars(
        description = "One-line summary, max 200 chars by default (configurable via [content].summary_max_chars) (required)"
    )]
    pub(super) summary: String,

    #[schemars(description = "Extended details (lazy-loaded)")]
    pub(super) details: Option<String>,

    #[schemars(description = "File paths this applies to (default [\"/\"])")]
    pub(super) physical: Option<Vec<String>>,

    #[schemars(description = "Logical scopes in dot notation")]
    pub(super) logical: Option<Vec<String>>,

    #[schemars(description = "Freeform tags")]
    pub(super) tags: Option<Vec<String>>,

    #[schemars(description = "Importance 0.0-1.0 (default 0.5)")]
    pub(super) criticality: Option<f64>,

    #[schemars(description = "Confidence 0.0-1.0 (default 0.8)")]
    pub(super) confidence: Option<f64>,

    #[schemars(description = "Visibility: shared|personal (default shared)")]
    pub(super) visibility: Option<String>,

    #[schemars(
        description = "IDs of memories this supersedes (full id or unique prefix; an id that matches no memory, or more than one, fails the call)"
    )]
    pub(super) supersedes: Option<Vec<String>>,

    #[schemars(
        description = "Typed edges to add from this memory (add semantics: existing edges are kept). Each: {to, kind, store?, confidence?, note?, status?}. kind: contradicts|supports|derived_from|related_to|duplicates (supersedes goes in `supersedes`)."
    )]
    pub(super) links: Option<Vec<LinkSpec>>,

    #[schemars(
        description = "Audience for a group/global share: the project ids and/or group ids the memory is scoped to in cross-store fan-in. Omit for whole-group visibility. Only meaningful when writing into a group (project: \"group:<name>\") or the global store; inert on a project-local memory. Advisory scoping, NOT a confidentiality boundary — a direct query of the shared store still returns it."
    )]
    pub(super) audience: Option<Vec<String>>,

    #[schemars(
        description = "Epistemic class: fact (structural, verifiable against the repo), observation (measured empirically, may go stale), decision (chosen over alternatives, valid while its premise holds). Defaults from type; set only when it differs."
    )]
    pub(super) epistemic: Option<String>,

    #[schemars(
        description = "Premise this memory depends on, e.g. 'while we pin ort rc.12'. State it if the memory becomes wrong when something specific changes."
    )]
    pub(super) premise: Option<String>,

    #[schemars(
        description = "Paths/globs whose change invalidates this memory (distinct from physical, which is where it applies)."
    )]
    pub(super) invalidated_by: Option<Vec<String>>,

    #[schemars(
        description = "Task or feature this was decided for (short human-readable name, not a session id)."
    )]
    pub(super) origin_task: Option<String>,

    #[schemars(description = "'project' (default) or 'task' (binding only within origin_task).")]
    pub(super) generality: Option<String>,

    #[schemars(
        description = "Valid-time start (RFC3339): when the claim became true in the world. Only to backdate; defaults to creation time."
    )]
    pub(super) valid_from: Option<String>,

    #[schemars(description = "Decay: none|linear|exponential|step")]
    pub(super) decay_strategy: Option<String>,

    #[schemars(description = "Half-life in seconds")]
    pub(super) decay_half_life: Option<u64>,

    #[schemars(description = "TTL in seconds")]
    pub(super) decay_ttl: Option<u64>,

    #[schemars(description = "Minimum decay factor (0.0-1.0)")]
    pub(super) decay_floor: Option<f64>,

    #[schemars(description = "Optional human-readable title for the memory file")]
    pub(super) title: Option<String>,

    #[schemars(
        description = "Title generation strategy: keyword|t5|none. Defaults to the project's [title].strategy config (t5 unless overridden)."
    )]
    pub(super) title_strategy: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct QueryInput {
    #[schemars(
        description = "Mode: \"rank\" for context-aware ranked results (browse); \"filter\" to require a positive signal (keyword, scope, tag, or semantic match). Required."
    )]
    pub(super) mode: String,

    #[schemars(description = "Search query text (tokenized against summary, content, tags)")]
    pub(super) query: Option<String>,

    #[schemars(
        description = "Physical scope — current file path for proximity scoring. Repo-relative or absolute (absolute paths under the project root are relativized automatically)."
    )]
    pub(super) path: Option<String>,

    #[schemars(
        description = "Logical scopes in dot notation — contributes to hierarchy-proximity scoring (not a filter)"
    )]
    pub(super) logical: Option<Vec<String>>,

    #[schemars(description = "Filter by memory types")]
    pub(super) types: Option<Vec<String>>,

    #[schemars(description = "Filter by tags (OR logic)")]
    pub(super) tags: Option<Vec<String>>,

    #[schemars(description = "Minimum criticality threshold")]
    pub(super) min_criticality: Option<f64>,

    #[schemars(description = "Maximum results (default 10)")]
    pub(super) max_results: Option<usize>,

    #[schemars(
        description = "Detail: summary|content|full (default content) — available in both modes"
    )]
    pub(super) detail_level: Option<String>,

    #[schemars(description = "Include expired/decayed memories")]
    pub(super) include_expired: Option<bool>,

    #[schemars(
        description = "Filter by epistemic class: fact, observation, decision (OR logic, like types)."
    )]
    pub(super) epistemic: Option<Vec<String>>,

    #[schemars(
        description = "Your current situation, to reweight classes: session_start, file_edit, debugging, design_choice. Declare debugging when investigating a failure (observations rank highest) and design_choice when weighing alternatives (prior decisions rank highest)."
    )]
    pub(super) situation: Option<String>,

    #[schemars(
        description = "Include memories whose validity window was closed (invalidated). Default false."
    )]
    pub(super) include_invalidated: Option<bool>,

    #[schemars(description = "Also include global memories in results (default false)")]
    pub(super) include_global: Option<bool>,

    #[schemars(
        description = "Add up to N one-hop neighbours of the top results, reached over confirmed supports / derived_from / related_to edges (0 = off, max 20; default from [retrieval].expand_links, normally 0). Each added result carries `via` {from, kind}; it is scored at its hit's score times expand_links_decay minus its own challenge/contradicts penalties (never above its hit), passes every filter a direct result would, and N is per query across every fanned-in store."
    )]
    pub(super) expand_links: Option<u32>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct GetInput {
    #[schemars(description = "Memory ID")]
    pub(super) id: String,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct UpdateInput {
    #[schemars(description = "Memory ID")]
    pub(super) id: String,

    #[serde(rename = "type")]
    #[schemars(description = "Memory type")]
    pub(super) type_: Option<String>,

    #[schemars(description = "Content")]
    pub(super) content: Option<String>,

    #[schemars(description = "Summary")]
    pub(super) summary: Option<String>,

    #[schemars(description = "Details")]
    pub(super) details: Option<String>,

    #[schemars(description = "Physical scopes")]
    pub(super) physical: Option<Vec<String>>,

    #[schemars(description = "Logical scopes")]
    pub(super) logical: Option<Vec<String>>,

    #[schemars(description = "Replace all tags")]
    pub(super) tags: Option<Vec<String>>,

    #[schemars(description = "Tags to add (merged with existing)")]
    pub(super) tags_add: Option<Vec<String>>,

    #[schemars(description = "Tags to remove")]
    pub(super) tags_remove: Option<Vec<String>>,

    #[schemars(description = "Criticality")]
    pub(super) criticality: Option<f64>,

    #[schemars(description = "Confidence")]
    pub(super) confidence: Option<f64>,

    #[schemars(description = "Visibility")]
    pub(super) visibility: Option<String>,

    #[schemars(description = "Human-readable title for the memory file")]
    pub(super) title: Option<String>,

    #[schemars(description = "Status: active|needsreview|challenged")]
    pub(super) status: Option<String>,

    #[schemars(
        description = "IDs of memories this supersedes (full id or unique prefix; an id that matches no memory, or more than one, fails the call)"
    )]
    pub(super) supersedes: Option<Vec<String>>,

    #[schemars(
        description = "Typed edges to add from this memory (add semantics — never replaces the existing set; use `unlink` to remove). Each: {to, kind, store?, confidence?, note?, status?}."
    )]
    pub(super) links: Option<Vec<LinkSpec>>,

    #[schemars(
        description = "Set the per-memory audience (project/group ids) for a group/global share. Omit to leave unchanged; pass an empty list to clear it to whole-group visibility. Only meaningful on a memory in a group/global store."
    )]
    pub(super) audience: Option<Vec<String>>,

    #[schemars(
        description = "Epistemic class: fact (structural, verifiable against the repo), observation (measured empirically, may go stale), decision (chosen over alternatives, valid while its premise holds). Defaults from type; set only when it differs."
    )]
    pub(super) epistemic: Option<String>,

    #[schemars(
        description = "Premise this memory depends on, e.g. 'while we pin ort rc.12'. State it if the memory becomes wrong when something specific changes."
    )]
    pub(super) premise: Option<String>,

    #[schemars(
        description = "Paths/globs whose change invalidates this memory (distinct from physical, which is where it applies)."
    )]
    pub(super) invalidated_by: Option<Vec<String>>,

    #[schemars(
        description = "Task or feature this was decided for (short human-readable name, not a session id)."
    )]
    pub(super) origin_task: Option<String>,

    #[schemars(description = "'project' (default) or 'task' (binding only within origin_task).")]
    pub(super) generality: Option<String>,

    #[schemars(
        description = "Valid-time start (RFC3339): when the claim became true in the world. Only to backdate; defaults to creation time."
    )]
    pub(super) valid_from: Option<String>,

    #[schemars(
        description = "Clear the whole validity condition (premise/invalidated_by/origin_task/generality)."
    )]
    pub(super) clear_validity: Option<bool>,

    #[schemars(
        description = "Reopen a closed validity window: clears invalidated_at and superseded_by. Invalidation is reversible, unlike deletion."
    )]
    pub(super) clear_invalidated: Option<bool>,

    #[schemars(description = "Decay: none|linear|exponential|step")]
    pub(super) decay_strategy: Option<String>,

    #[schemars(description = "Half-life in seconds")]
    pub(super) decay_half_life: Option<u64>,

    #[schemars(description = "TTL in seconds")]
    pub(super) decay_ttl: Option<u64>,

    #[schemars(description = "Minimum decay factor (0.0-1.0)")]
    pub(super) decay_floor: Option<f64>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct DeleteInput {
    #[schemars(description = "Memory ID")]
    pub(super) id: String,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ChallengeInput {
    #[schemars(description = "Memory ID")]
    pub(super) id: String,

    #[schemars(
        description = "Why the memory is wrong or outdated: the user's statement (\"we moved off structlog last week\"), or the line of a file you read that states the opposite. Either is enough evidence. Code that still matches the old memory does not make it current."
    )]
    pub(super) evidence: String,

    #[schemars(
        description = "Repo file that contradicts the memory, when the evidence came from one"
    )]
    pub(super) source_file: Option<String>,

    #[schemars(
        description = "Id of the memory that contradicts this one, when the conflict is between two memories. Also records a proposed `contradicts` edge between them, which a reviewer later confirms, rejects, or resolves as one superseding the other (`link_status`)."
    )]
    pub(super) against: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ReviewInput {
    #[schemars(description = "Filter to a scope")]
    pub(super) scope: Option<String>,

    #[schemars(description = "Max results (default 10)")]
    pub(super) max_results: Option<usize>,

    #[serde(rename = "type")]
    #[schemars(description = "Filter by memory type")]
    pub(super) type_: Option<String>,

    #[schemars(description = "Only show challenged memories")]
    pub(super) challenged_only: Option<bool>,

    #[schemars(description = "Only show needs-review memories")]
    pub(super) stale_only: Option<bool>,

    #[schemars(
        description = "Recency trigger: also surface active memories not updated in more than N days. Omit to use the project's [review].recency_days (default 90); set 0 or a very large value to effectively disable."
    )]
    pub(super) stale_after_days: Option<u64>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ResolveInput {
    #[schemars(description = "Memory ID")]
    pub(super) id: String,

    #[schemars(
        description = "Action: keep, update, delete, or invalidate. Prefer invalidate over delete when a memory WAS true but no longer is — history is kept and queryable via include_invalidated."
    )]
    pub(super) action: String,

    #[schemars(description = "New content (required for update)")]
    pub(super) updated_content: Option<String>,

    #[schemars(description = "New summary (optional for update)")]
    pub(super) updated_summary: Option<String>,

    #[schemars(
        description = "For invalidate: id of the memory that superseded this one (optional)."
    )]
    pub(super) superseded_by: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

/// One edge in a `create`/`update` `links` list.
#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct LinkSpec {
    #[schemars(description = "Target memory id (a unique prefix is resolved to the full id)")]
    pub(super) to: String,
    #[schemars(
        description = "contradicts | supports | derived_from | related_to | duplicates | supersedes"
    )]
    pub(super) kind: String,
    #[schemars(
        description = "Store of the target when it is not this one: \"global\", \"group:<name>\" or a 16-char project id"
    )]
    pub(super) store: Option<String>,
    #[schemars(description = "0.0-1.0 (default 1.0)")]
    pub(super) confidence: Option<f64>,
    #[schemars(description = "Why the edge holds")]
    pub(super) note: Option<String>,
    #[schemars(description = "proposed | confirmed (default confirmed)")]
    pub(super) status: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct LinkInput {
    #[schemars(description = "Source memory id")]
    pub(super) src: String,
    #[schemars(
        description = "contradicts | supports | derived_from | related_to | duplicates | supersedes. Symmetric kinds (contradicts, related_to, duplicates) are stored once and seen from both ends."
    )]
    pub(super) kind: String,
    #[schemars(description = "Target memory id")]
    pub(super) dst: String,
    #[schemars(
        description = "Store of the target when it is not the source's: \"global\", \"group:<name>\" or a 16-char project id. A store that cannot be opened here needs the full target id and is recorded as unknown."
    )]
    pub(super) dst_store: Option<String>,
    #[schemars(description = "0.0-1.0 (default 1.0)")]
    pub(super) confidence: Option<f64>,
    #[schemars(description = "Why the edge holds")]
    pub(super) note: Option<String>,
    #[schemars(description = "proposed | confirmed (default confirmed)")]
    pub(super) status: Option<String>,
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct UnlinkInput {
    #[schemars(description = "Source memory id")]
    pub(super) src: String,
    #[schemars(description = "Edge kind")]
    pub(super) kind: String,
    #[schemars(description = "Target memory id")]
    pub(super) dst: String,
    #[schemars(description = "Store of the target when it is not the source's")]
    pub(super) dst_store: Option<String>,
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct LinkStatusInput {
    #[schemars(description = "One end of the edge (either end for a symmetric kind)")]
    pub(super) src: String,
    #[schemars(description = "The other end")]
    pub(super) dst: String,
    #[schemars(description = "Edge kind (default contradicts)")]
    pub(super) kind: Option<String>,
    #[schemars(
        description = "confirmed | rejected | proposed. A rejected edge is kept so it is not proposed again. For contradicts, `confirmed` without `winner` means both stand (both are penalized)."
    )]
    pub(super) status: String,
    #[schemars(
        description = "contradicts only: the id of the memory that supersedes the other. Rewrites the edge as `winner supersedes loser` and invalidates the loser; only the loser is demoted."
    )]
    pub(super) winner: Option<String>,
    #[schemars(description = "Reviewer's note")]
    pub(super) note: Option<String>,
    #[schemars(description = "Store of `dst` when it is not `src`'s")]
    pub(super) dst_store: Option<String>,
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct VerifyInput {
    #[schemars(description = "Memory ID")]
    pub(super) id: String,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct TaskCurrentInput {
    #[schemars(
        description = "Task/feature name to declare for this session (short human-readable name). Omit to read the current declaration."
    )]
    pub(super) task: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct TaskCompleteInput {
    #[schemars(description = "Task/feature name to mark finished.")]
    pub(super) task: String,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct CompressCandidatesInput {
    #[schemars(description = "Scope to filter candidates")]
    pub(super) scope: Option<String>,

    #[schemars(description = "Criticality threshold (default 0.4)")]
    pub(super) threshold: Option<f64>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct CompressApplyInput {
    #[schemars(description = "IDs of memories to compress")]
    pub(super) source_ids: Vec<String>,

    #[schemars(description = "Summary for compressed memory")]
    pub(super) summary: String,

    #[schemars(description = "Content for compressed memory")]
    pub(super) content: String,

    #[schemars(description = "Logical scopes")]
    pub(super) scope: Option<Vec<String>>,

    #[schemars(description = "Tags")]
    pub(super) tags: Option<Vec<String>>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct GcInput {
    #[schemars(description = "List only, no delete (default true)")]
    pub(super) dry_run: Option<bool>,

    #[schemars(description = "Override GC score threshold")]
    pub(super) threshold: Option<f64>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ReindexInput {
    #[schemars(description = "Only re-embed, skip index rebuild")]
    pub(super) embeddings_only: Option<bool>,

    #[schemars(description = "Only rebuild index, skip embedding")]
    pub(super) index_only: Option<bool>,

    #[schemars(
        description = "Report what a reindex would rebuild and change nothing. Names the memories whose file changed since it was indexed, and whose vectors no longer match their text."
    )]
    pub(super) dry_run: Option<bool>,

    #[schemars(
        description = "Re-embed every memory, even ones whose vectors are provably current. A plain reindex reuses vectors whose stored digest still matches; force is the repair-grade rebuild for a chunk row damaged without its digest changing."
    )]
    pub(super) force: Option<bool>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ListInput {
    #[schemars(description = "Filter by memory types")]
    pub(super) types: Option<Vec<String>>,

    #[schemars(description = "Filter by epistemic class: fact, observation, decision (OR logic)")]
    pub(super) epistemic: Option<Vec<String>>,

    #[schemars(description = "Filter by tags (OR logic)")]
    pub(super) tags: Option<Vec<String>>,

    #[schemars(description = "Filter: active|needsreview|challenged")]
    pub(super) status: Option<String>,

    #[schemars(description = "Filter by scope (physical or logical)")]
    pub(super) scope: Option<String>,

    #[schemars(description = "Sort: criticality|created|updated|type (default criticality)")]
    pub(super) sort_field: Option<String>,

    #[schemars(
        description = "Include memories whose validity window was closed (invalidated). Default false."
    )]
    pub(super) include_invalidated: Option<bool>,

    #[schemars(description = "Reverse sort order")]
    pub(super) reverse: Option<bool>,

    #[schemars(description = "Maximum results")]
    pub(super) limit: Option<usize>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct StatsInput {
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
    #[schemars(
        description = "If true, include a `by_project` map breaking down runtime telemetry per project ID. Default false."
    )]
    #[serde(default)]
    pub(super) all_projects: Option<bool>,
    #[schemars(
        description = "A memory id (or unique prefix): add a `memory_usage` object with that memory's usage — returned / injected / fetched counts (separate signals, never summed), distinct sessions, writes, and a 30-day daily series."
    )]
    #[serde(default)]
    pub(super) memory_id: Option<String>,
    #[schemars(description = "Add a `top_used` list of the N most-used memories in this store.")]
    #[serde(default)]
    pub(super) top: Option<usize>,
    #[schemars(
        description = "Read signal that orders `top`: fetched (default), injected or returned. The others break ties; they are never summed."
    )]
    #[serde(default)]
    pub(super) by: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ConfigInput {
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
    #[schemars(
        description = "Number of top unique tags to include (most-used first). Default 20."
    )]
    #[serde(default)]
    pub(super) top_tags: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct DoctorInput {
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, \"global\" for the machine-wide everyone store, or \"group:<name>\" for a named group store. Subscribed groups also fan into a project's queries automatically. Omit for current project."
    )]
    pub(super) project: Option<String>,
    #[schemars(
        description = "Flip memories with epistemic findings (changed invalidation paths, invalid derived-from sources) to needs_review. Default false: report only."
    )]
    pub(super) fix: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ProjectsInfoInput {
    #[schemars(
        description = "Target project: absolute path or 16-char project ID. Omit for current project."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ProjectsLinkInput {
    #[schemars(description = "Project ID of the child to link (16-char hex)")]
    pub(super) child: String,

    #[schemars(description = "Project ID of the parent (16-char hex)")]
    pub(super) parent: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ProjectsUnlinkInput {
    #[schemars(description = "Project ID of the child to promote back to a root (16-char hex)")]
    pub(super) project_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct HarvestListInput {
    #[schemars(
        description = "Only sessions active since this point: RFC 3339, or relative shorthand like `7d`, `12h`, `2w`."
    )]
    pub(super) since: Option<String>,

    #[schemars(description = "Maximum number of sessions to return (newest first)")]
    pub(super) limit: Option<usize>,

    #[schemars(description = "Also list sessions already recorded as reviewed")]
    pub(super) include_harvested: Option<bool>,

    #[schemars(
        description = "Ignore project scoping and list every session on this machine. Off by default, and refused unless the user set [security] allow_all_projects_harvest = true in the global config file (false by default; a project config cannot grant it). Scope otherwise is this project's root plus its worktrees and linked sub-projects."
    )]
    pub(super) all_projects: Option<bool>,

    #[schemars(
        description = "Target project: absolute path or 16-char project ID. Omit for the current project. Unlike other tools, \"global\" and \"group:<name>\" are NOT valid here — transcripts belong to a filesystem project, not to a memory store."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct HarvestShowInput {
    #[schemars(description = "Session id, or a unique prefix of one (from harvest_list)")]
    pub(super) session_id: String,

    #[schemars(
        description = "Character budget for the digest. Defaults to [harvest] digest_budget (200000), enough to cover a typical session whole; pass a smaller value when scanning several sessions, or 0 for unlimited."
    )]
    pub(super) max_chars: Option<usize>,

    #[schemars(description = "Include the assistant's reasoning blocks (verbose)")]
    pub(super) include_thinking: Option<bool>,

    #[schemars(description = "Search every session on this machine, not just this project's")]
    pub(super) all_projects: Option<bool>,

    #[schemars(
        description = "Target project: absolute path or 16-char project ID (see harvest_list). \"global\" and \"group:<name>\" are NOT valid here. Naming a project other than your own is refused unless the user set [security] allow_all_projects_harvest = true in the global config file (a project config cannot grant it)."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct HarvestMarkInput {
    #[schemars(description = "Session id, or a unique prefix of one (from harvest_list)")]
    pub(super) session_id: String,

    #[schemars(description = "Ids of memories created from this session. Omit when none were.")]
    pub(super) memory_ids: Option<Vec<String>>,

    #[schemars(
        description = "harvested | skipped | deferred. Defaults to harvested when memory_ids is non-empty, else skipped. `deferred` keeps the session in harvest_list."
    )]
    pub(super) decision: Option<String>,

    #[schemars(description = "Why the session was skipped or deferred")]
    pub(super) note: Option<String>,

    #[schemars(
        description = "One or two sentences saying what this conversation settled, written into the conversation search index and embedded as its own vector. Worth the keystrokes: a human-written summary is higher-precision than the machine digest, and harvest_search breaks ties toward it. Recorded after the decision, so a model that fails to load costs the summary and not the review."
    )]
    pub(super) summary: Option<String>,

    #[schemars(
        description = "Forget the existing review so the session is unreviewed again. An archived transcript is kept (the ledger entry is the only route to it), so the entry becomes `unreviewed` rather than disappearing; with no archive the entry is removed."
    )]
    pub(super) clear: Option<bool>,

    #[schemars(
        description = "Resolve session_id against every session on this machine, not just this project's — the same scope harvest_show accepts. Off by default, and refused unless the user set [security] allow_all_projects_harvest = true in the global config file (false by default; a project config cannot grant it)."
    )]
    pub(super) all_projects: Option<bool>,

    #[schemars(
        description = "Target project: absolute path or 16-char project ID (see harvest_list). \"global\" and \"group:<name>\" are NOT valid here. Naming a project other than your own is refused unless the user set [security] allow_all_projects_harvest = true in the global config file (a project config cannot grant it)."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct HarvestSearchInput {
    #[schemars(description = "What to look for, in natural language")]
    pub(super) query: String,

    #[schemars(description = "Maximum number of conversations to return (default 10)")]
    pub(super) limit: Option<usize>,

    #[schemars(
        description = "Only conversations that ended since this point: RFC 3339, or relative shorthand like `30d`, `12h`, `2w`."
    )]
    pub(super) since: Option<String>,

    #[schemars(
        description = "Search every project's conversations on this machine. Off by default, and refused unless the user set [security] allow_all_projects_harvest = true in the global config file (false by default; a project config cannot grant it)."
    )]
    pub(super) all_projects: Option<bool>,

    #[schemars(
        description = "Target project: absolute path or 16-char project ID (see harvest_list). \"global\" and \"group:<name>\" are NOT valid here. Naming a project other than your own is refused unless the user set [security] allow_all_projects_harvest = true in the global config file (a project config cannot grant it)."
    )]
    pub(super) project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct HarvestLedgerInput {
    #[schemars(description = "Filter by decision: harvested, skipped, deferred, or unreviewed")]
    pub(super) decision: Option<String>,

    #[schemars(
        description = "Filter by stage — where the session's bytes are: collected, indexed, or compressed. Independent of decision: a session can be indexed while skipped."
    )]
    pub(super) stage: Option<String>,

    #[schemars(
        description = "Only sessions that still hold an archived transcript, i.e. the ones harvest_show can still read after Claude Code pruned its own copy."
    )]
    pub(super) with_archive: Option<bool>,

    #[schemars(
        description = "Target project: absolute path or 16-char project ID (see harvest_list). \"global\" and \"group:<name>\" are NOT valid here. Naming a project other than your own is refused unless the user set [security] allow_all_projects_harvest = true in the global config file (a project config cannot grant it)."
    )]
    pub(super) project: Option<String>,
}
