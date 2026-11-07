//! `link` / `unlink` / `links`: typed edges between memories.
//!
//! Thin over `engramdb::ops::links`, which owns every rule (one copy per
//! symmetric edge, `supersedes` kept canonical, no personal id in a committed
//! file, dangling is legal). This module parses the user's spellings and
//! renders.

use crate::output::OutputFormatter;
use anyhow::{bail, Result};
use engramdb::ops::links::{
    self, EdgeRef, LinkFilter, LinkKind, LinkOrigin, LinkParams, LinkStatus,
};
use engramdb::ops::ContradictionVerdict;
use engramdb::storage::MemoryStore;
use std::path::Path;

/// Arguments of `engramdb link`, as typed.
pub struct LinkArgs {
    pub src: String,
    pub kind: String,
    pub dst: String,
    pub store: Option<String>,
    pub confidence: Option<f64>,
    pub note: Option<String>,
    pub status: Option<String>,
    /// `--winner`: resolve a `contradicts` edge as one side superseding the
    /// other (`ops::confirm_contradiction_direction`).
    pub winner: Option<String>,
}

/// A kind the user typed. A typo is an error here, never an unknown kind
/// written into a committed file.
fn parse_kind(s: &str) -> Result<LinkKind> {
    match LinkKind::parse_known(s) {
        Some(k) => Ok(k),
        None => bail!(
            "unknown edge kind {s:?} (known: {})",
            LinkKind::known_list()
        ),
    }
}

fn parse_status(s: &str) -> Result<LinkStatus> {
    match LinkStatus::parse_known(s) {
        Some(st) => Ok(st),
        None => bail!(
            "unknown edge status {s:?} (known: {})",
            LinkStatus::known_list()
        ),
    }
}

async fn open(dir: &Path, global: bool) -> Result<MemoryStore> {
    Ok(if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    })
}

/// Add an edge, or update the same edge's confidence / note / status.
pub async fn run_link(
    dir: &Path,
    global: bool,
    args: LinkArgs,
    formatter: &OutputFormatter,
) -> Result<()> {
    let kind = parse_kind(&args.kind)?;
    let status = args.status.as_deref().map(parse_status).transpose()?;
    let store = open(dir, global).await?;
    if let Some(winner) = args.winner {
        if kind != LinkKind::Contradicts {
            bail!("--winner resolves a contradicts edge; this edge is {kind}");
        }
        let outcome = engramdb::ops::confirm_contradiction_direction(
            &store,
            &args.src,
            &args.dst,
            ContradictionVerdict::Supersedes { winner },
            args.note,
            engramdb::ops::Surface::Cli,
        )
        .await?;
        formatter.print_contradiction_outcome(&outcome);
        return Ok(());
    }
    let result = links::link(
        &store,
        LinkParams {
            src: args.src,
            kind,
            dst: args.dst,
            dst_store: args.store,
            confidence: args.confidence,
            origin: LinkOrigin::Human,
            status,
            note: args.note,
        },
        engramdb::ops::Surface::Cli,
    )
    .await?;
    formatter.print_link_result(&result);
    Ok(())
}

/// Remove one edge.
pub async fn run_unlink(
    dir: &Path,
    global: bool,
    src: &str,
    kind: &str,
    dst: &str,
    store_spec: Option<String>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let kind = parse_kind(kind)?;
    let store = open(dir, global).await?;
    let result = links::unlink(
        &store,
        EdgeRef {
            src: src.to_string(),
            kind,
            dst: dst.to_string(),
            dst_store: store_spec,
        },
        engramdb::ops::Surface::Cli,
    )
    .await?;
    formatter.print_unlink_result(&result);
    Ok(())
}

