//! Typed edges between memories ([`Link`]).
//!
//! A memory's **outbound** edges live in its own file (the hidden
//! `<!-- engramdb -->` block), for the same two reasons `source_sessions` does:
//! the schema migration rebuilds the index from the `.md` files, so a
//! LanceDB-only relation would be destroyed by the mechanism that adds
//! columns, and memory files travel with a clone while `lancedb/` does not.
//! The index's `edges` table is *derived* from the files and answers inbound
//! queries.
//!
//! # Unknown values survive
//!
//! The enum fields ([`LinkKind`], [`LinkOrigin`], [`LinkStatus`]) each carry
//! an `Other(String)` variant. A file written by a newer binary may name a
//! kind this binary does not know; parsing it into a closed enum would either
//! fail the whole file or drop the value, and the next rewrite (any `update`)
//! would then erase it from a committed file. `Other` round-trips the exact
//! spelling instead, and every policy decision treats it as inert: an unknown
//! kind is never symmetric, never scored, never rewritten into a known one.
//!
//! **Unknown keys do not survive.** A field this binary does not know (on a
//! link, or a new key in the hidden block) is ignored on read and therefore
//! dropped by the next rewrite of that file. Preserving them needs a
//! `#[serde(flatten)]` catch-all, which the hidden block's YAML
//! deserializer and every `Link { .. }` literal would have to carry; that was
//! judged not worth it before any newer field exists. A binary that adds a
//! field to `Link` must therefore treat its absence as the old behaviour —
//! which every field here already does — and must not rely on an older
//! binary preserving it. (A binary older than links drops the whole `links`
//! list on rewrite for the same reason; nothing written now can change that.)

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Declare a string-valued enum whose unknown spellings round-trip through
/// an `Other(String)` variant (see the module docs).
macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $( $(#[$vmeta:meta])* $variant:ident => $text:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this binary does not know (written by a newer one).
            /// Preserved verbatim on rewrite and otherwise inert.
            Other(String),
        }

        impl $name {
            /// Every known value, in declaration order.
            pub const KNOWN: &'static [$name] = &[ $( $name::$variant ),+ ];

            /// The on-disk spelling.
            pub fn as_str(&self) -> &str {
                match self {
                    $( $name::$variant => $text, )+
                    $name::Other(s) => s.as_str(),
                }
            }

            /// Parse any spelling. Never fails: an unknown value becomes
            /// `Other`, so a newer binary's value is kept, not refused.
            pub fn parse_lenient(s: &str) -> Self {
                match s {
                    $( $text => $name::$variant, )+
                    other => $name::Other(other.to_string()),
                }
            }

            /// Parse a known spelling only. For user input, where a typo must
            /// be an error rather than a silently unknown value.
            pub fn parse_known(s: &str) -> Option<Self> {
                match Self::parse_lenient(s.trim()) {
                    $name::Other(_) => None,
                    known => Some(known),
                }
            }

            /// Whether this is a value this binary understands.
            pub fn is_known(&self) -> bool {
                !matches!(self, $name::Other(_))
            }

            /// The known spellings, comma-separated (for error messages).
            pub fn known_list() -> String {
                [$( $text ),+].join(", ")
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Ok(Self::parse_lenient(&s))
            }
        }
    };
}

open_enum! {
    /// What an edge asserts about its two memories.
    LinkKind {
        /// The two cannot both be true. Symmetric.
        Contradicts => "contradicts",
        /// The source replaces the target. Directed; the canonical record is
        /// the `supersedes` / `superseded_by` fields, which the edges table
        /// mirrors — a link of this kind is written only by
        /// `confirm_contradiction_direction`, which updates those fields too.
        Supersedes => "supersedes",
        /// The source is evidence for the target. Directed.
        Supports => "supports",
        /// The source was derived from the target. Directed.
        DerivedFrom => "derived_from",
        /// Loosely related. Symmetric.
        RelatedTo => "related_to",
        /// The two say the same thing. Symmetric.
        Duplicates => "duplicates",
    }
}

impl LinkKind {
    /// Symmetric kinds are stored once, on the writer's side; the edges
    /// table answers them from both ends. An unknown kind is never symmetric:
    /// treating a newer binary's directed kind as symmetric would make it
    /// show up on the wrong memory.
    pub fn is_symmetric(&self) -> bool {
        matches!(
            self,
            LinkKind::Contradicts | LinkKind::RelatedTo | LinkKind::Duplicates
        )
    }
}

