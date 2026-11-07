//! Typed memory edges: link, unlink, review, and the read views over them.
//!
//! The file format and the derived `edges` table are storage's business
//! (`engram_types::Link`, `engram_storage::edge_index`); this module is the
//! policy both front ends share. Every write here is a read-modify-write
//! through [`MemoryStore::update_with`] that **adds or removes one edge** —
//! there is deliberately no `MemoryUpdate` field for `links`, because a
//! wholesale list replacement built from a snapshot read is exactly how a
//! concurrent writer's edge gets erased.
//!
//! # Rules
//!
//! - **One copy per edge.** A symmetric kind (`contradicts`, `related_to`,
//!   `duplicates`) is stored once, on whichever side wrote it first; linking
//!   the reverse direction updates that copy. The table answers both ends.
//! - **`supersedes` stays canonical in its own fields.** `link A supersedes B`
//!   adds B to A's `supersedes` list and closes B's window
//!   (`superseded_by = A`), exactly as `create --supersedes` does; a `links`
//!   entry rides along only to keep the edge's note, origin and confidence.
//!   `unlink` is the inverse and reopens B when A was what closed it.
//! - **No personal id in a committed file.** A project's shared memories are
//!   committed to git; a personal memory's id written into one leaks into the
//!   repository. A symmetric edge is therefore stored on the personal side
//!   instead (same meaning, no leak); a directed edge in that direction is
//!   refused with the remedy. Group and global stores are not in any
//!   repository, so the rule does not apply there. `doctor` reports a
//!   committed file that acquired such an edge some other way (a visibility
//!   change after linking).
//!
//!   **Declared gap:** the check reads the other end's visibility *before*
//!   the write lock is taken, and it spans two files. A `link` racing an
//!   `update --visibility personal` of its target can therefore both pass —
//!   the link saw the target shared, the visibility change saw no edge yet —
//!   leaving a committed file that names a personal memory. Re-reading under
//!   the lock would not close it, since the visibility change checks outside
//!   the lock too. `doctor`'s leak check (`EdgeHealth::personal_leaks`)
//!   reports the result, and `unlink` removes it.
//! - **Dangling is legal.** Deleting a memory leaves edges into it; reads
//!   filter them out by default and `doctor` reports them. A cross-store
//!   target whose store cannot be opened here is `Unknown`, never `Dangling`
//!   — the absence of evidence is not evidence of deletion.
//! - **Edges are not challenges.** `resolve --keep` clears challenges and
//!   never touches `links`; a rejected edge is kept (`status: rejected`) so an
//!   automatic proposer does not raise it again.
//! - **gc never edits edges.** A memory's own edges go when it is deleted;
//!   rejected edges are retained on purpose; dangling ones are reported, not
//!   pruned — pruning would rewrite committed files gc otherwise leaves alone,
//!   and a target removed on one branch can come back with a checkout.
//!
//! Every write records a `linked` usage event on the memory whose file
//! changed, with the other end as `related_id`.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::storage::{MemoryStore, StorageError};
use crate::types::{Memory, MemoryType, Status, Visibility};

pub use crate::storage::edge_index::{EdgeField, EdgeRow};
pub use crate::types::{Link, LinkKind, LinkOrigin, LinkStatus};

/// Most outbound edges one memory may hold. A guard against an automatic
/// proposer (NLI, similarity) flooding a committed file, not a modelling
/// limit: a human linking by hand never gets near it.
pub const MAX_LINKS_PER_MEMORY: usize = 256;

/// Longest note an edge may carry, in characters.
pub const MAX_NOTE_CHARS: usize = 1000;

/// A write this module refused on its own rules — the request is the
/// problem, not the store: a personal id that would leak, a malformed store
/// spec, a self-link, a loser already superseded by something else, an edge
/// cap. Typed so a front end can answer "refused" (MCP `VALIDATION_ERROR`,
/// dashboard 409) and keep every other failure (I/O, the index) the
/// server's. The message is what the CLI prints.
#[derive(Debug)]
pub struct LinkRefused(pub String);

impl std::fmt::Display for LinkRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LinkRefused {}

/// `bail!` with a [`LinkRefused`].
macro_rules! refuse {
    ($($arg:tt)*) => {
        return Err(LinkRefused(format!($($arg)*)).into())
    };
}

// ---------------------------------------------------------------------------
// Store ids
// ---------------------------------------------------------------------------

/// Turn a store spec as the front ends accept it into the id an edge records:
/// `global` → [`crate::storage::paths::GLOBAL_PROJECT_ID`], `group:<name>` →
/// its group id, and an already-canonical id (16-hex project id, `__g_…`
/// group id, the global id) unchanged. An id, never a path: a path differs
/// between checkouts and machines, and the file travels with the repository.
pub fn canonical_store_id(spec: &str) -> Result<String> {
    let spec = spec.trim();
    if spec == "global" || spec == crate::storage::paths::GLOBAL_PROJECT_ID {
        return Ok(crate::storage::paths::GLOBAL_PROJECT_ID.to_string());
    }
    if let Some(name) = spec.strip_prefix("group:") {
        if name.trim().is_empty() {
            refuse!("empty group name in store spec {spec:?}");
        }
        return Ok(crate::storage::paths::compute_group_id(name));
    }
    if crate::storage::paths::is_canonical_group_id(spec) {
        return Ok(spec.to_string());
    }
    if spec.len() == 16 && spec.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(spec.to_ascii_lowercase());
    }
    refuse!(
        "unrecognized store {spec:?}: expected \"global\", \"group:<name>\" or a 16-character \
         project id"
    )
}

/// Open the store `id` names ([`canonical_store_id`] form). `read_only`
/// uses the side-effect-free open — the right choice for an existence check,
/// which must not migrate or create anything in another project.
///
/// `id` often comes from a memory file someone else wrote (an edge's
/// `store`), so anything but a well-formed id
/// ([`crate::storage::paths::is_canonical_store_id`]) is refused before it
/// reaches the registry or a path join — `__g_/../..` would otherwise open an
/// arbitrary directory. Every caller reads a failed open as `Unknown`.
pub async fn open_store_by_id(id: &str, read_only: bool) -> Result<MemoryStore> {
    use crate::storage::paths;
    if !paths::is_canonical_store_id(id) {
        refuse!("{id:?} is not a store id");
    }
    if id == paths::GLOBAL_PROJECT_ID {
        return Ok(if read_only {
            MemoryStore::open_global_read_only().await?
        } else {
            MemoryStore::open_global().await?
        });
    }
    if paths::is_group_id(id) {
        return Ok(if read_only {
            MemoryStore::open_group_read_only(id).await?
        } else {
            MemoryStore::open_group(id).await?
        });
    }
    use crate::storage::RegistryBackend;
    let registry = crate::storage::FileRegistry::global()?.load().await?;
    let root = crate::storage::resolve_root_project_id(&registry, id);
    let entry = registry
        .projects
        .iter()
        .find(|e| e.project_id == root)
        .ok_or_else(|| anyhow!("project {id} is not in the registry"))?;
    let dir = std::path::PathBuf::from(&entry.project_path);
    Ok(if read_only {
        MemoryStore::open_read_only(&dir).await?
    } else {
        MemoryStore::open(&dir).await?
    })
}

/// A project's shared memories are committed to its repository; nothing
/// else EngramDB writes is.
fn is_committed(store: &MemoryStore, memory: &Memory) -> bool {
    !store.is_global() && !store.is_group() && memory.visibility == Visibility::Shared
}

/// `None` when `dst_store` names `store` itself — "same store" is always
/// spelled `None`, so two spellings of one edge cannot coexist.
fn normalize_store(store: &MemoryStore, dst_store: Option<&str>) -> Result<Option<String>> {
    match dst_store {
        None => Ok(None),
        Some(spec) => {
            let id = canonical_store_id(spec)?;
            Ok((id != store.project_id).then_some(id))
        }
    }
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// What to link.
#[derive(Debug, Clone)]
pub struct LinkParams {
    /// Source memory (id or unique prefix) in the store passed to [`link`].
    pub src: String,
    pub kind: LinkKind,
    /// Target memory: id or unique prefix. A prefix must resolve; a full id in
    /// a store that cannot be opened here is accepted and recorded as
    /// `Unknown`.
    pub dst: String,
    /// Target store spec ([`canonical_store_id`]); `None` = same store.
    pub dst_store: Option<String>,
    /// Defaults to 1.0.
    pub confidence: Option<f64>,
    pub origin: LinkOrigin,
    /// Defaults to `confirmed`: an explicit link is a claim its author makes.
    /// Automatic proposers pass `proposed`.
    pub status: Option<LinkStatus>,
    pub note: Option<String>,
}

/// Whether the far end of an edge could be checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeState {
    /// The other end exists and its validity window is open.
    Live,
    /// The other end exists but is invalidated (hidden by default).
    Invalidated,
    /// The other end's store was checked and the memory is not there.
    Dangling,
    /// The other end lives in a store that could not be opened here.
    Unknown,
}

/// What [`link`] (and the status writes) did.
#[derive(Debug, Clone, Serialize)]
pub struct LinkResult {
    /// The memory whose file now holds the edge — the source, or the target
    /// for a symmetric edge stored on the other side.
    pub owner_id: String,
    pub src_id: String,
    pub dst_id: String,
    pub dst_store: Option<String>,
    pub kind: LinkKind,
    pub status: LinkStatus,
    /// `false` when an existing edge was updated rather than added.
    pub created: bool,
    pub target_state: EdgeState,
    /// Set when the edge went somewhere other than the source's file, and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored_note: Option<String>,
}

/// A resolved far end: full id, and its memory when it could be read.
struct Target {
    id: String,
    memory: Option<Memory>,
    state: EdgeState,
}

async fn resolve_target(store: &MemoryStore, dst: &str, dst_store: Option<&str>) -> Result<Target> {
    match dst_store {
        None => {
            let id = store.resolve_id(dst).await.map_err(|e| match e {
                // Kept a `StorageError::NotFound` (with the side named) so a
                // front end answers "not found", not "refused".
                StorageError::NotFound(id) => anyhow::Error::from(StorageError::NotFound(id))
                    .context(format!("target memory not found: {dst}")),
                other => other.into(),
            })?;
            let memory = store.get(&id).await?;
            let state = if memory.is_invalidated() {
                EdgeState::Invalidated
            } else {
                EdgeState::Live
            };
            Ok(Target {
                id,
                memory: Some(memory),
                state,
            })
        }
        Some(other) => match open_store_by_id(other, true).await {
            Ok(target_store) => {
                let id = target_store.resolve_id(dst).await.map_err(|e| match e {
                    StorageError::NotFound(id) => anyhow::Error::from(StorageError::NotFound(id))
                        .context(format!("target memory not found in store {other}: {dst}")),
                    other => other.into(),
                })?;
                let memory = target_store.get(&id).await?;
                let state = if memory.is_invalidated() {
                    EdgeState::Invalidated
                } else {
                    EdgeState::Live
                };
                Ok(Target {
                    id,
                    memory: Some(memory),
                    state,
                })
            }
            Err(e) => {
                // An unopenable store cannot resolve a prefix, so only a full
                // id is accepted blind — and it is recorded as unknown.
                if uuid::Uuid::parse_str(dst).is_err() {
                    refuse!(
                        "store {other} cannot be opened here ({e:#}), so the target must be \
                         given as a full id, not a prefix"
                    );
                }
                tracing::info!(store = %other, "link target store not openable; edge recorded as unknown: {e:#}");
                Ok(Target {
                    id: dst.to_string(),
                    memory: None,
                    state: EdgeState::Unknown,
                })
            }
        },
    }
}

fn validate_note(note: Option<String>) -> Result<Option<String>> {
    let Some(note) = note else { return Ok(None) };
    let note = note.trim().to_string();
    if note.is_empty() {
        return Ok(None);
    }
    if note.chars().count() > MAX_NOTE_CHARS {
        refuse!("note is longer than {MAX_NOTE_CHARS} characters");
    }
    Ok(Some(note))
}

/// The edge `challenge --against` records: a proposed `contradicts` from the
/// challenged memory to the one that contradicts it, carrying the evidence
/// as its note. Proposed, not confirmed — a challenge disputes one memory,
/// and the reviewer is who decides whether both stand or one supersedes.
pub fn challenge_edge(challenged: &str, against: &str, evidence: &str) -> LinkParams {
    LinkParams {
        src: challenged.to_string(),
        kind: LinkKind::Contradicts,
        dst: against.to_string(),
        dst_store: None,
        confidence: None,
        origin: LinkOrigin::Agent,
        status: Some(LinkStatus::Proposed),
        note: Some(evidence.chars().take(MAX_NOTE_CHARS).collect()),
    }
}

/// Refuse a write that would put a personal memory's id into a committed
/// file — in any direction, before anything is written (module docs).
///
/// `holder` is the memory whose file is being written, as it will be after
/// the write. `named` are the same-store ids that file will name (`links`
/// without a store, `supersedes`, `superseded_by`); `supersede_targets` are
/// the ids whose files will name `holder` back (`superseded_by = holder`).
/// With `check_inbound`, the files that already name `holder` are checked
/// too — for a shared → personal visibility change, where the leak is in
/// *other* committed files.
///
/// Only project stores are committed; group and global stores pass.
pub(crate) async fn check_privacy(
    store: &MemoryStore,
    holder_id: Option<&str>,
    holder_visibility: Visibility,
    named: &[String],
    supersede_targets: &[String],
    check_inbound: bool,
) -> Result<()> {
    if store.is_global() || store.is_group() {
        return Ok(());
    }
    let mut ids: Vec<&str> = named.iter().map(String::as_str).collect();
    ids.extend(supersede_targets.iter().map(String::as_str));
    ids.retain(|id| Some(*id) != holder_id);
    let loaded: HashMap<String, Memory> = store.get_batch(&ids).await?.into_iter().collect();
    let holder = holder_id.unwrap_or("the new memory");
    if holder_visibility == Visibility::Shared {
        for id in named {
            if loaded
                .get(id)
                .is_some_and(|m| m.visibility == Visibility::Personal)
            {
                refuse!(
                    "refusing: shared memory {holder}'s file is committed to git and would name \
                     personal memory {id}, leaking its id into the repository. Make both the \
                     same visibility, or link from the personal side"
                );
            }
        }
    } else {
        for id in supersede_targets {
            if loaded
                .get(id)
                .is_some_and(|m| m.visibility == Visibility::Shared)
            {
                refuse!(
                    "refusing: personal memory {holder} cannot supersede shared memory {id} — \
                     the shared memory's committed file would record `superseded_by = \
                     {holder}`, leaking a personal id into the repository"
                );
            }
        }
        if check_inbound {
            if let Some(hid) = holder_id {
                let rows = store.list_edge_rows_touching(hid).await?;
                let owners: Vec<&str> = rows
                    .iter()
                    .filter(|r| r.owner_id != hid && r.dst_store.is_none())
                    .map(|r| r.owner_id.as_str())
                    .collect();
                let owner_mems = store.get_batch(&owners).await?;
                if let Some((_, m)) = owner_mems
                    .iter()
                    .find(|(_, m)| m.visibility == Visibility::Shared)
                {
                    refuse!(
                        "refusing to make {hid} personal: shared memory {}'s committed file \
                         names it (an edge or a supersession). Remove that edge first",
                        m.id
                    );
                }
            }
        }
    }
    Ok(())
}

