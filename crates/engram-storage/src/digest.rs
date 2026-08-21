//! Content identity for memory files — the input to index-currency checks.
//!
//! A memory's index row and its embedding vectors are *derived* from the `.md`
//! file. Nothing today records which bytes they were derived from, so a file
//! edited outside EngramDB — a hand edit, a `git checkout`, a merge, a restore
//! from backup — leaves the index serving the old summary, the old keyword
//! stems and the old vectors, with no surface reporting it. Neither of the
//! existing checks can see it: `check_staleness` compares *counts* and `doctor`
//! compares *id sets*, and both are invariant under in-place change.
//!
//! [`FileDigest`] is what makes that visible.

use sha2::{Digest, Sha256};

/// A memory file's content identity: a SHA-256 over its line-ending-normalized
/// bytes, plus its raw on-disk length.
///
/// **The two fields deliberately measure different things**, because they serve
/// two different checks:
///
/// - [`sha256`](Self::sha256) is the **authority**. It hashes the file with
///   CRLF terminators normalized to LF, so a `git core.autocrlf` rewrite — which
///   changes every byte offset while parsing to a byte-identical `Memory` — does
///   not read as drift. Without that, a Windows checkout would report its entire
///   store as drifted, permanently and unrepairably: a reindex would restamp the
///   CRLF digest, the next store-written file would be LF again, and the next
///   checkout would flip it back.
/// - [`len`](Self::len) is the **cheap discriminator**: the raw byte length as
///   `stat(2)` reports it, so the hot-path staleness check can compare it
///   without reading a single file. It is raw rather than normalized precisely
///   so it matches what `metadata().len()` returns. On a CRLF checkout it will
///   differ from an LF-stamped row exactly once, until the next reindex restamps
///   it; the hash tier is immune and is the one `doctor` uses.
///
/// Normalization uses [`memory_file::is_crlf_terminated`], the *same*
/// whole-file decision the parser makes. That is not a detail: if the digest
/// normalized a file the parser treats as LF-with-embedded-`\r`, two files that
/// parse to different memories could hash identically — a missed drift, which is
/// the dangerous direction.
///
/// [`memory_file::is_crlf_terminated`]: crate::memory_file::is_crlf_terminated
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDigest {
    /// Lowercase hex SHA-256 of the normalized bytes.
    pub sha256: String,
    /// Raw on-disk byte length, comparable with `std::fs::Metadata::len`.
    pub len: u64,
}

impl FileDigest {
    /// Compute the digest of the exact byte string that is (or was) written to
    /// disk.
    ///
    /// **Never** call this on a re-serialization of a parsed memory. A file
    /// written by an older binary, or edited by hand, need not round-trip to
    /// identical bytes — so hashing `write_memory_file(parse(f))` would mark
    /// every such file permanently dirty and no currency check would ever
    /// settle. The write path passes the string it is about to `atomic_write`;
    /// the read path passes the bytes it just read.
    ///
    /// Non-UTF-8 input is hashed verbatim (no normalization is attempted): the
    /// parser cannot read such a file either, so it can only ever be reported as
    /// drifted, which is the correct conservative direction.
    pub fn of(bytes: &[u8]) -> Self {
        let len = bytes.len() as u64;
        let sha256 = match std::str::from_utf8(bytes) {
            Ok(text) => hex_sha256(normalize_line_endings(text).as_bytes()),
            Err(_) => hex_sha256(bytes),
        };
        Self { sha256, len }
    }
}