/// List a memory's edges in both directions.
pub async fn run_links(
    dir: &Path,
    global: bool,
    id: &str,
    all: bool,
    kinds: &[String],
    formatter: &OutputFormatter,
) -> Result<()> {
    let kinds = kinds
        .iter()
        .map(|k| parse_kind(k))
        .collect::<Result<Vec<_>>>()?;
    let mut filter = if all {
        LinkFilter::all()
    } else {
        LinkFilter::default()
    };
    if !kinds.is_empty() {
        filter.kinds = Some(kinds);
    }
    let store = open(dir, global).await?;
    let view = links::links_of(&store, id, &filter).await?;
    formatter.print_links(&view);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::challenge::{run_challenge, ChallengeParams};
    use crate::testutil::{capturing_plain, snap_command, TempProject};
    use engramdb::types::{Memory, MemoryType, Provenance};

    /// The edge rows carry their creation instant; the rest of the run is
    /// already redacted by `normalize`.
    fn redact_times(raw: &str) -> String {
        let re =
            regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})")
                .unwrap();
        re.replace_all(raw, "[TS]").into_owned()
    }

    async fn two_memories(p: &TempProject) -> (String, String) {
        let store = p.init_store().await;
        let a = Memory::new(
            MemoryType::Decision,
            "Use rustls everywhere",
            "No OpenSSL in the dependency tree.",
            Provenance::human(),
        );
        let b = Memory::new(
            MemoryType::Hazard,
            "Custom CA breaks webpki downloads",
            "The sandbox gateway's CA is rejected by webpki.",
            Provenance::human(),
        );
        let (a_id, b_id) = (a.id.clone(), b.id.clone());
        store.create(&a).await.unwrap();
        store.create(&b).await.unwrap();
        (a_id, b_id)
    }

    #[test]
    fn parse_kind_refuses_typos_and_names_the_known_kinds() {
        assert_eq!(parse_kind("supports").unwrap(), LinkKind::Supports);
        let err = parse_kind("suports").unwrap_err().to_string();
        assert!(err.contains("derived_from"), "{err}");
        assert!(parse_status("maybe").is_err());
    }

    /// The whole lifecycle in one transcript: link (directed and symmetric),
    /// list from both ends, re-link to reject, list with and without
    /// `--all`, unlink.
    #[tokio::test]
    async fn link_lifecycle_plain() {
        let p = TempProject::new();
        let (a, b) = two_memories(&p).await;
        let (f, cap) = capturing_plain();
        let link =
            |src: &str, kind: &str, dst: &str, status: Option<&str>, note: Option<&str>| LinkArgs {
                src: src.to_string(),
                kind: kind.to_string(),
                dst: dst.to_string(),
                store: None,
                confidence: None,
                note: note.map(str::to_string),
                status: status.map(str::to_string),
                winner: None,
            };
        run_link(
            p.path(),
            false,
            link(&b, "supports", &a, None, Some("the reason")),
            &f,
        )
        .await
        .unwrap();
        run_link(p.path(), false, link(&a, "related_to", &b, None, None), &f)
            .await
            .unwrap();
        run_links(p.path(), false, &a, false, &[], &f)
            .await
            .unwrap();
        run_links(p.path(), false, &b, false, &[], &f)
            .await
            .unwrap();
        // Rejecting is re-linking the same edge with a status.
        run_link(
            p.path(),
            false,
            link(&b, "related_to", &a, Some("rejected"), None),
            &f,
        )
        .await
        .unwrap();
        run_links(p.path(), false, &a, false, &[], &f)
            .await
            .unwrap();
        run_links(p.path(), false, &a, true, &["related_to".to_string()], &f)
            .await
            .unwrap();
        run_unlink(p.path(), false, &b, "supports", &a, None, &f)
            .await
            .unwrap();
        run_links(p.path(), false, &a, true, &[], &f).await.unwrap();
        snap_command(
            "link_lifecycle_plain",
            p.path(),
            redact_times(&cap.transcript()),
        );
    }

    #[tokio::test]
    async fn link_refuses_an_unknown_kind_before_touching_the_store() {
        let p = TempProject::new();
        let (a, b) = two_memories(&p).await;
        let (f, cap) = capturing_plain();
        let err = run_link(
            p.path(),
            false,
            LinkArgs {
                src: a.clone(),
                kind: "refutes".into(),
                dst: b,
                store: None,
                confidence: None,
                note: None,
                status: None,
                winner: None,
            },
            &f,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("unknown edge kind"), "{err}");
        assert!(
            cap.transcript().trim().ends_with("--- stderr ---"),
            "{}",
            cap.transcript()
        );
        let store = MemoryStore::open(p.path()).await.unwrap();
        assert!(store.get(&a).await.unwrap().links.is_empty());
    }

    /// `challenge --against` records the challenge and proposes the edge.
    #[tokio::test]
    async fn link_challenge_against_proposes_a_contradiction() {
        let p = TempProject::new();
        let (a, b) = two_memories(&p).await;
        let (f, cap) = capturing_plain();
        run_challenge(
            p.path(),
            false,
            ChallengeParams {
                id: a.clone(),
                evidence: "webpki rejects the gateway CA".into(),
                source_file: None,
                against: Some(b.clone()),
            },
            &f,
        )
        .await
        .unwrap();
        run_links(p.path(), false, &a, false, &[], &f)
            .await
            .unwrap();
        snap_command(
            "link_challenge_against",
            p.path(),
            redact_times(&cap.transcript()),
        );
    }

    /// `--winner` turns a proposed contradiction into a supersession: the
    /// loser's window closes and the edge is rewritten.
    #[tokio::test]
    async fn link_winner_resolves_a_contradiction_by_direction() {
        let p = TempProject::new();
        let (a, b) = two_memories(&p).await;
        let (f, cap) = capturing_plain();
        let args = |status: Option<&str>, winner: Option<&str>| LinkArgs {
            src: a.clone(),
            kind: "contradicts".into(),
            dst: b.clone(),
            store: None,
            confidence: None,
            note: None,
            status: status.map(str::to_string),
            winner: winner.map(str::to_string),
        };
        run_link(p.path(), false, args(Some("proposed"), None), &f)
            .await
            .unwrap();
        run_link(p.path(), false, args(None, Some(&b)), &f)
            .await
            .unwrap();
        run_links(p.path(), false, &b, true, &[], &f).await.unwrap();
        let store = MemoryStore::open(p.path()).await.unwrap();
        let loser = store.get(&a).await.unwrap();
        assert_eq!(loser.superseded_by.as_deref(), Some(b.as_str()));
        assert!(loser.is_invalidated());
        snap_command(
            "link_winner_resolves_contradiction",
            p.path(),
            redact_times(&cap.transcript()),
        );
    }

    /// Review p3-ops #6: a bad `--against` fails the whole command before the
    /// challenge is recorded, so a retry cannot record it twice.
    #[tokio::test]
    async fn link_challenge_against_a_bad_target_records_nothing() {
        let p = TempProject::new();
        let (a, _b) = two_memories(&p).await;
        let (f, _cap) = capturing_plain();
        for against in [a.clone(), "ffffffff-no-such".to_string()] {
            let err = run_challenge(
                p.path(),
                false,
                ChallengeParams {
                    id: a.clone(),
                    evidence: "x".into(),
                    source_file: None,
                    against: Some(against),
                },
                &f,
            )
            .await
            .unwrap_err();
            assert!(err.to_string().contains("--against"), "{err:#}");
        }
        let store = MemoryStore::open(p.path()).await.unwrap();
        let m = store.get(&a).await.unwrap();
        assert!(m.challenges.is_empty(), "{:?}", m.challenges);
        assert!(m.links.is_empty());
    }
}