/// Every check [`link`] makes, and what it would write — computed without
/// writing anything.
struct Plan {
    src: Memory,
    target: Target,
    dst_store: Option<String>,
    /// The file that will hold the edge.
    owner: String,
    edge: Link,
    /// Which of the caller's optional fields were given (a re-link only
    /// changes what it was asked to change).
    given: Given,
    stored_note: Option<String>,
}

#[derive(Clone, Copy)]
struct Given {
    confidence: bool,
    status: bool,
    note: bool,
}

async fn plan_link(store: &MemoryStore, params: LinkParams) -> Result<Plan> {
    if !params.kind.is_known() {
        refuse!(
            "unknown edge kind {:?} (known: {})",
            params.kind.as_str(),
            LinkKind::known_list()
        );
    }
    let given = Given {
        confidence: params.confidence.is_some(),
        status: params.status.is_some(),
        note: params.note.is_some(),
    };
    let confidence = super::validate_score(params.confidence.unwrap_or(1.0), "confidence")
        .map_err(|e| LinkRefused(format!("{e:#}")))?;
    let note = validate_note(params.note)?;
    let status = params.status.unwrap_or(LinkStatus::Confirmed);
    if !status.is_known() {
        refuse!(
            "unknown edge status {:?} (known: {})",
            status.as_str(),
            LinkStatus::known_list()
        );
    }
    if params.kind == LinkKind::Supersedes && status != LinkStatus::Confirmed {
        refuse!(
            "a supersession is never {status}: it closes the target's validity window, so it is \
             either recorded or not. Propose a `contradicts` edge instead, or `unlink` an \
             existing supersession to reject it"
        );
    }
    let dst_store = normalize_store(store, params.dst_store.as_deref())?;

    let src = store.get(&params.src).await?;
    let target = resolve_target(store, &params.dst, dst_store.as_deref()).await?;
    if dst_store.is_none() && target.id == src.id {
        refuse!("a memory cannot link to itself");
    }

    let edge = Link {
        to: target.id.clone(),
        kind: params.kind.clone(),
        store: dst_store.clone(),
        confidence,
        origin: params.origin,
        status,
        at: Some(Utc::now()),
        note,
    };

    if params.kind == LinkKind::Supersedes {
        if dst_store.is_some() {
            refuse!(
                "`supersedes` edges must stay within one store (superseded_by cannot name \
                 another)"
            );
        }
        // Both files name the other: src's `supersedes`, target's
        // `superseded_by`. Either direction can leak.
        check_privacy(
            store,
            Some(&src.id),
            src.visibility,
            std::slice::from_ref(&target.id),
            std::slice::from_ref(&target.id),
            false,
        )
        .await?;
        if let Some(t) = &target.memory {
            if let Some(other) = t.superseded_by.as_deref().filter(|o| *o != src.id) {
                refuse!(
                    "{} is already superseded by {other}; unlink that supersession first",
                    t.id
                );
            }
        }
        return Ok(Plan {
            owner: src.id.clone(),
            src,
            target,
            dst_store,
            edge,
            given,
            stored_note: None,
        });
    }

    // The personal-id rule (module docs).
    let target_personal = target
        .memory
        .as_ref()
        .is_some_and(|m| m.visibility == Visibility::Personal);
    let mut owner = src.id.clone();
    let mut stored = edge.clone();
    let mut stored_note = None;
    if is_committed(store, &src) && target_personal {
        if params.kind.is_symmetric() && dst_store.is_none() {
            owner = target.id.clone();
            stored.to = src.id.clone();
            stored_note = Some(format!(
                "stored on the personal memory {}: a committed memory's file must not name a \
                 personal memory",
                target.id
            ));
        } else {
            refuse!(
                "refusing to link shared memory {} to personal memory {}: the shared memory's \
                 file is committed to git, and the personal id would leak into the repository. \
                 Link from the personal memory instead{}",
                src.id,
                target.id,
                if params.kind.is_symmetric() {
                    " (in its own store)"
                } else {
                    ", or make one of them shared/personal to match"
                }
            );
        }
    } else if params.kind.is_symmetric() && dst_store.is_none() {
        // Already stored on the other side? Update that copy (one per edge).
        // This read is outside the write lock and spans two files, so two
        // concurrent opposite links can each store a copy; the edge index
        // keys a symmetric edge on its sorted endpoints, so readers still see
        // one edge (`edge_index::EdgeRow::edge_key`).
        if let Some(other) = &target.memory {
            let reverse = Link {
                to: src.id.clone(),
                ..edge.clone()
            };
            if other.links.iter().any(|l| l.same_edge(&reverse)) {
                owner = other.id.clone();
                stored = reverse;
            }
        }
    }
    Ok(Plan {
        src,
        target,
        dst_store,
        owner,
        edge: stored,
        given,
        stored_note,
    })
}

/// Run every check [`link`] would, and write nothing. Front ends call this
/// before a write the edge depends on (`challenge --against`), so a bad
/// target fails the whole request instead of leaving half of it done.
pub async fn check_link(store: &MemoryStore, params: LinkParams) -> Result<()> {
    plan_link(store, params).await.map(|_| ())
}

/// Add an edge, or update the attributes of the same edge (same target, kind
/// and store) when it already exists. See the module docs for the rules.
///
/// Updating an existing edge changes only what the caller passed: an absent
/// confidence, note or status keeps the stored one, and the origin is always
/// kept (it records who proposed the edge). A **proposal** (`status:
/// proposed`) never touches an existing edge at all — re-proposing would
/// otherwise downgrade a confirmed edge, resurrect a rejected one, and
/// overwrite the reviewer's note.
pub async fn link(
    store: &MemoryStore,
    params: LinkParams,
    surface: super::usage::Surface,
) -> Result<LinkResult> {
    let plan = plan_link(store, params).await?;
    let kind = plan.edge.kind.clone();
    // A proposal onto an existing edge changes nothing — not even the file's
    // `updated_at`, so it is answered without a write.
    let owner_mem = if plan.owner == plan.src.id {
        Some(&plan.src)
    } else {
        plan.target.memory.as_ref()
    };
    let existing = owner_mem.and_then(|m| m.links.iter().find(|l| l.same_edge(&plan.edge)));
    if let (LinkStatus::Proposed, Some(existing)) = (&plan.edge.status, existing) {
        return Ok(LinkResult {
            owner_id: plan.owner.clone(),
            src_id: plan.src.id.clone(),
            dst_id: plan.target.id.clone(),
            dst_store: plan.dst_store.clone(),
            kind,
            status: existing.status.clone(),
            created: false,
            target_state: plan.target.state,
            stored_note: plan.stored_note.clone(),
        });
    }
    let (created, written) = if kind == LinkKind::Supersedes {
        let target_id = plan.target.id.clone();
        let edge = plan.edge.clone();
        let given = plan.given;
        let mut created = false;
        let mut written = edge.clone();
        let mut refused = None;
        let res = store
            .update_with(&plan.src.id, |m| {
                if !m.supersedes.contains(&target_id) {
                    m.supersedes.push(target_id.clone());
                    created = true;
                }
                written = merge_into(&mut m.links, edge.clone(), given)
                    .inspect_err(|e| refused = Some(e.to_string()))?
                    .1;
                Ok(())
            })
            .await;
        if let Some(msg) = refused {
            return Err(LinkRefused(msg).into());
        }
        res?;
        super::close_superseded_windows(store, std::slice::from_ref(&target_id), &plan.src.id)
            .await;
        (created, written)
    } else {
        upsert_edge(store, &plan.owner, plan.edge.clone(), plan.given).await?
    };
    let related = if plan.owner == plan.src.id {
        plan.target.id.clone()
    } else {
        plan.src.id.clone()
    };
    super::usage::record_write(
        store,
        &plan.owner,
        super::usage::UsageKind::Linked,
        surface,
        Some(&related),
    )
    .await;
    // `link A contradicts B --status confirmed|rejected` is a reviewer's
    // verdict like `link_status`'s: it settles the pair, so the NLI challenges
    // it came with go too ([`confirm_contradiction_direction`]).
    if kind == LinkKind::Contradicts
        && plan.dst_store.is_none()
        && plan.given.status
        && matches!(written.status, LinkStatus::Confirmed | LinkStatus::Rejected)
    {
        resolve_nli_challenges(store, &plan.src.id, &plan.target.id, surface, None).await;
    }
    Ok(LinkResult {
        owner_id: plan.owner,
        src_id: plan.src.id,
        dst_id: plan.target.id,
        dst_store: plan.dst_store,
        kind,
        status: written.status,
        created,
        target_state: plan.target.state,
        stored_note: plan.stored_note,
    })
}

/// Merge `edge` into `links` (see [`link`] for what an update changes).
/// Returns whether it was new, and the edge as it now stands.
fn merge_into(links: &mut Vec<Link>, edge: Link, given: Given) -> Result<(bool, Link)> {
    if let Some(existing) = links.iter_mut().find(|l| l.same_edge(&edge)) {
        if edge.status == LinkStatus::Proposed {
            return Ok((false, existing.clone()));
        }
        if given.status {
            existing.status = edge.status;
        }
        if given.confidence {
            existing.confidence = edge.confidence;
        }
        if given.note {
            existing.note = edge.note;
        }
        existing.at = edge.at;
        return Ok((false, existing.clone()));
    }
    if links.len() >= MAX_LINKS_PER_MEMORY {
        refuse!(
            "this memory already has {MAX_LINKS_PER_MEMORY} outbound edges; remove some before \
             adding more"
        );
    }
    links.push(edge.clone());
    Ok((true, edge))
}

/// Add `edge` to `owner`'s links, or update the existing same edge in place
/// ([`merge_into`]). Returns whether it was new and the edge as stored.
async fn upsert_edge(
    store: &MemoryStore,
    owner: &str,
    edge: Link,
    given: Given,
) -> Result<(bool, Link)> {
    let mut out = (false, edge.clone());
    // `update_with` turns a closure error into a storage error; a refusal
    // (the edge cap) is carried out separately so it stays typed.
    let mut refused = None;
    let res = store
        .update_with(owner, |m| {
            out = merge_into(&mut m.links, edge.clone(), given)
                .inspect_err(|e| refused = Some(e.to_string()))?;
            Ok(())
        })
        .await;
    if let Some(msg) = refused {
        return Err(LinkRefused(msg).into());
    }
    res?;
    Ok(out)
}

/// Which edge to remove or review.
#[derive(Debug, Clone)]
pub struct EdgeRef {
    pub src: String,
    pub kind: LinkKind,
    pub dst: String,
    pub dst_store: Option<String>,
}

/// Find where an edge is stored: `(owner_id, link as stored)`. Looks on the
/// source, then — for a symmetric kind in one store — on the target.
async fn locate(store: &MemoryStore, edge: &EdgeRef) -> Result<(String, Link, String, String)> {
    let dst_store = normalize_store(store, edge.dst_store.as_deref())?;
    let src = store.get(&edge.src).await?;
    let dst_id = match dst_store {
        None => store
            .resolve_id(&edge.dst)
            .await
            .unwrap_or_else(|_| edge.dst.clone()),
        Some(_) => edge.dst.clone(),
    };
    let probe = Link {
        to: dst_id.clone(),
        kind: edge.kind.clone(),
        store: dst_store.clone(),
        ..Link::new("", LinkKind::RelatedTo, LinkOrigin::Human)
    };
    if let Some(l) = src.links.iter().find(|l| l.same_edge(&probe)) {
        return Ok((src.id.clone(), l.clone(), src.id, dst_id));
    }
    if edge.kind.is_symmetric() && dst_store.is_none() {
        if let Ok(other) = store.get(&dst_id).await {
            let reverse = Link {
                to: src.id.clone(),
                ..probe
            };
            if let Some(l) = other.links.iter().find(|l| l.same_edge(&reverse)) {
                return Ok((other.id.clone(), l.clone(), src.id, dst_id));
            }
        }
    }
    Err(EdgeNotFound(format!(
        "no {} edge from {} to {}{}",
        edge.kind,
        src.id,
        dst_id,
        dst_store
            .map(|s| format!(" in store {s}"))
            .unwrap_or_default()
    ))
    .into())
}

/// The edge a status write or `unlink` named does not exist (both ends may).
/// Typed so a front end can answer "not found" rather than "refused"; the
/// message is what the CLI prints.
#[derive(Debug)]
pub struct EdgeNotFound(pub String);

impl std::fmt::Display for EdgeNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EdgeNotFound {}

/// What [`unlink`] did.
#[derive(Debug, Clone, Serialize)]
pub struct UnlinkResult {
    pub owner_id: String,
    pub src_id: String,
    pub dst_id: String,
    pub kind: LinkKind,
    /// For `supersedes`: the target's window was reopened because this edge
    /// was what closed it.
    pub reopened: bool,
}

/// Remove one edge. `supersedes` removes the canonical entry too, and
/// reopens the target when this source is what closed its window.
pub async fn unlink(
    store: &MemoryStore,
    edge: EdgeRef,
    surface: super::usage::Surface,
) -> Result<UnlinkResult> {
    if edge.kind == LinkKind::Supersedes {
        return unlink_supersedes(store, edge, surface).await;
    }
    let (owner, stored, src_id, dst_id) = locate(store, &edge).await?;
    store
        .update_with(&owner, |m| {
            let before = m.links.len();
            m.links.retain(|l| !l.same_edge(&stored));
            if m.links.len() == before {
                bail!("edge vanished before it could be removed");
            }
            Ok(())
        })
        .await?;
    let related = if owner == src_id { &dst_id } else { &src_id };
    // A symmetric edge can have a second copy in the other file (concurrent
    // opposite links, racing proposers, a merge); leaving it would keep the
    // edge visible from both ends after a successful unlink.
    remove_reverse_copy(store, &owner, related, &stored)
        .await
        .map_err(|e| {
            anyhow!(
                "removed the edge from {owner}, but its second copy on {related} could not be \
                 removed ({e:#}); run the same unlink again"
            )
        })?;
    super::usage::record_write(
        store,
        &owner,
        super::usage::UsageKind::Linked,
        surface,
        Some(related),
    )
    .await;
    Ok(UnlinkResult {
        owner_id: owner,
        src_id,
        dst_id,
        kind: edge.kind,
        reopened: false,
    })
}

/// Remove the reverse copy of the symmetric same-store edge `stored` (held
/// by `owner`) from `other`'s file, when that file holds one. Reads first,
/// so the common one-copy case writes nothing.
async fn remove_reverse_copy(
    store: &MemoryStore,
    owner: &str,
    other: &str,
    stored: &Link,
) -> Result<()> {
    if !stored.kind.is_symmetric() || stored.store.is_some() {
        return Ok(());
    }
    let reverse = Link {
        to: owner.to_string(),
        ..stored.clone()
    };
    let has_copy = match store.get(other).await {
        Ok(m) => m.links.iter().any(|l| l.same_edge(&reverse)),
        Err(StorageError::NotFound(_)) => false,
        Err(e) => return Err(e.into()),
    };
    if has_copy {
        store
            .update_with(other, |m| {
                m.links.retain(|l| !l.same_edge(&reverse));
                Ok(())
            })
            .await?;
    }
    Ok(())
}