/// Strip CRLF terminators when — and only when — the file is CRLF-terminated as
/// a whole, mirroring the parser's decision exactly.
///
/// Returns a borrowed `Cow` for the overwhelmingly common LF case, so the
/// normalization costs nothing on the platform that does not need it.
fn normalize_line_endings(text: &str) -> std::borrow::Cow<'_, str> {
    if crate::memory_file::is_crlf_terminated(text) {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Lowercase hex of a SHA-256.
///
/// `sha2` 0.11's `finalize` returns a `hybrid_array::Array` with no `LowerHex`,
/// so `{:x}` does not compile — the same gotcha `transcript_archive::hex_digest`,
/// `project_id::hash_to_id` and `ops::harvest::index_text_digest` all document.
fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LF: &str = "---\ntitle: T\n---\n\n## Summary\n\nline one\nline two\n";

    #[test]
    fn digest_is_stable_and_lowercase_hex() {
        let a = FileDigest::of(LF.as_bytes());
        let b = FileDigest::of(LF.as_bytes());
        assert_eq!(a, b);
        assert_eq!(a.sha256.len(), 64);
        assert!(a
            .sha256
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        assert_eq!(a.len, LF.len() as u64);
    }

    #[test]
    fn different_content_gives_different_digest() {
        let other = LF.replace("line one", "line ONE");
        assert_ne!(
            FileDigest::of(LF.as_bytes()).sha256,
            FileDigest::of(other.as_bytes()).sha256
        );
    }

    /// THE `core.autocrlf` GUARD: a CRLF rewrite of the same file must hash
    /// identically, or a Windows checkout reports its whole store as drifted
    /// forever. The raw `len` is expected to differ — that is what makes it the
    /// cheap-but-approximate tier.
    #[test]
    fn crlf_rewrite_hashes_identically_to_lf_original() {
        let crlf = LF.replace('\n', "\r\n");
        let lf_digest = FileDigest::of(LF.as_bytes());
        let crlf_digest = FileDigest::of(crlf.as_bytes());
        assert_eq!(
            lf_digest.sha256, crlf_digest.sha256,
            "a git core.autocrlf rewrite must not read as content drift"
        );
        assert!(
            crlf_digest.len > lf_digest.len,
            "raw length is deliberately NOT normalized"
        );
    }

    /// The dangerous direction: a lone `\r` inside content is NOT a line
    /// terminator (the parser keeps it verbatim), so it must survive hashing.
    /// Normalizing it away would let two files that parse to different memories
    /// hash identically — a drift the check could never see.
    #[test]
    fn embedded_carriage_return_is_not_normalized_away() {
        let with_cr = "---\ntitle: T\n---\n\n## Summary\n\nalpha\r beta\ngamma\n";
        let without = "---\ntitle: T\n---\n\n## Summary\n\nalpha beta\ngamma\n";
        assert!(
            !crate::memory_file::is_crlf_terminated(with_cr),
            "a single embedded CR must not make this a CRLF file"
        );
        assert_ne!(
            FileDigest::of(with_cr.as_bytes()).sha256,
            FileDigest::of(without.as_bytes()).sha256
        );
    }

    /// Normalization must agree with the parser's whole-file decision on a file
    /// that is CRLF *except* for one line — the parser treats it as LF, so the
    /// digest must too (hash verbatim), not silently collapse the CRLFs.
    #[test]
    fn mixed_endings_are_hashed_verbatim() {
        let mixed = "---\r\ntitle: T\r\n---\r\n\n## Summary\r\n\r\nbody\r\n";
        assert!(!crate::memory_file::is_crlf_terminated(mixed));
        let collapsed = mixed.replace("\r\n", "\n");
        assert_ne!(
            FileDigest::of(mixed.as_bytes()).sha256,
            FileDigest::of(collapsed.as_bytes()).sha256,
            "a mixed-ending file is not a CRLF file; collapsing it would hide drift"
        );
    }

    #[test]
    fn empty_and_non_utf8_input_do_not_panic() {
        assert_eq!(FileDigest::of(b"").len, 0);
        let bad = [0xffu8, 0xfe, 0x00, 0x41];
        let d = FileDigest::of(&bad);
        assert_eq!(d.len, 4);
        assert_eq!(d.sha256.len(), 64);
    }
}
