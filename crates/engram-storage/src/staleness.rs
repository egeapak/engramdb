//! Tiered index-currency checking for the read path.
//!
//! [`MemoryStore::check_staleness`] runs on every `query` / `list` / `get`, so
//! what it is allowed to do is a *budget* decision, not a correctness one. This
//! module holds the part that is pure — what a pass observed, how two passes
//! are reconciled, and how a finding is worded — so that all of it is unit
//! testable without a store.
//!
//! # The tiers
//!
//! | tier | compares | cost |
//! |------|----------|------|
//! | [`Counts`] | row count vs `.md` count | per-fragment metadata + one `read_dir` |
//! | [`Size`] | plus id sets and per-file length | + one `statx` per file, one narrow projection |
//! | [`Content`] | plus SHA-256 of the length-matched files | + one read/hash per file |
//!
//! Each tier only *adds*; a higher tier reports everything a lower one would.
//!
//! # Why the default is `Size` and not `Content`
//!
//! Exact hashing is not a free upgrade over the count check it replaces.
//! `LanceIndex::count` is `Table::count_rows`, which reads per-fragment metadata
//! and touches no data file; hashing reads every memory file on every retrieval.
//! `Size` closes the practical blind spot — a hand edit, a `git checkout`, a
//! restore all change the file's length — for one `statx` per file, and leaves
//! the exact check to `doctor` and `reindex --dry-run`, which are throttled or
//! explicitly invoked and are the unbudgeted authority.
//!
//! # Why a finding is confirmed by a second pass
//!
//! The read path holds no lock (`write_lock.rs` offers neither a try-acquire nor
//! a shared mode), so a `create` or `update` committing between the file pass
//! and the index read is observed as a mismatch on a perfectly healthy store —
//! `create` writes the file and the row separately, with up to two dirent scans
//! in between. A single pass therefore cannot distinguish drift from a torn
//! read. [`StalenessFindings::confirmed_by`] intersects two passes: a real
//! drift is present in both, a torn read is present in at most one. The second
//! pass runs *only* when the first found something, so a healthy store pays
//! nothing for it.

use std::collections::BTreeSet;

use engram_types::config::StalenessCheck;

/// What one staleness pass observed.
///
/// Ids are held in `BTreeSet`s so that [`confirmed_by`](Self::confirmed_by) is
/// a set intersection and the rendered message is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StalenessFindings {
    /// `.md` files on disk. Counts *files*, not distinct ids — a stale
    /// duplicate left by a crashed rename is a real inconsistency and has
    /// always been reported here.
    pub md_count: usize,
    /// Rows in the index.
    pub lance_count: usize,
    /// Ids with a file but no row (tier [`Size`](StalenessCheck::Size)+).
    pub missing_from_index: BTreeSet<String>,
    /// Ids with a row but no file (tier [`Size`](StalenessCheck::Size)+).
    pub missing_from_disk: BTreeSet<String>,
    /// Ids whose file no longer matches the row it was indexed from — by
    /// length at tier [`Size`](StalenessCheck::Size), by hash at tier
    /// [`Content`](StalenessCheck::Content).
    pub changed: BTreeSet<String>,
    /// The `content` tier stopped at its byte budget before hashing every
    /// candidate, so `changed` is a floor rather than the exact answer.
    pub budget_exhausted: bool,
}

impl StalenessFindings {
    /// True when this pass found nothing worth reporting.
    ///
    /// A budget exhaustion alone is not a finding: it means the check was
    /// incomplete, not that the index is stale. Warning on every read of a
    /// store larger than the budget would be noise the user cannot act on
    /// except by raising the budget.
    pub fn is_clean(&self) -> bool {
        self.md_count == self.lance_count
            && self.missing_from_index.is_empty()
            && self.missing_from_disk.is_empty()
            && self.changed.is_empty()
    }

    /// Keep only what a second pass saw too.
    ///
    /// This is what turns a torn read into a no-op instead of a warning; see
    /// the module docs. A count mismatch is kept only if *both* passes
    /// disagreed; when it is kept, the later pass's numbers are reported, since
    /// that is the more current observation.
    pub fn confirmed_by(&self, other: &Self) -> Self {
        let counts_confirmed =
            self.md_count != self.lance_count && other.md_count != other.lance_count;
        Self {
            // Collapsing a settled count mismatch to equal numbers is what
            // drops the tier-A clause; the per-id clauses below carry their own
            // evidence and survive independently.
            md_count: if counts_confirmed {
                other.md_count
            } else {
                other.lance_count
            },
            lance_count: other.lance_count,
            missing_from_index: self
                .missing_from_index
                .intersection(&other.missing_from_index)
                .cloned()
                .collect(),
            missing_from_disk: self
                .missing_from_disk
                .intersection(&other.missing_from_disk)
                .cloned()
                .collect(),
            changed: self.changed.intersection(&other.changed).cloned().collect(),
            budget_exhausted: other.budget_exhausted,
        }
    }