async fn unlink_supersedes(
    store: &MemoryStore,
    edge: EdgeRef,
    surface: super::usage::Surface,
) -> Result<UnlinkResult> {
    let src = store.get(&edge.src).await?;
    let dst_id = store
        .resolve_id(&edge.dst)
        .await
        .unwrap_or_else(|_| edge.dst.clone());
    let target = store.get(&dst_id).await.ok();
    let in_list = src.supersedes.contains(&dst_id);
    let in_links = src
        .links
        .iter()
        .any(|l| l.kind == LinkKind::Supersedes && l.to == dst_id && l.store.is_none());
    let mirrored = target
        .as_ref()
        .is_some_and(|t| t.superseded_by.as_deref() == Some(src.id.as_str()));
    if !in_list && !in_links && !mirrored {
        return Err(EdgeNotFound(format!("{} does not supersede {}", src.id, dst_id)).into());
    }
    if in_list || in_links {
        store
            .update_with(&src.id, |m| {
                m.supersedes.retain(|s| s != &dst_id);
                m.links.retain(|l| {
                    !(l.kind == LinkKind::Supersedes && l.to == dst_id && l.store.is_none())
                });
                Ok(())
            })
            .await?;
    }
    // Reopen the target only when nothing else still supersedes it: another
    // memory listing it in `supersedes` takes over the mirror instead, and
    // the window stays closed.
    let reopened = if mirrored {
        let src_id = src.id.clone();
        let others: Vec<String> = store
            .list_edge_rows_touching(&dst_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                r.kind == LinkKind::Supersedes
                    && r.field == EdgeField::Supersedes
                    && r.dst_id == dst_id
                    && r.src_id != src_id
            })
            .map(|r| r.src_id)
            .collect();
        let successor = store
            .get_batch(&others.iter().map(String::as_str).collect::<Vec<_>>())
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|(_, m)| m.supersedes.contains(&dst_id))
            .map(|(id, _)| id);
        let reopen = successor.is_none();
        store
            .update_with(&dst_id, |m| {
                if m.superseded_by.as_deref() == Some(src_id.as_str()) {
                    match successor {
                        Some(next) => m.superseded_by = Some(next),
                        None => {
                            m.superseded_by = None;
                            m.invalidated_at = None;
                        }
                    }
                }
                Ok(())
            })
            .await?;
        reopen
    } else {
        false
    };
    super::usage::record_write(
        store,
        &src.id,
        super::usage::UsageKind::Linked,
        surface,
        Some(&dst_id),
    )
    .await;
    Ok(UnlinkResult {
        owner_id: src.id.clone(),
        src_id: src.id,
        dst_id,
        kind: LinkKind::Supersedes,
        reopened,
    })
}

/// Set an existing edge's review status (confirm / reject / back to
/// proposed). A rejected edge is kept, not removed (module docs).
pub async fn set_link_status(
    store: &MemoryStore,
    edge: EdgeRef,
    status: LinkStatus,
    note: Option<String>,
    surface: super::usage::Surface,
) -> Result<LinkResult> {
    if !status.is_known() {
        refuse!(
            "unknown edge status {:?} (known: {})",
            status.as_str(),
            LinkStatus::known_list()
        );
    }
    if edge.kind == LinkKind::Supersedes {
        // A supersession is recorded or it is not (see `link`): flipping
        // its entry's status would leave the target's window closed under
        // an edge the table calls rejected.
        refuse!(
            "a supersession has no review status: it is confirmed by existing. To reject it, \
             `unlink` it (which reopens the target)"
        );
    }
    match write_status(store, edge, status, note, surface, false).await? {
        Some(r) => Ok(r),
        // Unreachable without the precondition; answer as a no-op.
        None => refuse!("edge status unchanged"),
    }
}

/// The edge a review named is no longer `proposed`: someone reviewed it
/// already (another tab, the CLI). Typed so a front end can answer
/// "conflict" and say what it is now.
#[derive(Debug)]
pub struct NotProposed {
    pub current: LinkStatus,
}

impl std::fmt::Display for NotProposed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this edge is already {}, not proposed: it was reviewed elsewhere. Reload to see it",
            self.current.as_str()
        )
    }
}

impl std::error::Error for NotProposed {}

/// What a review write did, for a front end to say.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewOutcome {
    /// `confirmed`, `rejected`, `superseded`.
    pub outcome: &'static str,
    /// `false` when the edge was already in the requested state (a repeated
    /// submit): nothing was written and no event recorded.
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loser: Option<String>,
    /// Memories whose NLI challenge about the other end was resolved by this
    /// review ([`resolve_nli_challenges`]).
    pub resolved_challenges: Vec<String>,
}

/// A reviewer's verdict on a **proposed** edge: confirm or reject it. Unlike
/// [`set_link_status`] this requires the edge to still be `proposed`, checked
/// under the write lock: an edge already in `to` is a no-op (`changed:
/// false`, no event — a double submit), any other status is
/// [`NotProposed`]. Settling a same-store `contradicts` edge either way also
/// resolves the NLI challenges it came with ([`resolve_nli_challenges`]); for
/// the supersede verdict use [`review_contradiction`].
pub async fn review_link(
    store: &MemoryStore,
    edge: EdgeRef,
    to: LinkStatus,
    note: Option<String>,
    surface: super::usage::Surface,
) -> Result<ReviewOutcome> {
    if to == LinkStatus::Proposed {
        refuse!("a review confirms or rejects; it cannot set an edge back to proposed");
    }
    let contradicts = edge.kind == LinkKind::Contradicts && edge.dst_store.is_none();
    let outcome = if to == LinkStatus::Rejected {
        "rejected"
    } else {
        "confirmed"
    };
    let written = write_status(store, edge, to, note, surface, true).await?;
    let mut resolved_challenges = Vec::new();
    if let (true, Some(r)) = (contradicts, &written) {
        resolved_challenges =
            resolve_nli_challenges(store, &r.src_id, &r.dst_id, surface, None).await;
    }
    Ok(ReviewOutcome {
        outcome,
        changed: written.is_some(),
        winner: None,
        loser: None,
        resolved_challenges,
    })
}

