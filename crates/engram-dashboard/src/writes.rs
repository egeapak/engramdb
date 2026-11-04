//! The dashboard's only writes: confirming or rejecting a proposed edge.
//!
//! `POST /api/projects/{id}/edges/confirm` and `…/edges/reject`, with a JSON
//! body naming the edge as the CLI does (`src`, `kind`, `dst`, optional
//! `dst_store`). A `contradicts` edge is confirmed with a `verdict`, the
//! three choices of the plan's "Contradiction direction":
//!
//! - `"both_stand"` — the edge becomes `confirmed`; both sides stand, in
//!   conflict.
//! - `"supersedes"` with `winner` (one of the two ids) — the edge is replaced
//!   by `winner supersedes loser`; only the loser's validity window closes.
//!
//! and rejected like any other kind (kept as `rejected`, so a proposer never
//! raises it again). Every write goes through `ops::links` (`review_link`,
//! `review_contradiction`), which records the `linked` (and `resolved`)
//! usage events — from [`Surface::Dashboard`].
//!
//! **Only a proposed edge is reviewed.** The status is checked under the
//! store's write lock: a request for the state the edge is already in (a
//! double submit, a second tab) is a no-op answered `changed: false`; an edge
//! someone else already reviewed differently is 409 `not_proposed`. Settling a
//! contradiction any way also resolves the NLI challenge it came with, and the
//! response lists the memories whose challenge was resolved, so the page can
//! say so.
//!
//! **Gated three times.** The security guard refuses any non-GET without
//! this dashboard's `Origin` and CSRF header (on top of the bearer token);
//! the handlers refuse everything with 403 `read_only` unless the dashboard
//! was started with `--allow-writes`; and the page asks for an in-page
//! confirmation before it sends.
//!
//! **A writable store per request, never the cache.** The read-only handles
//! in [`crate::stores::StoreCache`] cannot write by construction, and that
//! stays true: a write opens the store through the ordinary
//! `MemoryStore::open` path, which takes the project's write lock for each
//! write and — like any engramdb command — may write `.engramdb/.gitignore`
//! or migrate a store whose schema is behind. That side effect is acceptable
//! here only because the user explicitly asked for a write (`--allow-writes`
//! and a confirmed click). The handle is dropped when the request ends, and
//! the cached read-only handle is invalidated so the next read reopens.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use engramdb::ops::links::{
    self, ContradictionVerdict, EdgeRef, LinkKind, LinkStatus, MAX_NOTE_CHARS,
};
use engramdb::ops::usage::Surface;
use engramdb::storage::{MemoryStore, StorageError};
use serde::Deserialize;

use crate::api::{ok, ApiResult};
use crate::projects::{Location, StoreKind};
use crate::{ApiError, AppState};

/// Largest request body accepted (a note is at most [`MAX_NOTE_CHARS`]).
const MAX_BODY: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EdgeBody {
    src: String,
    kind: String,
    dst: String,
    #[serde(default)]
    dst_store: Option<String>,
    /// `contradicts` confirm only: `both_stand` or `supersedes`.
    #[serde(default)]
    verdict: Option<String>,
    /// With `verdict: "supersedes"`: the memory that wins.
    #[serde(default)]
    winner: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

fn read_only() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "read_only",
        "This dashboard is read-only. Restart it with `engramdb dashboard --allow-writes` to \
         confirm or reject relations.",
    )
}

fn parse(body: &Bytes) -> Result<(EdgeBody, LinkKind), ApiError> {
    if body.len() > MAX_BODY {
        return Err(ApiError::bad_request("request body too large"));
    }
    let b: EdgeBody = serde_json::from_slice(body)
        .map_err(|e| ApiError::bad_request(format!("invalid request body: {e}")))?;
    let kind = LinkKind::parse_known(&b.kind).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown relation kind {:?} (known: {})",
            b.kind,
            LinkKind::known_list()
        ))
    })?;
    if kind == LinkKind::Supersedes {
        return Err(ApiError::bad_request(
            "a supersession has no review status: it is confirmed by existing. Remove it with \
             `engramdb unlink` if it is wrong",
        ));
    }
    if b.note
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
    {
        return Err(ApiError::bad_request(format!(
            "note is longer than {MAX_NOTE_CHARS} characters"
        )));
    }
    Ok((b, kind))
}

/// Open `location` for writing (module docs: the one place the dashboard
/// does).
async fn open_writable(location: &Location) -> Result<MemoryStore, ApiError> {
    let opened = match location.kind {
        StoreKind::Global => MemoryStore::open_global().await,
        StoreKind::Group => MemoryStore::open_group(&location.id).await,
        StoreKind::Project => MemoryStore::open(&location.root).await,
    };
    opened.map_err(|e| match e {
        StorageError::NotInitialized => {
            ApiError::unavailable(format!("{} has no EngramDB store", location.root.display()))
        }
        other => ApiError::internal(other),
    })
}