    /// Render the user-facing warning, or `None` when nothing was found.
    ///
    /// The message **names the tier that found it**, because the tiers differ
    /// in what they can see and a user acting on this needs to know which
    /// check ran: "2 memories changed since indexing (size check)" invites a
    /// `doctor` run in a way that a bare count never would.
    pub fn message(&self, tier: StalenessCheck) -> Option<String> {
        if self.is_clean() {
            return None;
        }

        let mut clauses = Vec::new();
        // The raw counts are the *fallback* wording, for when nothing more
        // specific is known — the `counts` tier reads no ids at all, and at
        // higher tiers a duplicate file can outnumber the rows while every id
        // still matches. Whenever id-level detail exists it is strictly more
        // useful, so it replaces this rather than joining it.
        let ids_differ = !self.missing_from_index.is_empty() || !self.missing_from_disk.is_empty();
        if self.md_count != self.lance_count && !ids_differ {
            clauses.push(format!(
                "{} on disk, {} indexed",
                plural(self.md_count, "memory", "memories"),
                self.lance_count
            ));
        }
        if !self.missing_from_index.is_empty() {
            clauses.push(format!(
                "{} not indexed",
                plural(self.missing_from_index.len(), "memory", "memories")
            ));
        }
        if !self.missing_from_disk.is_empty() {
            clauses.push(format!(
                "{} indexed with no file",
                plural(self.missing_from_disk.len(), "memory", "memories")
            ));
        }
        if !self.changed.is_empty() {
            clauses.push(format!(
                "{} changed since indexing ({} check)",
                plural(self.changed.len(), "memory", "memories"),
                tier.as_str()
            ));
        }

        let mut msg = format!(
            "Index may be stale ({}). Run 'engramdb reindex' to rebuild.",
            clauses.join(", ")
        );
        if self.budget_exhausted {
            msg.push_str(
                " The content check stopped at its byte budget, so more may have changed — \
                 run 'engramdb doctor' for the complete check.",
            );
        }
        Some(msg)
    }
}