/// [`confirm_contradiction_direction`] for a reviewer of a **proposed**
/// `contradicts` edge (the dashboard), with [`review_link`]'s precondition: a
/// repeated submit is a no-op (`changed: false`), an edge reviewed elsewhere
/// is [`NotProposed`]. Every verdict also resolves the NLI challenges the
/// contradiction came with ([`resolve_nli_challenges`]): the reviewer has
/// settled the pair, so neither memory keeps paying a score penalty for it —
/// "both stand" records the conflict as a confirmed edge instead.
pub async fn review_contradiction(
    store: &MemoryStore,
    a: &str,
    b: &str,
    verdict: ContradictionVerdict,
    note: Option<String>,
    surface: super::usage::Surface,
) -> Result<ReviewOutcome> {
    let edge = EdgeRef {
        src: a.to_string(),
        kind: LinkKind::Contradicts,
        dst: b.to_string(),
        dst_store: None,
    };
    match verdict {
        ContradictionVerdict::BothStand | ContradictionVerdict::Reject => {
            let to = if verdict == ContradictionVerdict::Reject {
                LinkStatus::Rejected
            } else {
                LinkStatus::Confirmed
            };
            review_link(store, edge, to, note, surface).await
        }
        ContradictionVerdict::Supersedes { winner } => {
            let (_, stored, a_id, b_id) = match locate(store, &edge).await {
                Ok(v) => v,
                Err(e) if e.downcast_ref::<EdgeNotFound>().is_some() => {
                    // Gone: a repeated submit of this same verdict leaves
                    // `winner supersedes loser` behind.
                    let w = winner.trim();
                    let (ra, rb) = (store.resolve_id(a).await?, store.resolve_id(b).await?);
                    let (win, lose) = if !w.is_empty() && ra.starts_with(w) {
                        (ra, rb)
                    } else {
                        (rb, ra)
                    };
                    let done = store.get(&win).await?.supersedes.contains(&lose);
                    if done {
                        return Ok(ReviewOutcome {
                            outcome: "superseded",
                            changed: false,
                            winner: Some(win),
                            loser: Some(lose),
                            resolved_challenges: vec![],
                        });
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            };
            // A fast answer from the snapshot; the check that counts is the
            // claim under the write lock in `apply_direction`.
            if stored.status != LinkStatus::Proposed {
                return Err(NotProposed {
                    current: stored.status,
                }
                .into());
            }
            let (out, resolved_challenges) = apply_direction(
                store,
                &a_id,
                &b_id,
                ContradictionVerdict::Supersedes { winner },
                note,
                surface,
                true,
            )
            .await?;
            Ok(ReviewOutcome {
                outcome: out.outcome,
                changed: true,
                winner: out.winner,
                loser: out.loser,
                resolved_challenges,
            })
        }
    }
}

/// Remove the NLI challenges `a` and `b` carry about each other
/// ([`crate::nli::names_nli_contradiction_with`]); a memory left with no
/// challenge goes back to `active` from `challenged` / `needs_review`. Records
/// `resolved` (related = the other end) for each memory changed, except
/// `already_recorded` (a superseded loser that recorded it). Best-effort: a
/// failed write is logged and the memory is left out of the result.
pub async fn resolve_nli_challenges(
    store: &MemoryStore,
    a: &str,
    b: &str,
    surface: super::usage::Surface,
    already_recorded: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (id, other) in [(a, b), (b, a)] {
        let Ok(m) = store.get(id).await else { continue };
        if !m
            .challenges
            .iter()
            .any(|c| crate::nli::names_nli_contradiction_with(c, other))
        {
            continue;
        }
        let other_id = other.to_string();
        let res = store
            .update_with(id, |m| {
                m.challenges
                    .retain(|c| !crate::nli::names_nli_contradiction_with(c, &other_id));
                if m.challenges.is_empty()
                    && matches!(
                        m.status,
                        crate::types::Status::Challenged | crate::types::Status::NeedsReview
                    )
                {
                    m.status = crate::types::Status::Active;
                }
                Ok(())
            })
            .await;
        match res {
            Ok(_) => {
                if already_recorded != Some(id) {
                    super::usage::record_write(
                        store,
                        id,
                        super::usage::UsageKind::Resolved,
                        surface,
                        Some(other),
                    )
                    .await;
                }
                out.push(id.to_string());
            }
            Err(e) => tracing::warn!(memory_id = id, "NLI challenge not resolved: {e:#}"),
        }
    }
    out
}

/// The status write behind [`set_link_status`] and [`review_link`].
/// `require_proposed` checks the stored status under the write lock:
/// `Ok(None)` when it already equals `status`, [`NotProposed`] for any other
/// non-proposed status. Both copies of a symmetric same-store edge are
/// updated (concurrent proposers can leave one in each file; updating only
/// the first found let the other's `proposed` win the next read).
async fn write_status(
    store: &MemoryStore,
    edge: EdgeRef,
    status: LinkStatus,
    note: Option<String>,
    surface: super::usage::Surface,
    require_proposed: bool,
) -> Result<Option<LinkResult>> {
    let note = validate_note(note)?;
    let (owner, stored, src_id, dst_id) = locate(store, &edge).await?;
    let new_status = status.clone();
    let mirror_note = note.clone();
    let mut seen: Option<LinkStatus> = None;
    let written = store
        .update_with(&owner, |m| {
            let l = m
                .links
                .iter_mut()
                .find(|l| l.same_edge(&stored))
                .ok_or_else(|| anyhow!("edge vanished before its status could be set"))?;
            if require_proposed && l.status != LinkStatus::Proposed {
                seen = Some(l.status.clone());
                bail!("edge is no longer proposed");
            }
            l.status = new_status;
            l.at = Some(Utc::now());
            if note.is_some() {
                l.note = note;
            }
            Ok(())
        })
        .await;
    if let Some(current) = seen {
        if current == status {
            return Ok(None);
        }
        return Err(NotProposed { current }.into());
    }
    written?;
    let other = if owner == src_id { &dst_id } else { &src_id };
    if stored.kind.is_symmetric() && stored.store.is_none() {
        let reverse = Link {
            to: owner.clone(),
            ..stored.clone()
        };
        let has_copy = store
            .get(other)
            .await
            .map(|m| m.links.iter().any(|l| l.same_edge(&reverse)))
            .unwrap_or(false);
        if has_copy {
            let mirror_status = status.clone();
            if let Err(e) = store
                .update_with(other, |m| {
                    if let Some(l) = m.links.iter_mut().find(|l| l.same_edge(&reverse)) {
                        l.status = mirror_status;
                        l.at = Some(Utc::now());
                        if mirror_note.is_some() {
                            l.note = mirror_note;
                        }
                    }
                    Ok(())
                })
                .await
            {
                // The owner's copy is written; reads prefer the decided
                // state (`status_rank`), so a stale `proposed` mirror loses.
                tracing::warn!(memory_id = %other, "second copy of the edge not updated: {e:#}");
            }
        }
    }
    let related = other;
    super::usage::record_write(
        store,
        &owner,
        super::usage::UsageKind::Linked,
        surface,
        Some(related),
    )
    .await;
    Ok(Some(LinkResult {
        owner_id: owner,
        src_id,
        dst_id,
        dst_store: stored.store,
        kind: stored.kind,
        status,
        created: false,
        target_state: EdgeState::Live,
        stored_note: None,
    }))
}

/// A reviewer's answer to a `contradicts` edge (plan: "Contradiction
/// direction").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContradictionVerdict {
    /// Both stand, in conflict: the edge becomes `confirmed` and both sides
    /// carry the penalty (phase 5).
    BothStand,
    /// `winner` (one of the two ids, or a prefix of it) supersedes the other:
    /// the `contradicts` edge is replaced by `winner supersedes loser`, the
    /// loser's window closes (`superseded_by = winner`) and only it is
    /// demoted.
    Supersedes { winner: String },
    /// Not a contradiction: kept as `rejected` so NLI does not propose it
    /// again.
    Reject,
}

/// What [`confirm_contradiction_direction`] did.
#[derive(Debug, Clone, Serialize)]
pub struct ContradictionOutcome {
    pub a: String,
    pub b: String,
    /// `confirmed`, `rejected`, or `superseded`.
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loser: Option<String>,
}

/// Apply a reviewer's verdict to the `contradicts` edge between `a` and `b`
/// (stored on either side). See [`ContradictionVerdict`].
///
/// Every verdict also resolves the NLI challenges the contradiction came
/// with ([`resolve_nli_challenges`]), as the dashboard's review does: the
/// pair is settled, so neither memory keeps paying a challenge penalty for
/// it.
///
/// The supersede path is three writes, ordered so an interruption leaves a
/// re-runnable state rather than a lost edge: the `supersedes` edge is added
/// first, the loser's window closed second, and the `contradicts` edge removed
/// last (both copies, when a race left one in each file) — a crash in between
/// leaves both edges, and running the same verdict again finishes the job.
pub async fn confirm_contradiction_direction(
    store: &MemoryStore,
    a: &str,
    b: &str,
    verdict: ContradictionVerdict,
    note: Option<String>,
    surface: super::usage::Surface,
) -> Result<ContradictionOutcome> {
    apply_direction(store, a, b, verdict, note, surface, false)
        .await
        .map(|(out, _)| out)
}

/// [`confirm_contradiction_direction`], plus the memories whose NLI
/// challenge it resolved. With `require_proposed` (a review of a proposed
/// edge, [`review_contradiction`]), the supersede path first **claims** the
/// edge under the owner's write lock — still `proposed`, else
/// [`NotProposed`] — by setting it `confirmed`, so a concurrent reject (or a
/// second supersede) sees it reviewed and cannot have its verdict deleted by
/// the removal at the end. Every check the later writes make runs before the
/// claim, and a failed `supersedes` write puts the edge back to `proposed`.
/// A crash after the claim leaves a confirmed contradiction (both stand); the
/// CLI's `link --winner` finishes the job from there.
async fn apply_direction(
    store: &MemoryStore,
    a: &str,
    b: &str,
    verdict: ContradictionVerdict,
    note: Option<String>,
    surface: super::usage::Surface,
    require_proposed: bool,
) -> Result<(ContradictionOutcome, Vec<String>)> {
    let edge = EdgeRef {
        src: a.to_string(),
        kind: LinkKind::Contradicts,
        dst: b.to_string(),
        dst_store: None,
    };
    let (owner, stored, a_id, b_id) = locate(store, &edge).await?;
    match verdict {
        ContradictionVerdict::BothStand | ContradictionVerdict::Reject => {
            let (status, outcome) = if verdict == ContradictionVerdict::Reject {
                (LinkStatus::Rejected, "rejected")
            } else {
                (LinkStatus::Confirmed, "confirmed")
            };
            set_link_status(store, edge, status, note, surface).await?;
            let resolved = resolve_nli_challenges(store, &a_id, &b_id, surface, None).await;
            Ok((
                ContradictionOutcome {
                    a: a_id,
                    b: b_id,
                    outcome,
                    winner: None,
                    loser: None,
                },
                resolved,
            ))
        }
        ContradictionVerdict::Supersedes { winner } => {
            let w = winner.trim();
            let (win, lose) = match (
                !w.is_empty() && a_id.starts_with(w),
                !w.is_empty() && b_id.starts_with(w),
            ) {
                (true, false) => (a_id.clone(), b_id.clone()),
                (false, true) => (b_id.clone(), a_id.clone()),
                (true, true) => refuse!("winner {winner:?} is a prefix of both {a_id} and {b_id}"),
                (false, false) => refuse!("winner {winner:?} is neither {a_id} nor {b_id}"),
            };
            // Refuse before any write when the loser was already replaced by
            // something else (a compress, an earlier verdict): rewriting its
            // `superseded_by` and `invalidated_at` would erase that history.
            let loser = store.get(&lose).await?;
            if let Some(other) = loser.superseded_by.as_deref().filter(|o| *o != win) {
                refuse!(
                    "{lose} is already superseded by {other}; resolve this contradiction as \
                     both-stand or rejected, or unlink that supersession first"
                );
            }
            let params = LinkParams {
                src: win.clone(),
                kind: LinkKind::Supersedes,
                dst: lose.clone(),
                dst_store: None,
                confidence: Some(stored.confidence),
                origin: stored.origin.clone(),
                status: Some(LinkStatus::Confirmed),
                note: validate_note(note)?.or(stored.note.clone()),
            };
            let claimed = if require_proposed {
                check_link(store, params.clone()).await?;
                claim_proposed(store, &owner, &stored).await?;
                true
            } else {
                false
            };
            if let Err(e) = link(store, params, surface).await {
                if claimed {
                    unclaim(store, &owner, &stored).await;
                }
                return Err(e);
            }
            // `link` closed the loser's window if it was live. One that was
            // already closed without a successor (`resolve invalidate`) keeps
            // its `invalidated_at` and only gains the mirror.
            let winner_id = win.clone();
            store
                .update_with(&lose, |m| {
                    if m.superseded_by.is_none() {
                        m.superseded_by = Some(winner_id);
                    }
                    Ok(())
                })
                .await?;
            store
                .update_with(&owner, |m| {
                    m.links.retain(|l| !l.same_edge(&stored));
                    Ok(())
                })
                .await?;
            let other = if owner == a_id { &b_id } else { &a_id };
            remove_reverse_copy(store, &owner, other, &stored).await?;
            // The loser's window closed: the same `resolved` event a
            // `resolve invalidate --superseded-by` records, naming the winner.
            super::usage::record_write(
                store,
                &lose,
                super::usage::UsageKind::Resolved,
                surface,
                Some(&win),
            )
            .await;
            // The loser already recorded `resolved` (its window closed).
            let resolved = resolve_nli_challenges(store, &a_id, &b_id, surface, Some(&lose)).await;
            Ok((
                ContradictionOutcome {
                    a: a_id,
                    b: b_id,
                    outcome: "superseded",
                    winner: Some(win),
                    loser: Some(lose),
                },
                resolved,
            ))
        }
    }
}

/// Under `owner`'s write lock: the edge must still be `proposed`
/// ([`NotProposed`] otherwise); it becomes `confirmed`, which is what makes
/// a concurrent review see it reviewed ([`apply_direction`]).
async fn claim_proposed(store: &MemoryStore, owner: &str, stored: &Link) -> Result<()> {
    let mut seen: Option<LinkStatus> = None;
    let res = store
        .update_with(owner, |m| {
            let l = m
                .links
                .iter_mut()
                .find(|l| l.same_edge(stored))
                .ok_or_else(|| anyhow!("edge vanished before it could be reviewed"))?;
            if l.status != LinkStatus::Proposed {
                seen = Some(l.status.clone());
                bail!("edge is no longer proposed");
            }
            l.status = LinkStatus::Confirmed;
            l.at = Some(Utc::now());
            Ok(())
        })
        .await;
    if let Some(current) = seen {
        return Err(NotProposed { current }.into());
    }
    res?;
    Ok(())
}

/// Undo [`claim_proposed`] after the verdict's first write failed, when the
/// edge is still the `confirmed` the claim left. Best-effort.
async fn unclaim(store: &MemoryStore, owner: &str, stored: &Link) {
    let res = store
        .update_with(owner, |m| {
            if let Some(l) = m
                .links
                .iter_mut()
                .find(|l| l.same_edge(stored) && l.status == LinkStatus::Confirmed)
            {
                l.status = LinkStatus::Proposed;
            }
            Ok(())
        })
        .await;
    if let Err(e) = res {
        tracing::warn!(memory_id = %owner, "a failed review left its edge confirmed: {e:#}");
    }
}

/// A pending automatic proposal (NLI or similarity), as
/// `link_proposals` counts them.
fn is_auto_pending(l: &Link) -> bool {
    l.status == LinkStatus::Proposed && matches!(l.origin, LinkOrigin::Nli | LinkOrigin::Similarity)
}

/// How decided a review state is: confirmed > rejected > proposed. A
/// reviewer's "no" outranks an unreviewed proposal, so a rejected copy of an
/// edge is never shadowed by a stale proposed one.
fn status_rank(s: &LinkStatus) -> u8 {
    match s {
        LinkStatus::Confirmed => 3,
        LinkStatus::Rejected => 2,
        LinkStatus::Proposed => 1,
        LinkStatus::Other(_) => 0,
    }
}

/// Compress/consolidation: move the sources' outbound edges onto `summary_id`.
///
/// Edges between two sources (and edges into the summary itself) are dropped
/// — they describe a relation the merge has absorbed. Inbound edges are left
/// where they are: they now point at an invalidated source whose
/// `superseded_by` names the summary, which is how readers resolve them
/// ([`EdgeView::successor`]). An edge the summary's file may not hold (the
/// personal-id rule — for a cross-store target, any target not shown to be
/// shared in its own store) stays on its source. Moved edges take `origin:
/// compress`; their status, confidence and note are kept, and they replace
/// any automatic proposal the summary already got to the same end (the sync
/// create path proposes before this runs), so a reviewed edge is never
/// downgraded or re-raised by its own compress.
///
/// Best-effort like the rest of supersession maintenance: a failure is
/// logged and never fails the compress that produced a valid summary.
pub async fn carry_links_onto(store: &MemoryStore, summary_id: &str, sources: &[Memory]) {
    if let Err(e) = carry_links_inner(store, summary_id, sources).await {
        tracing::warn!(summary = %summary_id, "could not carry source edges onto the summary: {e:#}");
    }
}

async fn carry_links_inner(
    store: &MemoryStore,
    summary_id: &str,
    sources: &[Memory],
) -> Result<()> {
    let source_ids: HashSet<&str> = sources.iter().map(|m| m.id.as_str()).collect();
    if sources.iter().all(|m| m.links.is_empty()) {
        return Ok(());
    }
    let summary = store.get(summary_id).await?;
    let committed = is_committed(store, &summary);
    // The personal-id rule needs the same-store targets' visibility.
    let targets: Vec<&str> = sources
        .iter()
        .flat_map(|m| m.links.iter())
        .filter(|l| l.store.is_none())
        .map(|l| l.to.as_str())
        .collect();
    let personal: HashSet<String> = store
        .get_batch(&targets)
        .await?
        .into_iter()
        .filter(|(_, m)| m.visibility == Visibility::Personal)
        .map(|(id, _)| id)
        .collect();
    // A cross-store target can be personal in its own store, and `link`
    // refuses that edge from a committed file just the same. A committed
    // summary takes one only when its target is shown to be shared: an
    // unopenable store or a missing memory keeps the edge on its source.
    let mut shown_shared: HashSet<(String, String)> = HashSet::new();
    if committed {
        let mut by_store: HashMap<&str, Vec<&str>> = HashMap::new();
        for l in sources.iter().flat_map(|m| m.links.iter()) {
            if let Some(sid) = l.store.as_deref() {
                by_store.entry(sid).or_default().push(l.to.as_str());
            }
        }
        for (sid, ids) in by_store {
            let Ok(other) = open_store_by_id(sid, true).await else {
                continue;
            };
            let Ok(found) = other.get_batch(&ids).await else {
                continue;
            };
            for (id, m) in found {
                if m.visibility != Visibility::Personal {
                    shown_shared.insert((sid.to_string(), id));
                }
            }
        }
    }

    let mut carried: Vec<Link> = Vec::new();
    let mut moved_from: HashMap<String, Vec<Link>> = HashMap::new();
    for source in sources {
        for link in &source.links {
            // A `supersedes` entry is metadata for the source's own canonical
            // field, which compress does not move; carried alone it would be
            // an entry with no edge behind it. It stays with its field.
            if link.kind == LinkKind::Supersedes {
                continue;
            }
            let internal = link.store.is_none()
                && (source_ids.contains(link.to.as_str()) || link.to == summary_id);
            if internal {
                moved_from
                    .entry(source.id.clone())
                    .or_default()
                    .push(link.clone());
                continue;
            }
            if committed && link.store.is_none() && personal.contains(&link.to) {
                continue; // stays on the source
            }
            if committed
                && link
                    .store
                    .as_ref()
                    .is_some_and(|sid| !shown_shared.contains(&(sid.clone(), link.to.clone())))
            {
                continue; // stays on the source
            }
            let mut moved = link.clone();
            moved.origin = LinkOrigin::Compress;
            // Two sources holding the same edge: keep the most decided review
            // state (confirmed > rejected > proposed), not whichever came first.
            match carried.iter_mut().find(|c| c.same_edge(&moved)) {
                Some(c) if status_rank(&moved.status) > status_rank(&c.status) => *c = moved,
                Some(_) => {}
                None => carried.push(moved),
            }
            moved_from
                .entry(source.id.clone())
                .or_default()
                .push(link.clone());
        }
    }
    if !carried.is_empty() {
        store
            .update_with(summary_id, |m| {
                // On the sync create path the proposers ran on the new
                // summary before this carry, and saw no edge to the sources'
                // ends. "Any existing edge blocks a proposal": an automatic
                // proposal to an end a source was already joined to goes,
                // whatever its kind, so a reviewed edge (a rejection
                // included) is carried rather than shadowed by a fresh one.
                m.links.retain(|l| {
                    !(is_auto_pending(l)
                        && carried.iter().any(|c| c.to == l.to && c.store == l.store))
                });
                for link in carried {
                    match m.links.iter_mut().find(|l| l.same_edge(&link)) {
                        Some(existing)
                            if status_rank(&link.status) > status_rank(&existing.status) =>
                        {
                            *existing = link;
                        }
                        Some(_) => {}
                        None => m.links.push(link),
                    }
                }
                Ok(())
            })
            .await?;
    }
    let ids: Vec<String> = moved_from.keys().cloned().collect();
    let (_, errors) = store
        .update_batch_with(&ids, |m| {
            if let Some(gone) = moved_from.get(&m.id) {
                m.links.retain(|l| !gone.iter().any(|g| g.same_edge(l)));
            }
            Ok(())
        })
        .await?;
    for (id, e) in errors {
        tracing::warn!(source = %id, "edges were copied to the summary but not removed from the source: {e}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Which side of the viewed memory an edge sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Out,
    In,
}

/// One edge as seen from one memory.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeView {
    pub direction: Direction,
    /// The memory at the other end.
    pub other_id: String,
    /// `None` = same store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_store: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_summary: Option<String>,
    pub kind: LinkKind,
    pub symmetric: bool,
    pub confidence: f64,
    pub origin: LinkOrigin,
    pub status: LinkStatus,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub state: EdgeState,
    /// When the other end is invalidated by supersession: the memory that
    /// replaced it, which is where an edge into a compressed source resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successor: Option<String>,
}

/// What a read hides by default. Everything `false` is the default view:
/// live and unknown edges of every status except `rejected`.
#[derive(Debug, Clone, Default)]
pub struct LinkFilter {
    pub include_dangling: bool,
    pub include_invalidated: bool,
    pub include_rejected: bool,
    /// `None` = every kind.
    pub kinds: Option<Vec<LinkKind>>,
}

impl LinkFilter {
    /// Show everything.
    pub fn all() -> Self {
        Self {
            include_dangling: true,
            include_invalidated: true,
            include_rejected: true,
            kinds: None,
        }
    }

    fn admits(&self, kind: &LinkKind, status: &LinkStatus, state: EdgeState) -> bool {
        if let Some(kinds) = &self.kinds {
            if !kinds.contains(kind) {
                return false;
            }
        }
        if !self.include_rejected && *status == LinkStatus::Rejected {
            return false;
        }
        match state {
            EdgeState::Dangling => self.include_dangling,
            EdgeState::Invalidated => self.include_invalidated,
            EdgeState::Live | EdgeState::Unknown => true,
        }
    }
}

/// A memory's edges, both directions.
#[derive(Debug, Clone, Serialize)]
pub struct LinksView {
    pub id: String,
    pub outbound: Vec<EdgeView>,
    pub inbound: Vec<EdgeView>,
    /// Edges the filter hid (dangling, invalidated, rejected, other kinds),
    /// so a view never claims a completeness it does not have.
    pub hidden: usize,
}

/// Existence and validity of the other ends, checked once per store.
struct EndStates {
    local: HashMap<String, (EdgeState, Option<String>, Option<String>)>,
    foreign: HashMap<String, Option<HashSet<String>>>,
}

impl EndStates {
    async fn gather(store: &MemoryStore, ends: &[(String, Option<String>)]) -> Result<Self> {
        let local_ids: Vec<&str> = ends
            .iter()
            .filter(|(_, s)| s.is_none())
            .map(|(id, _)| id.as_str())
            .collect();
        let local = store
            .get_batch(&local_ids)
            .await?
            .into_iter()
            .map(|(id, m)| {
                let state = if m.is_invalidated() {
                    EdgeState::Invalidated
                } else {
                    EdgeState::Live
                };
                (
                    id,
                    (state, Some(m.summary.clone()), m.superseded_by.clone()),
                )
            })
            .collect();
        let mut foreign: HashMap<String, Option<HashSet<String>>> = HashMap::new();
        let mut by_store: HashMap<&str, Vec<&str>> = HashMap::new();
        for (id, s) in ends {
            if let Some(s) = s {
                by_store.entry(s.as_str()).or_default().push(id.as_str());
            }
        }
        for (sid, ids) in by_store {
            let found = match open_store_by_id(sid, true).await {
                Ok(other) => other.batch_exists(&ids).await.ok(),
                Err(_) => None,
            };
            foreign.insert(sid.to_string(), found);
        }
        Ok(Self { local, foreign })
    }

    fn state(&self, id: &str, store: Option<&str>) -> (EdgeState, Option<String>, Option<String>) {
        match store {
            None => self
                .local
                .get(id)
                .cloned()
                .unwrap_or((EdgeState::Dangling, None, None)),
            Some(s) => match self.foreign.get(s) {
                Some(Some(found)) if found.contains(id) => (EdgeState::Live, None, None),
                Some(Some(_)) => (EdgeState::Dangling, None, None),
                _ => (EdgeState::Unknown, None, None),
            },
        }
    }
}

/// `id`'s edges in both directions, with the far ends checked.
///
/// An inbound edge's far end is always in this store (the table holds only
/// edges declared by this store's files); an outbound one may be anywhere.
pub async fn links_of(store: &MemoryStore, id: &str, filter: &LinkFilter) -> Result<LinksView> {
    let full = store.resolve_id(id).await?;
    let rows = store.edges_touching(&full).await?;
    let ends: Vec<(String, Option<String>)> = rows
        .iter()
        .map(|r| {
            if r.src_id == full {
                (r.dst_id.clone(), r.dst_store.clone())
            } else {
                (r.src_id.clone(), None)
            }
        })
        .collect();
    let states = EndStates::gather(store, &ends).await?;
    let mut view = LinksView {
        id: full.clone(),
        outbound: Vec::new(),
        inbound: Vec::new(),
        hidden: 0,
    };
    for row in rows {
        let out = row.src_id == full;
        let (other, other_store) = if out {
            (row.dst_id.clone(), row.dst_store.clone())
        } else {
            (row.src_id.clone(), None)
        };
        let (state, summary, successor) = states.state(&other, other_store.as_deref());
        if !filter.admits(&row.kind, &row.status, state) {
            view.hidden += 1;
            continue;
        }
        let edge = EdgeView {
            direction: if out { Direction::Out } else { Direction::In },
            other_id: other,
            other_store,
            other_summary: summary,
            symmetric: row.kind.is_symmetric(),
            kind: row.kind,
            confidence: row.confidence,
            origin: row.origin,
            status: row.status,
            created_at: row.created_at,
            note: row.note,
            state,
            successor: if state == EdgeState::Invalidated {
                successor
            } else {
                None
            },
        };
        if out {
            view.outbound.push(edge);
        } else {
            view.inbound.push(edge);
        }
    }
    let order = |a: &EdgeView, b: &EdgeView| {
        (a.kind.as_str(), &a.other_id).cmp(&(b.kind.as_str(), &b.other_id))
    };
    view.outbound.sort_by(order);
    view.inbound.sort_by(order);
    Ok(view)
}

// ---------------------------------------------------------------------------
// Graph read API (the dashboard's graph view)
// ---------------------------------------------------------------------------

/// What [`load_graph`] returns.
#[derive(Debug, Clone, Default)]
pub struct GraphFilter {
    /// `None` = every kind.
    pub kinds: Option<Vec<LinkKind>>,
    /// Include `proposed` edges (the review queue). Default off: a proposal
    /// is not yet a claim.
    pub include_proposed: bool,
    pub include_rejected: bool,
    /// Include invalidated memories as nodes, and edges touching them.
    pub include_invalidated: bool,
}

/// A memory as a graph node.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub summary: String,
    #[serde(rename = "type")]
    pub type_: MemoryType,
    pub status: Status,
    pub visibility: Visibility,
    pub invalidated: bool,
}

/// An edge as the graph draws it.
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub src: String,
    pub dst: String,
    /// `None` = same store; otherwise the target is outside this graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_store: Option<String>,
    pub kind: LinkKind,
    pub symmetric: bool,
    pub confidence: f64,
    pub origin: LinkOrigin,
    pub status: LinkStatus,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One store's graph.
#[derive(Debug, Clone, Serialize)]
pub struct StoreGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Same-store edges left out because an end no longer exists.
    pub dangling: usize,
    /// Those of them whose *source* is still a shown node — the ones a view
    /// can draw as a stub to a deleted-memory mark. (A dangling edge whose source
    /// is gone is a `superseded_by` mirror with nothing left to draw from.)
    pub dangling_edges: Vec<GraphEdge>,
    /// Edges into another store, which this view does not open: included in
    /// `edges` (with `dst_store` set), never counted as dangling.
    pub external: usize,
}