/// How a failed review maps to HTTP. The request's problem gets 4xx: no such
/// edge or memory (404); an edge already reviewed elsewhere (409
/// `not_proposed`, naming its status); an `ops::links` refusal — a loser
/// already superseded, a personal-id leak — which is an `anyhow` message with
/// no storage error inside (409 `edge_write_refused`). Anything from storage
/// itself (I/O, the index) is the server's problem: 500.
fn write_error(e: anyhow::Error) -> ApiError {
    if let Some(missing) = e.downcast_ref::<links::EdgeNotFound>() {
        return ApiError::not_found("unknown_edge", missing.to_string());
    }
    if let Some(np) = e.downcast_ref::<links::NotProposed>() {
        return ApiError::new(StatusCode::CONFLICT, "not_proposed", np.to_string());
    }
    match e.downcast::<StorageError>() {
        Ok(StorageError::NotFound(id)) => {
            ApiError::not_found("unknown_memory", format!("no memory {id} in this store"))
        }
        Ok(other) => ApiError::internal(other),
        Err(e) => ApiError::new(StatusCode::CONFLICT, "edge_write_refused", format!("{e:#}")),
    }
}

async fn write<F, Fut>(state: &AppState, id: &str, f: F) -> ApiResult
where
    F: FnOnce(MemoryStore) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<links::ReviewOutcome>>,
{
    if !state.allow_writes {
        return Err(read_only());
    }
    let location = crate::projects::locate(id, state.current.as_ref()).await?;
    let store = open_writable(&location).await?;
    let result = f(store).await;
    // Even a refused write may have opened (and migrated) the store.
    state.stores.invalidate(&location).await;
    let out = result.map_err(write_error)?;
    ok(&out)
}

/// A confirm's verdict, validated before any store is opened.
fn verdict_of(b: &EdgeBody, kind: &LinkKind) -> Result<Option<ContradictionVerdict>, ApiError> {
    if *kind != LinkKind::Contradicts {
        if b.verdict.is_some() || b.winner.is_some() {
            return Err(ApiError::bad_request(
                "`verdict` and `winner` apply to contradicts relations only",
            ));
        }
        return Ok(None);
    }
    let verdict = match b.verdict.as_deref() {
        Some("both_stand") => ContradictionVerdict::BothStand,
        Some("supersedes") => ContradictionVerdict::Supersedes {
            winner: b
                .winner
                .clone()
                .filter(|w| !w.trim().is_empty())
                .ok_or_else(|| {
                    ApiError::bad_request("verdict \"supersedes\" needs a `winner` id")
                })?,
        },
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "unknown verdict {other:?}: use both_stand or supersedes"
            )))
        }
        None => {
            return Err(ApiError::bad_request(
                "confirming a contradiction needs a `verdict`: both_stand or supersedes",
            ))
        }
    };
    if b.dst_store.is_some() && verdict != ContradictionVerdict::BothStand {
        return Err(ApiError::bad_request(
            "a supersession cannot cross stores; confirm a cross-store contradiction as \
             both_stand",
        ));
    }
    if b.winner.is_some() && !matches!(verdict, ContradictionVerdict::Supersedes { .. }) {
        return Err(ApiError::bad_request(
            "`winner` applies to the supersedes verdict only",
        ));
    }
    Ok(Some(verdict))
}

/// Confirm a proposed edge. Answers [`links::ReviewOutcome`]: `changed:
/// false` for a repeated submit, and the memories whose NLI challenge the
/// review resolved.
pub(crate) async fn confirm_edge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    if !state.allow_writes {
        return Err(read_only());
    }
    let (b, kind) = parse(&body)?;
    let verdict = verdict_of(&b, &kind)?;
    write(&state, &id, move |store| async move {
        match verdict {
            Some(v) if b.dst_store.is_none() => {
                links::review_contradiction(&store, &b.src, &b.dst, v, b.note, Surface::Dashboard)
                    .await
            }
            _ => {
                links::review_link(
                    &store,
                    EdgeRef {
                        src: b.src,
                        kind,
                        dst: b.dst,
                        dst_store: b.dst_store,
                    },
                    LinkStatus::Confirmed,
                    b.note,
                    Surface::Dashboard,
                )
                .await
            }
        }
    })
    .await
}

/// Reject a proposed edge (kept as `rejected`, never proposed again).
pub(crate) async fn reject_edge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    if !state.allow_writes {
        return Err(read_only());
    }
    let (b, kind) = parse(&body)?;
    if b.verdict.is_some() || b.winner.is_some() {
        return Err(ApiError::bad_request(
            "`verdict` and `winner` belong to confirm, not reject",
        ));
    }
    write(&state, &id, move |store| async move {
        links::review_link(
            &store,
            EdgeRef {
                src: b.src,
                kind,
                dst: b.dst,
                dst_store: b.dst_store,
            },
            LinkStatus::Rejected,
            b.note,
            Surface::Dashboard,
        )
        .await
    })
    .await
}