/// `3 memories` / `1 memory`, so no message ever reads "1 memories".
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", n, if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn balanced(n: usize) -> StalenessFindings {
        StalenessFindings {
            md_count: n,
            lance_count: n,
            ..Default::default()
        }
    }

    #[test]
    fn in_sync_store_is_clean_and_silent() {
        let f = balanced(5);
        assert!(f.is_clean());
        assert!(f.message(StalenessCheck::Size).is_none());
    }

    #[test]
    fn count_mismatch_reports_both_numbers() {
        let f = StalenessFindings {
            md_count: 3,
            lance_count: 5,
            ..Default::default()
        };
        let msg = f.message(StalenessCheck::Counts).unwrap();
        assert!(msg.contains("3 memories on disk, 5 indexed"), "{msg}");
        assert!(msg.contains("engramdb reindex"), "{msg}");
    }

    /// Blind spot the `size` tier exists to close: a delete paired with a
    /// create leaves the counts equal, so tier A sees nothing.
    #[test]
    fn equal_counts_with_differing_ids_are_still_reported() {
        let f = StalenessFindings {
            missing_from_index: ids(&["a"]),
            missing_from_disk: ids(&["b"]),
            ..balanced(4)
        };
        assert!(!f.is_clean());
        let msg = f.message(StalenessCheck::Size).unwrap();
        assert!(msg.contains("1 memory not indexed"), "{msg}");
        assert!(msg.contains("1 memory indexed with no file"), "{msg}");
        // Counts agree, so the count clause must not appear.
        assert!(!msg.contains("on disk,"), "{msg}");
    }

    /// Id-level detail replaces the raw counts rather than joining them —
    /// "1 memory on disk, 0 indexed, 1 memory not indexed" says one thing
    /// twice.
    #[test]
    fn id_detail_replaces_the_count_clause() {
        let f = StalenessFindings {
            md_count: 1,
            lance_count: 0,
            missing_from_index: ids(&["a"]),
            ..Default::default()
        };
        let msg = f.message(StalenessCheck::Size).unwrap();
        assert!(msg.contains("1 memory not indexed"), "{msg}");
        assert!(!msg.contains("on disk,"), "redundant count clause: {msg}");
    }

    /// A count mismatch with every id accounted for — a duplicate file left by
    /// a crashed rename — has no id-level wording available, so the counts are
    /// the only thing that can be said.
    #[test]
    fn count_clause_survives_when_no_id_detail_exists() {
        let f = StalenessFindings {
            md_count: 3,
            lance_count: 2,
            ..Default::default()
        };
        let msg = f.message(StalenessCheck::Size).unwrap();
        assert!(msg.contains("3 memories on disk, 2 indexed"), "{msg}");
    }

    /// The message names the tier, so a `size`-tier finding is never mistaken
    /// for an exact one.
    #[test]
    fn changed_message_names_the_tier_that_found_it() {
        let f = StalenessFindings {
            changed: ids(&["a", "b"]),
            ..balanced(4)
        };
        assert!(f
            .message(StalenessCheck::Size)
            .unwrap()
            .contains("2 memories changed since indexing (size check)"));
        assert!(f
            .message(StalenessCheck::Content)
            .unwrap()
            .contains("2 memories changed since indexing (content check)"));
    }

    #[test]
    fn singular_never_reads_as_plural() {
        let f = StalenessFindings {
            changed: ids(&["only"]),
            ..balanced(2)
        };
        let msg = f.message(StalenessCheck::Content).unwrap();
        assert!(msg.contains("1 memory changed"), "{msg}");
        assert!(!msg.contains("1 memories"), "{msg}");
    }

    /// An exhausted budget qualifies a finding but is not itself a finding —
    /// otherwise every read of a store larger than the budget would warn.
    #[test]
    fn budget_exhaustion_alone_does_not_warn() {
        let f = StalenessFindings {
            budget_exhausted: true,
            ..balanced(3)
        };
        assert!(f.is_clean());
        assert!(f.message(StalenessCheck::Content).is_none());
    }

    #[test]
    fn budget_exhaustion_qualifies_a_real_finding() {
        let f = StalenessFindings {
            changed: ids(&["a"]),
            budget_exhausted: true,
            ..balanced(3)
        };
        let msg = f.message(StalenessCheck::Content).unwrap();
        assert!(msg.contains("stopped at its byte budget"), "{msg}");
        assert!(msg.contains("engramdb doctor"), "{msg}");
    }

    // ===================================================================
    // Torn-read reconciliation
    // ===================================================================

    /// A finding only one pass saw is a torn read, not drift.
    #[test]
    fn finding_seen_by_one_pass_only_is_dropped() {
        let first = StalenessFindings {
            changed: ids(&["racing"]),
            ..balanced(3)
        };
        let second = balanced(3);
        let confirmed = first.confirmed_by(&second);
        assert!(
            confirmed.is_clean(),
            "a mismatch that cleared on re-read must not warn: {confirmed:?}"
        );
    }

    /// A real drift is present in both passes and survives.
    #[test]
    fn finding_seen_by_both_passes_survives() {
        let f = StalenessFindings {
            changed: ids(&["real"]),
            ..balanced(3)
        };
        let confirmed = f.confirmed_by(&f.clone());
        assert_eq!(confirmed.changed, ids(&["real"]));
        assert!(!confirmed.is_clean());
    }

    /// Two passes that each caught a *different* racing write agree on
    /// nothing, so nothing is reported.
    #[test]
    fn passes_disagreeing_on_which_id_drifted_report_nothing() {
        let first = StalenessFindings {
            changed: ids(&["a"]),
            ..balanced(3)
        };
        let second = StalenessFindings {
            changed: ids(&["b"]),
            ..balanced(3)
        };
        assert!(first.confirmed_by(&second).is_clean());
    }

    /// A count mismatch present in both passes is kept, and reports the
    /// *later* pass's numbers.
    #[test]
    fn confirmed_count_mismatch_reports_the_later_numbers() {
        let first = StalenessFindings {
            md_count: 3,
            lance_count: 5,
            ..Default::default()
        };
        let second = StalenessFindings {
            md_count: 4,
            lance_count: 5,
            ..Default::default()
        };
        let confirmed = first.confirmed_by(&second);
        assert_eq!((confirmed.md_count, confirmed.lance_count), (4, 5));
        assert!(!confirmed.is_clean());
    }

    /// A count mismatch that resolved between passes — exactly the shape of a
    /// `create` committing its file and its row across the two reads.
    #[test]
    fn count_mismatch_that_settles_is_dropped() {
        let first = StalenessFindings {
            md_count: 6,
            lance_count: 5,
            ..Default::default()
        };
        let second = balanced(6);
        assert!(first.confirmed_by(&second).is_clean());
    }

    /// Per-id findings must survive reconciliation even when the counts
    /// settled — they carry their own evidence and are not implied by counts.
    #[test]
    fn settled_counts_do_not_discard_confirmed_id_findings() {
        let drifted = StalenessFindings {
            md_count: 6,
            lance_count: 5,
            changed: ids(&["real"]),
            ..Default::default()
        };
        let settled = StalenessFindings {
            changed: ids(&["real"]),
            ..balanced(6)
        };
        let confirmed = drifted.confirmed_by(&settled);
        assert_eq!(confirmed.changed, ids(&["real"]));
        let msg = confirmed.message(StalenessCheck::Content).unwrap();
        assert!(msg.contains("1 memory changed"), "{msg}");
        assert!(!msg.contains("on disk,"), "count clause settled: {msg}");
    }
}