/// Load a store's edges and node set, side-effect free.
///
/// Reads the index only (the `memories` projection and the `edges` table)
/// and creates nothing, so it is safe on a store from
/// [`MemoryStore::open_read_only`]; a store without an `edges` table (not yet
/// migrated to 0.9.0) has no edges. Other stores are not opened: a
/// cross-store edge is returned with its `dst_store` for the caller, which
/// knows which stores it is showing, to resolve.
pub async fn load_graph(store: &MemoryStore, filter: &GraphFilter) -> Result<StoreGraph> {
    let rows = store.list_filterable().await?;
    Ok(graph_of(&rows, store.list_edges().await?, filter))
}

/// [`load_graph`] over rows the caller already read: the memories projection
/// (`MemoryStore::list_filterable`, invalidated rows included) and the
/// deduplicated edges (`MemoryStore::list_edges`). Pure, so a reader that
/// needs the projection anyway (the dashboard) reads it once, not twice.
pub fn graph_of(
    rows: &[crate::storage::lance_index::IndexFilterable],
    edges: Vec<EdgeRow>,
    filter: &GraphFilter,
) -> StoreGraph {
    let now = Utc::now();
    let mut all_ids: HashSet<&str> = HashSet::new();
    let mut shown: HashSet<&str> = HashSet::new();
    let mut nodes = Vec::new();
    for r in rows {
        let invalidated = r.invalidated_at.is_some_and(|t| now >= t);
        all_ids.insert(r.id.as_str());
        if invalidated && !filter.include_invalidated {
            continue;
        }
        shown.insert(r.id.as_str());
        nodes.push(GraphNode {
            id: r.id.clone(),
            summary: r.summary.clone(),
            type_: r.type_,
            status: r.status,
            visibility: r.visibility,
            invalidated,
        });
    }
    let mut graph = StoreGraph {
        nodes,
        edges: Vec::new(),
        dangling: 0,
        dangling_edges: Vec::new(),
        external: 0,
    };
    for row in edges {
        if let Some(kinds) = &filter.kinds {
            if !kinds.contains(&row.kind) {
                continue;
            }
        }
        match row.status {
            LinkStatus::Proposed if !filter.include_proposed => continue,
            LinkStatus::Rejected if !filter.include_rejected => continue,
            _ => {}
        }
        if !all_ids.contains(row.src_id.as_str()) {
            graph.dangling += 1;
            continue;
        }
        if row.dst_store.is_none() && !all_ids.contains(row.dst_id.as_str()) {
            graph.dangling += 1;
            if shown.contains(row.src_id.as_str()) {
                graph.dangling_edges.push(GraphEdge::from_row(row));
            }
            continue;
        }
        if !shown.contains(row.src_id.as_str())
            || (row.dst_store.is_none() && !shown.contains(row.dst_id.as_str()))
        {
            continue; // an end is invalidated and hidden
        }
        if row.dst_store.is_some() {
            graph.external += 1;
        }
        graph.edges.push(GraphEdge::from_row(row));
    }
    graph
}

impl GraphEdge {
    fn from_row(row: EdgeRow) -> Self {
        Self {
            src: row.src_id,
            dst: row.dst_id,
            dst_store: row.dst_store,
            symmetric: row.kind.is_symmetric(),
            kind: row.kind,
            confidence: row.confidence,
            origin: row.origin,
            status: row.status,
            created_at: row.created_at,
            note: row.note,
        }
    }
}

// ---------------------------------------------------------------------------
// Health (doctor)
// ---------------------------------------------------------------------------

/// A same-store edge whose far end is gone.
#[derive(Debug, Clone, Serialize)]
pub struct DanglingEdge {
    pub src: String,
    pub dst: String,
    pub kind: LinkKind,
}

/// What `doctor` reports about a store's edges. Every finding here is legal
/// state (module docs) or derived-table drift, reported so it can be tidied;
/// none of it is a failure.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EdgeHealth {
    pub total: usize,
    pub dangling: Vec<DanglingEdge>,
    /// Cross-store edges whose store could not be opened (or read) here.
    pub unknown: usize,
    /// Committed (shared, project-store) memories whose file names a
    /// personal memory — the leak the write rules refuse, acquired some
    /// other way (a hand edit, a file from an older binary, the declared
    /// `link`/visibility race). A cross-store target counts when its own
    /// store opens here and calls it personal. Checked on the
    /// **un-deduplicated** rows: dedupe folds a `superseded_by` mirror into
    /// its `supersedes` row, and that mirror is the one in the other file.
    pub personal_leaks: Vec<DanglingEdge>,
    /// Edges of a kind this binary does not know (a newer binary wrote them).
    pub unknown_kinds: usize,
    /// Rows the edges table should hold but does not, plus rows it holds that
    /// no file declares. The table is derived and its writes never fail a
    /// memory write, so a failed one surfaces here; `reindex` rebuilds it.
    pub table_drift: usize,
}