open_enum! {
    /// Who or what proposed an edge.
    LinkOrigin {
        Human => "human",
        Agent => "agent",
        /// The NLI contradiction detector.
        Nli => "nli",
        /// Embedding similarity (neighbour search, compress candidates).
        Similarity => "similarity",
        /// Carried over by `compress` from a source onto the summary.
        Compress => "compress",
    }
}

open_enum! {
    /// Review state of an edge. Only `confirmed` edges are ever meant to
    /// change a score; `rejected` is kept (not deleted) so an automatic
    /// proposer does not propose the same edge again.
    LinkStatus {
        Proposed => "proposed",
        Confirmed => "confirmed",
        Rejected => "rejected",
    }
}

fn default_confidence() -> f64 {
    1.0
}

fn default_origin() -> LinkOrigin {
    // A link with no origin was typed into the file by hand.
    LinkOrigin::Human
}

fn default_status() -> LinkStatus {
    // Likewise: a hand-written edge is as confirmed as an edge can be.
    LinkStatus::Confirmed
}

/// One outbound edge, as stored in the source memory's file.
///
/// Every field but `to` and `kind` has a lenient default, because these
/// lines are hand-editable and a missing optional field must not make the
/// whole file unreadable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    /// The target's **full** id. Never a prefix: a prefix that is unique today
    /// stops being unique as the store grows, and the file outlives the store.
    pub to: String,
    pub kind: LinkKind,
    /// `None` = the target lives in the same store. Otherwise a project id,
    /// a group id (`__g_…`), or the global store's id (`__global_store__`,
    /// `engram_storage::paths::GLOBAL_PROJECT_ID`) — an id, never a path.
    #[serde(default)]
    pub store: Option<String>,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default = "default_origin")]
    pub origin: LinkOrigin,
    #[serde(default = "default_status")]
    pub status: LinkStatus,
    /// When the edge was written (or last changed status). `None` only for a
    /// hand-written line without one; readers fall back to the memory's
    /// `updated_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Link {
    /// A new edge stamped now.
    pub fn new(to: impl Into<String>, kind: LinkKind, origin: LinkOrigin) -> Self {
        Self {
            to: to.into(),
            kind,
            store: None,
            confidence: 1.0,
            origin,
            status: LinkStatus::Confirmed,
            at: Some(Utc::now()),
            note: None,
        }
    }

    /// Whether `self` and `other` name the same edge: same target, kind and
    /// store. Confidence, origin, status and note are attributes *of* an
    /// edge, so re-linking with a different confidence updates rather than
    /// duplicates.
    pub fn same_edge(&self, other: &Link) -> bool {
        self.to == other.to && self.kind == other.kind && self.store == other.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_values_round_trip_verbatim() {
        let json = r#"{"to":"x","kind":"refutes_v2","origin":"crowd","status":"disputed"}"#;
        let link: Link = serde_json::from_str(json).unwrap();
        assert_eq!(link.kind, LinkKind::Other("refutes_v2".into()));
        assert_eq!(link.origin, LinkOrigin::Other("crowd".into()));
        assert_eq!(link.status, LinkStatus::Other("disputed".into()));
        let back = serde_json::to_value(&link).unwrap();
        assert_eq!(back["kind"], "refutes_v2");
        assert_eq!(back["origin"], "crowd");
        assert_eq!(back["status"], "disputed");
    }

    #[test]
    fn known_values_parse_and_defaults_apply() {
        let link: Link = serde_json::from_str(r#"{"to":"x","kind":"derived_from"}"#).unwrap();
        assert_eq!(link.kind, LinkKind::DerivedFrom);
        assert_eq!(link.origin, LinkOrigin::Human);
        assert_eq!(link.status, LinkStatus::Confirmed);
        assert_eq!(link.confidence, 1.0);
        assert!(link.store.is_none() && link.at.is_none() && link.note.is_none());
    }

    #[test]
    fn parse_known_refuses_typos() {
        assert_eq!(LinkKind::parse_known("supports"), Some(LinkKind::Supports));
        assert_eq!(LinkKind::parse_known("suports"), None);
        assert!(!LinkKind::Other("x".into()).is_symmetric());
        assert!(LinkKind::Contradicts.is_symmetric());
        assert!(!LinkKind::Supersedes.is_symmetric());
    }
}