/// Check every edge of `store`. Side-effect free.
pub async fn edge_health(store: &MemoryStore) -> Result<EdgeHealth> {
    let raw = store.list_edge_rows().await?;

    // What the files declare, for the drift count. A file that does not
    // parse is skipped by `get_batch`; its preserved rows are not drift.
    let ids = store.list_ids().await?;
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let memories = store.get_batch(&id_refs).await?;
    let parsed: HashSet<&str> = memories.iter().map(|(id, _)| id.as_str()).collect();
    let expected: HashSet<String> = memories
        .iter()
        .flat_map(|(_, m)| crate::storage::edge_index::edges_of(m))
        .map(|r| r.row_key())
        .collect();
    let actual: HashSet<String> = raw
        .iter()
        .filter(|r| parsed.contains(r.owner_id.as_str()))
        .map(|r| r.row_key())
        .collect();
    let table_drift = expected.symmetric_difference(&actual).count();

    let edges = crate::storage::edge_index::dedupe(raw.clone());
    let mut health = EdgeHealth {
        total: edges.len(),
        table_drift,
        ..Default::default()
    };
    if edges.is_empty() {
        return Ok(health);
    }
    let visibility: HashMap<String, Visibility> = store
        .list_filterable()
        .await?
        .into_iter()
        .map(|r| (r.id, r.visibility))
        .collect();
    let committed_store = !store.is_global() && !store.is_group();
    let mut foreign: HashMap<String, Vec<String>> = HashMap::new();
    for e in &edges {
        if !e.kind.is_known() {
            health.unknown_kinds += 1;
        }
        if let Some(s) = &e.dst_store {
            foreign.entry(s.clone()).or_default().push(e.dst_id.clone());
            continue;
        }
        let gone = !visibility.contains_key(&e.src_id) || !visibility.contains_key(&e.dst_id);
        if gone {
            health.dangling.push(DanglingEdge {
                src: e.src_id.clone(),
                dst: e.dst_id.clone(),
                kind: e.kind.clone(),
            });
        }
    }
    if committed_store {
        for r in raw.iter().filter(|r| r.dst_store.is_none()) {
            let other = if r.owner_id == r.src_id {
                &r.dst_id
            } else {
                &r.src_id
            };
            if visibility.get(&r.owner_id) == Some(&Visibility::Shared)
                && visibility.get(other) == Some(&Visibility::Personal)
            {
                let leak = DanglingEdge {
                    src: r.src_id.clone(),
                    dst: r.dst_id.clone(),
                    kind: r.kind.clone(),
                };
                if !health
                    .personal_leaks
                    .iter()
                    .any(|l| l.src == leak.src && l.dst == leak.dst && l.kind == leak.kind)
                {
                    health.personal_leaks.push(leak);
                }
            }
        }
    }
    for (sid, ids) in foreign {
        // Absence of evidence is not evidence of deletion: a store that will
        // not open, or will not answer, leaves its edges unknown.
        let opened = open_store_by_id(&sid, true).await.ok();
        let found = match &opened {
            Some(other) => other.batch_exists(&ids).await.ok(),
            None => None,
        };
        let Some(found) = found else {
            health.unknown += ids.len();
            continue;
        };
        if committed_store {
            // A cross-store target that is personal in its own store leaks
            // its id just the same (`link` refuses it; compress keeps it on
            // the source). The owner of a cross-store row is its source.
            let leaking: Vec<&EdgeRow> = raw
                .iter()
                .filter(|r| {
                    r.dst_store.as_deref() == Some(sid.as_str())
                        && visibility.get(&r.owner_id) == Some(&Visibility::Shared)
                })
                .collect();
            if !leaking.is_empty() {
                let dst: Vec<&str> = leaking.iter().map(|r| r.dst_id.as_str()).collect();
                let personal: HashSet<String> = match &opened {
                    Some(other) => other
                        .get_batch(&dst)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|(_, m)| m.visibility == Visibility::Personal)
                        .map(|(id, _)| id)
                        .collect(),
                    None => HashSet::new(),
                };
                for r in leaking.into_iter().filter(|r| personal.contains(&r.dst_id)) {
                    if !health
                        .personal_leaks
                        .iter()
                        .any(|l| l.src == r.src_id && l.dst == r.dst_id && l.kind == r.kind)
                    {
                        health.personal_leaks.push(DanglingEdge {
                            src: r.src_id.clone(),
                            dst: r.dst_id.clone(),
                            kind: r.kind.clone(),
                        });
                    }
                }
            }
        }
        for e in edges
            .iter()
            .filter(|e| e.dst_store.as_deref() == Some(sid.as_str()))
        {
            if !found.contains(&e.dst_id) {
                health.dangling.push(DanglingEdge {
                    src: e.src_id.clone(),
                    dst: e.dst_id.clone(),
                    kind: e.kind.clone(),
                });
            }
        }
    }
    Ok(health)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::usage::Surface;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Provenance, Visibility};
    use tempfile::TempDir;

    async fn store() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        (tmp, store)
    }

    async fn mem(store: &MemoryStore, summary: &str, visibility: Visibility) -> String {
        let mut m = Memory::new(
            MemoryType::Decision,
            summary,
            "content",
            Provenance::human(),
        );
        m.visibility = visibility;
        store.create(&m).await.unwrap()
    }

    fn params(src: &str, kind: LinkKind, dst: &str) -> LinkParams {
        LinkParams {
            src: src.into(),
            kind,
            dst: dst.into(),
            dst_store: None,
            confidence: None,
            origin: LinkOrigin::Human,
            status: None,
            note: None,
        }
    }

    fn upd() -> crate::ops::UpdateParams {
        crate::ops::UpdateParams {
            type_: None,
            content: None,
            summary: None,
            title: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            details: None,
            visibility: None,
            status: None,
            supersedes: None,
            audience: None,
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: false,
            clear_invalidated: false,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            embed_async: false,
        }
    }

    fn eref(src: &str, kind: LinkKind, dst: &str) -> EdgeRef {
        EdgeRef {
            src: src.into(),
            kind,
            dst: dst.into(),
            dst_store: None,
        }
    }

    #[tokio::test]
    async fn symmetric_edge_is_stored_once_and_seen_from_both_ends() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;

        let r = link(
            &store,
            params(&a[..24], LinkKind::RelatedTo, &b[..24]),
            Surface::Cli,
        )
        .await
        .unwrap();
        assert!(r.created);
        assert_eq!(
            (r.src_id.as_str(), r.dst_id.as_str()),
            (a.as_str(), b.as_str())
        );
        assert_eq!(
            store.get(&a).await.unwrap().links[0].to,
            b,
            "full id, never a prefix"
        );

        // The reverse direction updates the existing copy instead of adding one.
        let mut rev = params(&b, LinkKind::RelatedTo, &a);
        rev.confidence = Some(0.4);
        let r = link(&store, rev, Surface::Cli).await.unwrap();
        assert!(!r.created);
        assert_eq!(r.owner_id, a);
        assert!(store.get(&b).await.unwrap().links.is_empty());
        assert_eq!(store.get(&a).await.unwrap().links[0].confidence, 0.4);

        let from_a = links_of(&store, &a, &LinkFilter::default()).await.unwrap();
        let from_b = links_of(&store, &b, &LinkFilter::default()).await.unwrap();
        assert_eq!(from_a.outbound.len(), 1);
        assert_eq!(from_b.inbound.len(), 1);
        assert_eq!(from_b.inbound[0].other_id, a);
        assert!(from_b.inbound[0].symmetric);
        assert_eq!(from_b.inbound[0].other_summary.as_deref(), Some("a"));

        // Usage: every write records `linked` on the file it changed.
        let events = crate::ops::usage::tests::logged(&store);
        assert!(events
            .iter()
            .any(|e| e.kind == crate::ops::usage::UsageKind::Linked && e.memory_id == a));

        // Unlinking from either end removes the one copy.
        unlink(&store, eref(&b, LinkKind::RelatedTo, &a), Surface::Cli)
            .await
            .unwrap();
        assert!(store.list_edges().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn bad_input_is_refused() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        assert!(
            link(&store, params(&a, LinkKind::Supports, &a), Surface::Cli)
                .await
                .is_err()
        );
        assert!(
            link(&store, params(&a, LinkKind::Supports, "nope"), Surface::Cli)
                .await
                .is_err()
        );
        assert!(link(
            &store,
            params(&a, LinkKind::Other("x".into()), &a),
            Surface::Cli
        )
        .await
        .is_err());
        let b = mem(&store, "b", Visibility::Shared).await;
        let mut p = params(&a, LinkKind::Supports, &b);
        p.confidence = Some(1.5);
        assert!(link(&store, p, Surface::Cli).await.is_err());
    }

    /// The plan's "personal ids leaking into git" risk.
    #[tokio::test]
    async fn committed_memory_never_names_a_personal_one() {
        let (_t, store) = store().await;
        let shared = mem(&store, "shared", Visibility::Shared).await;
        let personal = mem(&store, "personal", Visibility::Personal).await;

        // Directed: refused, with the remedy.
        let err = link(
            &store,
            params(&shared, LinkKind::Supports, &personal),
            Surface::Cli,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("leak"), "{err}");
        assert!(store.get(&shared).await.unwrap().links.is_empty());

        // Symmetric: stored on the personal side, same meaning.
        let r = link(
            &store,
            params(&shared, LinkKind::Contradicts, &personal),
            Surface::Cli,
        )
        .await
        .unwrap();
        assert_eq!(r.owner_id, personal);
        assert!(r.stored_note.is_some());
        assert!(store.get(&shared).await.unwrap().links.is_empty());
        assert_eq!(store.get(&personal).await.unwrap().links[0].to, shared);

        // The other direction (personal → shared) is fine as written.
        link(
            &store,
            params(&personal, LinkKind::Supports, &shared),
            Surface::Cli,
        )
        .await
        .unwrap();

        // A visibility change after linking is what doctor catches.
        store
            .update_with(&personal, |m| {
                m.visibility = Visibility::Shared;
                Ok(())
            })
            .await
            .unwrap();
        let p2 = mem(&store, "p2", Visibility::Personal).await;
        link(
            &store,
            params(&p2, LinkKind::Supports, &shared),
            Surface::Cli,
        )
        .await
        .unwrap();
        store
            .update_with(&shared, |m| {
                m.links
                    .push(Link::new(&p2, LinkKind::Supports, LinkOrigin::Human));
                Ok(())
            })
            .await
            .unwrap();
        let health = edge_health(&store).await.unwrap();
        assert_eq!(health.personal_leaks.len(), 1, "{health:?}");
    }

    #[tokio::test]
    async fn supersedes_writes_the_canonical_fields_and_unlink_reopens() {
        let (_t, store) = store().await;
        let new = mem(&store, "new", Visibility::Shared).await;
        let old = mem(&store, "old", Visibility::Shared).await;
        let mut p = params(&new, LinkKind::Supersedes, &old);
        p.note = Some("v2 replaces v1".into());
        link(&store, p, Surface::Cli).await.unwrap();

        let n = store.get(&new).await.unwrap();
        assert_eq!(n.supersedes, vec![old.clone()]);
        let o = store.get(&old).await.unwrap();
        assert_eq!(o.superseded_by.as_deref(), Some(new.as_str()));
        assert!(o.is_invalidated());
        let edges = store.list_edges().await.unwrap();
        assert_eq!(
            edges.len(),
            1,
            "the field, the link and the mirror are one edge"
        );
        assert_eq!(edges[0].note.as_deref(), Some("v2 replaces v1"));

        // Invalidated targets keep the edge but hide it by default.
        let view = links_of(&store, &new, &LinkFilter::default())
            .await
            .unwrap();
        assert!(view.outbound.is_empty());
        assert_eq!(view.hidden, 1);
        let all = links_of(&store, &new, &LinkFilter::all()).await.unwrap();
        assert_eq!(all.outbound[0].state, EdgeState::Invalidated);

        let r = unlink(&store, eref(&new, LinkKind::Supersedes, &old), Surface::Cli)
            .await
            .unwrap();
        assert!(r.reopened);
        assert!(store.get(&new).await.unwrap().supersedes.is_empty());
        let o = store.get(&old).await.unwrap();
        assert!(o.superseded_by.is_none() && o.invalidated_at.is_none());
        assert!(store.list_edges().await.unwrap().is_empty());
    }

    async fn proposed_contradiction(store: &MemoryStore) -> (String, String) {
        let a = mem(store, "a", Visibility::Shared).await;
        let b = mem(store, "b", Visibility::Shared).await;
        let mut p = params(&b, LinkKind::Contradicts, &a);
        p.origin = LinkOrigin::Nli;
        p.status = Some(LinkStatus::Proposed);
        p.confidence = Some(0.87);
        link(store, p, Surface::Cli).await.unwrap();
        (a, b)
    }

    #[tokio::test]
    async fn contradiction_verdicts() {
        // Both stand: confirmed, nothing invalidated.
        let (_t, store) = store().await;
        let (a, b) = proposed_contradiction(&store).await;
        let out = confirm_contradiction_direction(
            &store,
            &a,
            &b,
            ContradictionVerdict::BothStand,
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        assert_eq!(out.outcome, "confirmed");
        assert_eq!(
            store.get(&b).await.unwrap().links[0].status,
            LinkStatus::Confirmed
        );
        assert!(!store.get(&a).await.unwrap().is_invalidated());

        // Reject: kept, as rejected — and hidden from the default view.
        let (a, b) = proposed_contradiction(&store).await;
        confirm_contradiction_direction(
            &store,
            &a,
            &b,
            ContradictionVerdict::Reject,
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        assert_eq!(
            store.get(&b).await.unwrap().links[0].status,
            LinkStatus::Rejected
        );
        assert!(links_of(&store, &a, &LinkFilter::default())
            .await
            .unwrap()
            .inbound
            .is_empty());

        // "A supersedes B": the edge becomes supersedes A → B, B is invalidated.
        let (a, b) = proposed_contradiction(&store).await;
        let out = confirm_contradiction_direction(
            &store,
            &a,
            &b,
            ContradictionVerdict::Supersedes {
                winner: a[..24].to_string(),
            },
            Some("reviewed".into()),
            Surface::Cli,
        )
        .await
        .unwrap();
        assert_eq!(out.winner.as_deref(), Some(a.as_str()));
        let am = store.get(&a).await.unwrap();
        let bm = store.get(&b).await.unwrap();
        assert_eq!(am.supersedes, vec![b.clone()]);
        assert_eq!(bm.superseded_by.as_deref(), Some(a.as_str()));
        assert!(bm.is_invalidated() && !am.is_invalidated());
        assert!(bm.links.is_empty(), "the contradicts edge is gone");
        let sup = am
            .links
            .iter()
            .find(|l| l.kind == LinkKind::Supersedes)
            .unwrap();
        assert_eq!(sup.origin, LinkOrigin::Nli);
        assert_eq!(sup.note.as_deref(), Some("reviewed"));
    }

    /// Review p3-ops #1: the reverse direction of a supersession leaks too —
    /// `personal supersedes shared` would write the personal id into the
    /// shared file's `superseded_by`. Refused before any write, on `link`,
    /// on `--winner`, and on `create --supersedes`.
    #[tokio::test]
    async fn personal_cannot_supersede_shared_in_any_path() {
        let (_t, store) = store().await;
        let s = mem(&store, "shared", Visibility::Shared).await;
        let p = mem(&store, "personal", Visibility::Personal).await;
        let err = link(&store, params(&p, LinkKind::Supersedes, &s), Surface::Cli)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("superseded_by"), "{err:#}");
        let sm = store.get(&s).await.unwrap();
        assert!(sm.superseded_by.is_none() && !sm.is_invalidated());
        assert!(store.get(&p).await.unwrap().supersedes.is_empty());

        // The winner path goes through the same check.
        let mut c = params(&p, LinkKind::Contradicts, &s);
        c.status = Some(LinkStatus::Proposed);
        link(&store, c, Surface::Cli).await.unwrap();
        let err = confirm_contradiction_direction(
            &store,
            &p,
            &s,
            ContradictionVerdict::Supersedes { winner: p.clone() },
            None,
            Surface::Cli,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("superseded_by"), "{err:#}");
        assert!(store.get(&s).await.unwrap().superseded_by.is_none());

        // And create: a personal memory created with `supersedes = [shared]`.
        let cp = crate::ops::CreateParams {
            type_: MemoryType::Decision,
            content: "c".into(),
            summary: "new personal".into(),
            title: None,
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            confidence: 0.8,
            details: None,
            visibility: Visibility::Personal,
            provenance: crate::types::Provenance::human(),
            supersedes: vec![s.clone()],
            audience: None,
            epistemic: None,
            premise: None,
            invalidated_by: vec![],
            origin_task: None,
            generality: None,
            valid_from: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            title_strategy: crate::title::TitleStrategy::None,
            embed_async: false,
        };
        assert!(crate::ops::create_memory(&store, cp, None, Surface::Cli)
            .await
            .is_err());
        assert!(store.get(&s).await.unwrap().superseded_by.is_none());
    }

    /// Review p3-ops #2 / lead decision (b): a supersession exists only as
    /// confirmed.
    #[tokio::test]
    async fn supersedes_is_never_proposed_or_rejected() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        for status in [LinkStatus::Proposed, LinkStatus::Rejected] {
            let mut p = params(&a, LinkKind::Supersedes, &b);
            p.status = Some(status);
            assert!(link(&store, p, Surface::Cli).await.is_err());
            assert!(!store.get(&b).await.unwrap().is_invalidated());
        }
        link(&store, params(&a, LinkKind::Supersedes, &b), Surface::Cli)
            .await
            .unwrap();
        let err = set_link_status(
            &store,
            eref(&a, LinkKind::Supersedes, &b),
            LinkStatus::Rejected,
            None,
            Surface::Cli,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("unlink"), "{err:#}");
        assert!(store.get(&b).await.unwrap().is_invalidated());
    }

    /// Review p3-ops #3 / #4 / lead decision (d): a proposal never touches an
    /// existing edge, and a status-only change keeps the metadata.
    #[tokio::test]
    async fn proposals_never_overwrite_and_status_changes_keep_metadata() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        // challenge --against twice: one edge, first note kept.
        link(&store, challenge_edge(&a, &b, "first"), Surface::Cli)
            .await
            .unwrap();
        let r = link(&store, challenge_edge(&a, &b, "second"), Surface::Cli)
            .await
            .unwrap();
        assert!(!r.created);
        let am = store.get(&a).await.unwrap();
        assert_eq!(am.links.len(), 1);
        assert_eq!(am.links[0].note.as_deref(), Some("first"));

        // A reviewer rejects it with a note; a re-proposal changes nothing.
        set_link_status(
            &store,
            eref(&a, LinkKind::Contradicts, &b),
            LinkStatus::Rejected,
            Some("not a conflict".into()),
            Surface::Cli,
        )
        .await
        .unwrap();
        let before = store.get(&a).await.unwrap();
        let r = link(&store, challenge_edge(&b, &a, "again"), Surface::Cli)
            .await
            .unwrap();
        assert_eq!(r.status, LinkStatus::Rejected, "reported as it stands");
        let after = store.get(&a).await.unwrap();
        assert_eq!(after.links, before.links);
        assert_eq!(after.updated_at, before.updated_at, "no write at all");

        // A confirmed edge is not downgraded either.
        let mut p = params(&a, LinkKind::Contradicts, &b);
        p.status = Some(LinkStatus::Confirmed);
        link(&store, p, Surface::Cli).await.unwrap();
        link(&store, challenge_edge(&a, &b, "x"), Surface::Cli)
            .await
            .unwrap();
        assert_eq!(
            store.get(&a).await.unwrap().links[0].status,
            LinkStatus::Confirmed
        );

        // A status-only re-link keeps origin, confidence and note.
        let mut p = params(&b, LinkKind::Supports, &a);
        p.origin = LinkOrigin::Nli;
        p.confidence = Some(0.4);
        p.note = Some("evidence".into());
        p.status = Some(LinkStatus::Proposed);
        link(&store, p, Surface::Cli).await.unwrap();
        let mut p = params(&b, LinkKind::Supports, &a);
        p.status = Some(LinkStatus::Rejected);
        link(&store, p, Surface::Cli).await.unwrap();
        let l = store.get(&b).await.unwrap().links[0].clone();
        assert_eq!(l.status, LinkStatus::Rejected);
        assert_eq!(l.origin, LinkOrigin::Nli);
        assert_eq!(l.confidence, 0.4);
        assert_eq!(l.note.as_deref(), Some("evidence"));
    }

    /// Review p3-ops #5: a verdict must not rewrite the history of a loser
    /// that something else already superseded.
    #[tokio::test]
    async fn winner_refuses_a_loser_superseded_by_another() {
        let (_t, store) = store().await;
        let (a, b) = proposed_contradiction(&store).await;
        let c = mem(&store, "c", Visibility::Shared).await;
        link(&store, params(&c, LinkKind::Supersedes, &b), Surface::Cli)
            .await
            .unwrap();
        let before = store.get(&b).await.unwrap();
        let err = confirm_contradiction_direction(
            &store,
            &a,
            &b,
            ContradictionVerdict::Supersedes { winner: a.clone() },
            None,
            Surface::Cli,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("already superseded"), "{err:#}");
        let after = store.get(&b).await.unwrap();
        assert_eq!(after.superseded_by, before.superseded_by);
        assert_eq!(after.invalidated_at, before.invalidated_at);
        assert!(
            store.get(&a).await.unwrap().supersedes.is_empty(),
            "no write"
        );
        assert!(after.links.iter().any(|l| l.kind == LinkKind::Contradicts));
    }

    /// Review p3-storage #7: unlinking one of two supersessions keeps the
    /// target closed and points its mirror at the other.
    #[tokio::test]
    async fn unlink_supersedes_hands_the_mirror_to_another_superseder() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let c = mem(&store, "c", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        link(&store, params(&a, LinkKind::Supersedes, &b), Surface::Cli)
            .await
            .unwrap();
        // C also supersedes B (canonical field only: `link` refuses a second
        // superseder, `create`/`update --supersedes` does not).
        crate::ops::update_memory(
            &store,
            &c,
            crate::ops::UpdateParams {
                supersedes: Some(vec![b.clone()]),
                ..upd()
            },
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        let r = unlink(&store, eref(&a, LinkKind::Supersedes, &b), Surface::Cli)
            .await
            .unwrap();
        assert!(!r.reopened);
        let bm = store.get(&b).await.unwrap();
        assert!(bm.is_invalidated());
        assert_eq!(bm.superseded_by.as_deref(), Some(c.as_str()));
    }

    /// Review p3-storage #3 / lead decision (c): visibility changes that would
    /// leak are refused, both ways, before any write.
    #[tokio::test]
    async fn visibility_change_cannot_leak_a_personal_id() {
        let (_t, store) = store().await;
        let p1 = mem(&store, "p1", Visibility::Personal).await;
        let p2 = mem(&store, "p2", Visibility::Personal).await;
        link(&store, params(&p1, LinkKind::Supports, &p2), Surface::Cli)
            .await
            .unwrap();
        let to = |v| crate::ops::UpdateParams {
            visibility: Some(v),
            ..upd()
        };
        // p1 → shared would commit a file naming p2.
        assert!(
            crate::ops::update_memory(&store, &p1, to(Visibility::Shared), None, Surface::Cli)
                .await
                .is_err()
        );
        assert_eq!(
            store.get(&p1).await.unwrap().visibility,
            Visibility::Personal
        );

        // s → personal while a shared file names s.
        let s1 = mem(&store, "s1", Visibility::Shared).await;
        let s2 = mem(&store, "s2", Visibility::Shared).await;
        link(&store, params(&s1, LinkKind::Supports, &s2), Surface::Cli)
            .await
            .unwrap();
        assert!(crate::ops::update_memory(
            &store,
            &s2,
            to(Visibility::Personal),
            None,
            Surface::Cli
        )
        .await
        .is_err());
        assert_eq!(store.get(&s2).await.unwrap().visibility, Visibility::Shared);
        // With no edge in the way the change goes through.
        let s3 = mem(&store, "s3", Visibility::Shared).await;
        crate::ops::update_memory(&store, &s3, to(Visibility::Personal), None, Surface::Cli)
            .await
            .unwrap();
    }

    /// Review p3-storage #2: the leak check sees the `superseded_by` mirror,
    /// which dedupe folds away.
    #[tokio::test]
    async fn edge_health_flags_a_superseded_by_leak() {
        let (_t, store) = store().await;
        let p = mem(&store, "p", Visibility::Personal).await;
        // A shared memory whose committed file says `superseded_by = p` —
        // what an older binary (or a hand edit) could leave behind.
        let mut s = Memory::new(
            MemoryType::Decision,
            "s",
            "c",
            crate::types::Provenance::human(),
        );
        s.superseded_by = Some(p.clone());
        let s = store.create(&s).await.unwrap();
        store
            .update_with(&p, |m| {
                m.supersedes.push(s.clone());
                Ok(())
            })
            .await
            .unwrap();
        let h = edge_health(&store).await.unwrap();
        assert_eq!(h.personal_leaks.len(), 1, "{h:?}");
        assert_eq!(h.personal_leaks[0].src, p);
        assert_eq!(h.personal_leaks[0].dst, s);
        assert_eq!(h.table_drift, 0, "{h:?}");
    }

    /// Cross-store edges against a real, openable group store: checked as
    /// live, then dangling once the target is deleted.
    #[tokio::test]
    async fn cross_store_edge_into_a_group_store() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let name = format!("links-test-{}", uuid::Uuid::now_v7().simple());
        let gid = crate::storage::paths::compute_group_id(&name);
        let group = MemoryStore::open_group(&gid).await.unwrap();
        let g = mem(&group, "in the group", Visibility::Shared).await;
        let mut p = params(&a, LinkKind::Supports, &g[..24]);
        p.dst_store = Some(format!("group:{name}"));
        let r = link(&store, p, Surface::Cli).await.unwrap();
        assert_eq!(r.dst_store.as_deref(), Some(gid.as_str()));
        assert_eq!(r.dst_id, g, "the prefix resolved in the other store");
        assert_eq!(r.target_state, EdgeState::Live);
        let v = links_of(&store, &a, &LinkFilter::default()).await.unwrap();
        assert_eq!(v.outbound.len(), 1);
        assert_eq!(v.outbound[0].state, EdgeState::Live);
        assert_eq!(edge_health(&store).await.unwrap().unknown, 0);

        group.delete(&g).await.unwrap();
        let v = links_of(&store, &a, &LinkFilter::all()).await.unwrap();
        assert_eq!(v.outbound[0].state, EdgeState::Dangling);
        assert_eq!(edge_health(&store).await.unwrap().dangling.len(), 1);
    }

    /// `resolve --keep` clears challenges; it must not take the edge too.
    #[tokio::test]
    async fn resolve_keep_does_not_remove_contradiction_edges() {
        let (_t, store) = store().await;
        let (a, b) = proposed_contradiction(&store).await;
        crate::ops::challenge_memory(&store, &b, "conflicts with a", None, Surface::Cli)
            .await
            .unwrap();
        crate::ops::resolve_memory(
            &store,
            crate::ops::ResolveParams {
                id: b.clone(),
                action: crate::ops::ResolveAction::Keep,
                updated_content: None,
                updated_summary: None,
                superseded_by: None,
            },
            Surface::Cli,
        )
        .await
        .unwrap();
        let bm = store.get(&b).await.unwrap();
        assert!(bm.challenges.is_empty());
        assert_eq!(bm.links.len(), 1);
        assert_eq!(bm.links[0].to, a);
    }

    #[tokio::test]
    async fn deleted_targets_dangle_and_doctor_reports_them() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        link(&store, params(&a, LinkKind::Supports, &b), Surface::Cli)
            .await
            .unwrap();
        store.delete(&b).await.unwrap();

        assert_eq!(
            store.get(&a).await.unwrap().links.len(),
            1,
            "a's file is untouched"
        );
        let view = links_of(&store, &a, &LinkFilter::default()).await.unwrap();
        assert!(view.outbound.is_empty());
        assert_eq!(view.hidden, 1);
        let view = links_of(&store, &a, &LinkFilter::all()).await.unwrap();
        assert_eq!(view.outbound[0].state, EdgeState::Dangling);

        let health = edge_health(&store).await.unwrap();
        assert_eq!(health.dangling.len(), 1);
        let graph = load_graph(&store, &GraphFilter::default()).await.unwrap();
        assert!(graph.edges.is_empty());
        assert_eq!(graph.dangling, 1);
        assert_eq!(
            graph.dangling_edges.len(),
            1,
            "drawable: its source is shown"
        );
    }

    /// A store that cannot be opened is "unknown", never "dangling".
    #[tokio::test]
    async fn unopenable_cross_store_target_is_unknown() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let elsewhere = uuid::Uuid::now_v7().to_string();
        let mut p = params(&a, LinkKind::DerivedFrom, &elsewhere);
        p.dst_store = Some("0123456789abcdef".into());
        let r = link(&store, p.clone(), Surface::Cli).await.unwrap();
        assert_eq!(r.target_state, EdgeState::Unknown);
        assert_eq!(r.dst_store.as_deref(), Some("0123456789abcdef"));

        // A prefix cannot be resolved blind.
        p.dst = elsewhere[..8].to_string();
        assert!(link(&store, p, Surface::Cli).await.is_err());

        let health = edge_health(&store).await.unwrap();
        assert_eq!((health.unknown, health.dangling.len()), (1, 0));
        let view = links_of(&store, &a, &LinkFilter::default()).await.unwrap();
        assert_eq!(view.outbound[0].state, EdgeState::Unknown);
        let graph = load_graph(&store, &GraphFilter::default()).await.unwrap();
        assert_eq!(
            (graph.edges.len(), graph.external, graph.dangling),
            (1, 1, 0)
        );
    }

    #[tokio::test]
    async fn compress_moves_outbound_edges_onto_the_summary() {
        let (_t, store) = store().await;
        let s1 = mem(&store, "s1", Visibility::Shared).await;
        let s2 = mem(&store, "s2", Visibility::Shared).await;
        let ext = mem(&store, "ext", Visibility::Shared).await;
        let fan = mem(&store, "fan", Visibility::Shared).await;
        link(&store, params(&s1, LinkKind::Supports, &ext), Surface::Cli)
            .await
            .unwrap();
        link(&store, params(&s2, LinkKind::Supports, &ext), Surface::Cli)
            .await
            .unwrap();
        link(
            &store,
            params(&s1, LinkKind::DerivedFrom, &s2),
            Surface::Cli,
        )
        .await
        .unwrap();
        link(&store, params(&fan, LinkKind::Supports, &s1), Surface::Cli)
            .await
            .unwrap();

        let result = crate::ops::compress_apply(
            &store,
            crate::ops::CompressApplyParams {
                source_ids: vec![s1.clone(), s2.clone()],
                summary: "merged".into(),
                content: "merged content".into(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        let summary = store.get(&result.new_id).await.unwrap();
        assert_eq!(
            summary.links.len(),
            1,
            "one supports edge to ext, deduped: {:?}",
            summary.links
        );
        assert_eq!(summary.links[0].to, ext);
        assert_eq!(summary.links[0].origin, LinkOrigin::Compress);
        assert!(store.get(&s1).await.unwrap().links.is_empty());
        assert!(store.get(&s2).await.unwrap().links.is_empty());

        // The inbound edge stays on `fan` and resolves through superseded_by.
        let view = links_of(&store, &fan, &LinkFilter::all()).await.unwrap();
        assert_eq!(view.outbound[0].other_id, s1);
        assert_eq!(
            view.outbound[0].successor.as_deref(),
            Some(result.new_id.as_str())
        );
    }

    /// The phase-4 read API on a read-only store, including one with no
    /// edges table at all: nothing is created, and missing means none.
    #[tokio::test]
    async fn load_graph_is_side_effect_free_and_filters() {
        let (tmp, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        let mut p = params(&a, LinkKind::Contradicts, &b);
        p.status = Some(LinkStatus::Proposed);
        link(&store, p, Surface::Cli).await.unwrap();
        link(&store, params(&a, LinkKind::Supports, &b), Surface::Cli)
            .await
            .unwrap();

        let ro = MemoryStore::open_read_only(tmp.path()).await.unwrap();
        let g = load_graph(&ro, &GraphFilter::default()).await.unwrap();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1, "proposals are opt-in");
        let g = load_graph(
            &ro,
            &GraphFilter {
                include_proposed: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(g.edges.len(), 2);
        let g = load_graph(
            &ro,
            &GraphFilter {
                include_proposed: true,
                kinds: Some(vec![LinkKind::Contradicts]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(g.edges.len(), 1);

        // Drop the table: the read-only view reports no edges and recreates nothing.
        let lance = crate::storage::paths::lancedb_dir(&store.project_id).unwrap();
        let conn = lancedb::connect(lance.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        conn.drop_table(crate::storage::edge_index::EDGES_TABLE, &[])
            .await
            .unwrap();
        let ro = MemoryStore::open_read_only(tmp.path()).await.unwrap();
        let g = load_graph(&ro, &GraphFilter::default()).await.unwrap();
        assert!(g.edges.is_empty());
        assert!(crate::storage::edge_index::open(&conn)
            .await
            .unwrap()
            .is_none());
    }

    fn proposed(src: &str, kind: LinkKind, dst: &str) -> LinkParams {
        LinkParams {
            status: Some(LinkStatus::Proposed),
            origin: LinkOrigin::Nli,
            ..params(src, kind, dst)
        }
    }

    /// Two racing proposers can leave one copy of a symmetric edge in each
    /// file. A review updates both, and a read prefers the decided state, so
    /// a rejection sticks.
    #[tokio::test]
    async fn a_review_updates_both_copies_of_a_symmetric_edge() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        link(&store, proposed(&a, LinkKind::RelatedTo, &b), Surface::Cli)
            .await
            .unwrap();
        // The racing writer's copy, on the other side.
        let copy = store.get(&a).await.unwrap().links[0].clone();
        let a_id = a.clone();
        store
            .update_with(&b, move |m| {
                m.links.push(Link { to: a_id, ..copy });
                Ok(())
            })
            .await
            .unwrap();
        let out = review_link(
            &store,
            eref(&b, LinkKind::RelatedTo, &a),
            LinkStatus::Rejected,
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        assert!(out.changed);
        for id in [&a, &b] {
            let m = store.get(id).await.unwrap();
            assert_eq!(m.links.len(), 1, "{id}");
            assert_eq!(m.links[0].status, LinkStatus::Rejected, "{id}");
        }
        let rows = store.list_edges().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, LinkStatus::Rejected);
    }

    #[test]
    fn a_rejection_outranks_a_proposal() {
        assert!(status_rank(&LinkStatus::Rejected) > status_rank(&LinkStatus::Proposed));
        assert!(status_rank(&LinkStatus::Confirmed) > status_rank(&LinkStatus::Rejected));
    }

    /// A review applies to a proposed edge only: the same verdict again is a
    /// no-op, a different one after someone reviewed it is `NotProposed`.
    #[tokio::test]
    async fn a_review_needs_the_edge_still_proposed() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        link(&store, proposed(&a, LinkKind::Supports, &b), Surface::Cli)
            .await
            .unwrap();
        let review = |to| {
            review_link(
                &store,
                eref(&a, LinkKind::Supports, &b),
                to,
                None,
                Surface::Cli,
            )
        };
        assert!(review(LinkStatus::Confirmed).await.unwrap().changed);
        let again = review(LinkStatus::Confirmed).await.unwrap();
        assert!(!again.changed);
        let err = review(LinkStatus::Rejected).await.unwrap_err();
        assert_eq!(
            err.downcast_ref::<NotProposed>().unwrap().current,
            LinkStatus::Confirmed
        );
        assert_eq!(
            store.get(&a).await.unwrap().links[0].status,
            LinkStatus::Confirmed
        );
        // A human edge confirmed by `link` is never flipped by a review.
        let c = mem(&store, "c", Visibility::Shared).await;
        link(&store, params(&a, LinkKind::Supports, &c), Surface::Cli)
            .await
            .unwrap();
        let err = review_link(
            &store,
            eref(&a, LinkKind::Supports, &c),
            LinkStatus::Rejected,
            None,
            Surface::Cli,
        )
        .await
        .unwrap_err();
        assert!(err.downcast_ref::<NotProposed>().is_some());
    }

    async fn nli_challenged(store: &MemoryStore, id: &str, about: &str) {
        let evidence = format!(
            "NLI contradiction (score: 0.91): new memory 'x' ({about}) contradicts this observation"
        );
        crate::nli::challenge_memory(store, id, &evidence, None)
            .await
            .unwrap();
        // An unrelated challenge on the other memory stays.
    }

    /// Settling a contradiction resolves the NLI challenge it came with, on
    /// whichever side carries it; an unrelated challenge stays.
    #[tokio::test]
    async fn settling_a_contradiction_resolves_its_nli_challenge() {
        for verdict in ["reject", "both_stand", "supersede"] {
            let (_t, store) = store().await;
            let a = mem(&store, "Cache is on", Visibility::Shared).await;
            let b = mem(&store, "Cache is off", Visibility::Shared).await;
            link(
                &store,
                proposed(&b, LinkKind::Contradicts, &a),
                Surface::Cli,
            )
            .await
            .unwrap();
            nli_challenged(&store, &a, &b).await;
            crate::nli::challenge_memory(&store, &b, "hand-written: check the docs", None)
                .await
                .unwrap();
            let v = match verdict {
                "reject" => ContradictionVerdict::Reject,
                "both_stand" => ContradictionVerdict::BothStand,
                _ => ContradictionVerdict::Supersedes { winner: a.clone() },
            };
            let out = review_contradiction(&store, &a, &b, v.clone(), None, Surface::Cli)
                .await
                .unwrap();
            assert!(out.changed, "{verdict}");
            assert_eq!(out.resolved_challenges, vec![a.clone()], "{verdict}");
            let am = store.get(&a).await.unwrap();
            assert!(am.challenges.is_empty(), "{verdict}");
            assert_eq!(am.status, crate::types::Status::Active, "{verdict}");
            let bm = store.get(&b).await.unwrap();
            assert_eq!(
                bm.challenges.len(),
                1,
                "{verdict}: unrelated challenge kept"
            );
            // A repeated submit is a no-op.
            let again = review_contradiction(&store, &a, &b, v, None, Surface::Cli)
                .await
                .unwrap();
            assert!(!again.changed, "{verdict}");
        }
    }

    /// Put a second copy of `owner`'s edge to `other` into `other`'s file,
    /// as two racing writers can.
    async fn second_copy(store: &MemoryStore, owner: &str, other: &str) {
        let copy = store.get(owner).await.unwrap().links[0].clone();
        let owner_id = owner.to_string();
        store
            .update_with(other, move |m| {
                m.links.push(Link {
                    to: owner_id,
                    ..copy
                });
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn unlink_removes_both_copies_of_a_symmetric_edge() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        link(&store, params(&a, LinkKind::RelatedTo, &b), Surface::Cli)
            .await
            .unwrap();
        second_copy(&store, &a, &b).await;
        unlink(&store, eref(&a, LinkKind::RelatedTo, &b), Surface::Cli)
            .await
            .unwrap();
        for id in [&a, &b] {
            assert!(store.get(id).await.unwrap().links.is_empty(), "{id}");
        }
        assert!(store.list_edges().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_supersede_verdict_removes_both_copies_of_the_contradiction() {
        let (_t, store) = store().await;
        let (a, b) = proposed_contradiction(&store).await;
        second_copy(&store, &b, &a).await;
        confirm_contradiction_direction(
            &store,
            &a,
            &b,
            ContradictionVerdict::Supersedes { winner: a.clone() },
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        for id in [&a, &b] {
            assert!(
                !store
                    .get(id)
                    .await
                    .unwrap()
                    .links
                    .iter()
                    .any(|l| l.kind == LinkKind::Contradicts),
                "{id} still holds a contradicts copy"
            );
        }
        let rows = store.list_edges().await.unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].kind, LinkKind::Supersedes);
    }

    /// The CLI/MCP verdicts (`link_status`, `link --winner`, `link --status`)
    /// resolve the NLI challenge a contradiction came with, as the
    /// dashboard's review does.
    #[tokio::test]
    async fn cli_and_mcp_verdicts_resolve_the_nli_challenge() {
        for verdict in ["reject", "both_stand", "supersede", "link_status"] {
            let (_t, store) = store().await;
            let a = mem(&store, "Cache is on", Visibility::Shared).await;
            let b = mem(&store, "Cache is off", Visibility::Shared).await;
            link(
                &store,
                proposed(&b, LinkKind::Contradicts, &a),
                Surface::Cli,
            )
            .await
            .unwrap();
            nli_challenged(&store, &a, &b).await;
            if verdict == "link_status" {
                let mut p = params(&b, LinkKind::Contradicts, &a);
                p.status = Some(LinkStatus::Confirmed);
                link(&store, p, Surface::Cli).await.unwrap();
            } else {
                let v = match verdict {
                    "reject" => ContradictionVerdict::Reject,
                    "both_stand" => ContradictionVerdict::BothStand,
                    _ => ContradictionVerdict::Supersedes { winner: a.clone() },
                };
                confirm_contradiction_direction(&store, &a, &b, v, None, Surface::Mcp)
                    .await
                    .unwrap();
            }
            let am = store.get(&a).await.unwrap();
            assert!(am.challenges.is_empty(), "{verdict}");
            assert_eq!(am.status, crate::types::Status::Active, "{verdict}");
        }
    }

    /// A supersede review claims the edge under the lock: an edge someone
    /// rejected after the caller's snapshot is `NotProposed`, and the
    /// rejection survives with nothing superseded.
    #[tokio::test]
    async fn a_supersede_review_never_deletes_a_concurrent_rejection() {
        let (_t, store) = store().await;
        let (a, b) = proposed_contradiction(&store).await;
        // The other tab's rejection lands first.
        review_link(
            &store,
            eref(&a, LinkKind::Contradicts, &b),
            LinkStatus::Rejected,
            None,
            Surface::Dashboard,
        )
        .await
        .unwrap();
        // `apply_direction` without the snapshot pre-check: only the claim
        // under the lock stands between it and the rejection.
        let err = apply_direction(
            &store,
            &a,
            &b,
            ContradictionVerdict::Supersedes { winner: a.clone() },
            None,
            Surface::Dashboard,
            true,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.downcast_ref::<NotProposed>().unwrap().current,
            LinkStatus::Rejected
        );
        let bm = store.get(&b).await.unwrap();
        assert_eq!(bm.links.len(), 1);
        assert_eq!(bm.links[0].status, LinkStatus::Rejected);
        assert!(!bm.is_invalidated());
        assert!(store.get(&a).await.unwrap().supersedes.is_empty());

        // A proposed one is claimed and superseded as before.
        let (c, d) = proposed_contradiction(&store).await;
        let out = review_contradiction(
            &store,
            &c,
            &d,
            ContradictionVerdict::Supersedes { winner: c.clone() },
            None,
            Surface::Dashboard,
        )
        .await
        .unwrap();
        assert!(out.changed);
        assert!(store.get(&d).await.unwrap().is_invalidated());
        assert!(store.get(&d).await.unwrap().links.is_empty());
    }

    /// Compress on the sync create path: the proposers ran on the summary
    /// first. A source's reviewed edge must win over that fresh proposal —
    /// a rejection is not re-raised, and a proposal of another kind to the
    /// same end does not survive next to it.
    #[tokio::test]
    async fn carry_replaces_the_summarys_fresh_proposals_to_a_sources_end() {
        let (_t, store) = store().await;
        let s1 = mem(&store, "s1", Visibility::Shared).await;
        let t = mem(&store, "t", Visibility::Shared).await;
        let u = mem(&store, "u", Visibility::Shared).await;
        let x = mem(&store, "x", Visibility::Shared).await;
        link(
            &store,
            proposed(&s1, LinkKind::Contradicts, &t),
            Surface::Cli,
        )
        .await
        .unwrap();
        review_link(
            &store,
            eref(&s1, LinkKind::Contradicts, &t),
            LinkStatus::Rejected,
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        link(&store, params(&s1, LinkKind::Supports, &u), Surface::Cli)
            .await
            .unwrap();
        // What the proposers wrote on the summary before the carry.
        link(
            &store,
            proposed(&x, LinkKind::Contradicts, &t),
            Surface::Cli,
        )
        .await
        .unwrap();
        let mut sim = proposed(&x, LinkKind::RelatedTo, &u);
        sim.origin = LinkOrigin::Similarity;
        link(&store, sim, Surface::Cli).await.unwrap();

        let source = store.get(&s1).await.unwrap();
        carry_links_onto(&store, &x, &[source]).await;
        let summary = store.get(&x).await.unwrap();
        assert_eq!(summary.links.len(), 2, "{:?}", summary.links);
        let to_t = summary.links.iter().find(|l| l.to == t).unwrap();
        assert_eq!(
            (to_t.kind.clone(), to_t.status.clone(), to_t.origin.clone()),
            (
                LinkKind::Contradicts,
                LinkStatus::Rejected,
                LinkOrigin::Compress
            )
        );
        let to_u = summary.links.iter().find(|l| l.to == u).unwrap();
        assert_eq!(
            to_u.kind,
            LinkKind::Supports,
            "the proposal of another kind went"
        );
        assert!(store.get(&s1).await.unwrap().links.is_empty());
    }

    /// A committed summary takes a cross-store edge only when its target is
    /// shown to be shared in its own store.
    #[tokio::test]
    async fn carry_keeps_unverifiable_cross_store_edges_off_a_committed_summary() {
        let (_t, store) = store().await;
        let p = mem(&store, "personal source", Visibility::Personal).await;
        let x = mem(&store, "summary", Visibility::Shared).await;
        let name = format!("links-carry-{}", uuid::Uuid::now_v7().simple());
        let gid = crate::storage::paths::compute_group_id(&name);
        let group = MemoryStore::open_group(&gid).await.unwrap();
        let shared = mem(&group, "shared there", Visibility::Shared).await;
        let elsewhere = uuid::Uuid::now_v7().to_string();
        let mut to_group = params(&p, LinkKind::Supports, &shared);
        to_group.dst_store = Some(gid.clone());
        link(&store, to_group, Surface::Cli).await.unwrap();
        let mut to_unknown = params(&p, LinkKind::DerivedFrom, &elsewhere);
        to_unknown.dst_store = Some("0123456789abcdef".into());
        link(&store, to_unknown, Surface::Cli).await.unwrap();

        let source = store.get(&p).await.unwrap();
        carry_links_onto(&store, &x, &[source]).await;
        let summary = store.get(&x).await.unwrap();
        assert_eq!(summary.links.len(), 1, "{:?}", summary.links);
        assert_eq!(summary.links[0].to, shared);
        let left = store.get(&p).await.unwrap().links;
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(
            left[0].to, elsewhere,
            "the unverifiable one stays on its source"
        );
    }

    /// A store id read from a file is validated before it finds a store on
    /// disk: a path-like `__g_` id is refused (and reads as unknown).
    #[tokio::test]
    async fn path_like_store_ids_never_reach_the_filesystem() {
        for bad in ["__g_/../../../../tmp", "__g_..", "../0123456789ab"] {
            let err = open_store_by_id(bad, true).await.err().unwrap();
            assert!(
                err.downcast_ref::<LinkRefused>().is_some(),
                "{bad}: {err:#}"
            );
            assert!(canonical_store_id(bad).is_err(), "{bad}");
        }
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let far = uuid::Uuid::now_v7().to_string();
        let mut link_entry = Link::new(far, LinkKind::Supports, LinkOrigin::Human);
        link_entry.store = Some("__g_/../../../../tmp".into());
        store
            .update_with(&a, move |m| {
                m.links.push(link_entry);
                Ok(())
            })
            .await
            .unwrap();
        let v = links_of(&store, &a, &LinkFilter::default()).await.unwrap();
        assert_eq!(v.outbound[0].state, EdgeState::Unknown);
    }

    /// Refusals are typed (`LinkRefused`), so a front end can tell them from
    /// a storage failure; a missing target stays a storage `NotFound`.
    #[tokio::test]
    async fn refusals_are_typed_and_a_missing_target_is_not_found() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let p = mem(&store, "p", Visibility::Personal).await;
        let err = link(&store, params(&a, LinkKind::Supports, &a), Surface::Cli)
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<LinkRefused>().is_some(), "{err:#}");
        let err = link(&store, params(&a, LinkKind::Supports, &p), Surface::Cli)
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<LinkRefused>().is_some(), "{err:#}");
        let missing = uuid::Uuid::now_v7().to_string();
        let err = link(
            &store,
            params(&a, LinkKind::Supports, &missing),
            Surface::Cli,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<StorageError>(),
                Some(StorageError::NotFound(_))
            ),
            "{err:#}"
        );
        assert!(format!("{err:#}").contains("target memory not found"));
        let err = unlink(&store, eref(&a, LinkKind::Supersedes, &p), Surface::Cli)
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<EdgeNotFound>().is_some(), "{err:#}");
    }

    /// doctor's leak check covers a cross-store target that is personal in
    /// its own store.
    #[tokio::test]
    async fn edge_health_flags_a_cross_store_personal_target() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let name = format!("links-leak-{}", uuid::Uuid::now_v7().simple());
        let gid = crate::storage::paths::compute_group_id(&name);
        let group = MemoryStore::open_group(&gid).await.unwrap();
        let personal = mem(&group, "personal there", Visibility::Personal).await;
        // As a hand edit (or an older binary) would leave it.
        let mut entry = Link::new(personal.clone(), LinkKind::Supports, LinkOrigin::Human);
        entry.store = Some(gid);
        store
            .update_with(&a, move |m| {
                m.links.push(entry);
                Ok(())
            })
            .await
            .unwrap();
        let health = edge_health(&store).await.unwrap();
        assert_eq!(health.personal_leaks.len(), 1, "{health:?}");
        assert_eq!(health.personal_leaks[0].dst, personal);
    }
}
