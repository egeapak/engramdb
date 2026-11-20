//! SHA-256 check of the built-in model files EngramDB fetches itself.
//!
//! Every loader that fetches files through `hf-hub` directly (the embedding
//! `hf_override` specs, NLI, T5, the user-defined rerankers) calls [`verify`]
//! on each file after `repo.get(file)` and before reading it. A file whose
//! digest is listed in [`DIGESTS`] and does not match is refused: the loader
//! fails, and the model's caller falls back as for a missing model (keyword
//! search, keyword titles, no NLI). Repositories track `main` with no
//! revision pin (a pin changes the cache layout and would make every user
//! download every model again), so a digest is what ties a file to the bytes
//! that were reviewed.
//!
//! What is **not** covered, declared:
//! - Files with no digest here load unchecked: models `fastembed` downloads
//!   itself (its registry), and the built-in specs whose files were not
//!   available when the table was written (`Xenova/all-MiniLM-L6-v2`,
//!   `optimum/t5-small`). Add their digests from the published files; never
//!   copy one from anywhere else.
//! - A custom `[nli].model` repository (opt-in only) has no digest.
//!
//! Hashing a model file costs ~1 s per 300 MB, so a verified result is
//! memoized in a sidecar under the model cache (`.verified/`), keyed by the
//! file's resolved path and checked against its length and mtime; a file
//! that changes is hashed again. The snapshot entry's blob name is **not**
//! trusted as a digest: `hf-hub` names the blob after the server's `ETag`
//! without hashing the bytes, so a compromised upstream would choose it.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// `(repository, file, sha256)` for every built-in model file with a known
/// digest. Taken from the files `main` served on 2026-11-14 (each LFS file
/// also checked against the Hub's `x-linked-etag`).
pub const DIGESTS: &[(&str, &str, &str)] = &[
    // Embeddings: `all-minilm-l12` (the default), uint8.
    (
        "Xenova/all-MiniLM-L12-v2",
        "onnx/model_uint8.onnx",
        "597c3881f7cb587375b62b6d79f6a87ad34f8d69c9cb7b5e068018f6bd393e7c",
    ),
    (
        "Xenova/all-MiniLM-L12-v2",
        "tokenizer.json",
        "da0e79933b9ed51798a3ae27893d3c5fa4a201126cef75586296df9b4d2c62a0",
    ),
    (
        "Xenova/all-MiniLM-L12-v2",
        "config.json",
        "98cfb098e4f623321632b32b3e62d58ea49601e90dd3c13736c42812b4533ab7",
    ),
    (
        "Xenova/all-MiniLM-L12-v2",
        "special_tokens_map.json",
        "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3",
    ),
    (
        "Xenova/all-MiniLM-L12-v2",
        "tokenizer_config.json",
        "9261e7d79b44c8195c1cada2b453e55b00aeb81e907a6664974b4d7776172ab3",
    ),
    // T5 titles (the default), quantized.
    (
        "Xenova/t5-small",
        "onnx/encoder_model_quantized.onnx",
        "966c5cb80c2805821aa9eac5fb26dcaad44de0fee11b990ffc97500ae83044a7",
    ),
    (
        "Xenova/t5-small",
        "onnx/decoder_model_quantized.onnx",
        "fe424ae0e0f10b6219a2a839c894df55c64158897bfd7dda6db155179cdb6bf4",
    ),
    (
        "Xenova/t5-small",
        "tokenizer.json",
        "5f0ed8ab5b8cfa9812bb73752f1d80c292e52bcf5a87a144dc9ab2d251056cbb",
    ),
    // NLI (the default, quantized) and the fp32 original.
    (
        "Xenova/nli-deberta-v3-xsmall",
        "onnx/model_quantized.onnx",
        "3fac2500c45c75af42c7711de0d1b93d59577456100208be0dc1f9e8811946b6",
    ),
    (
        "Xenova/nli-deberta-v3-xsmall",
        "tokenizer.json",
        "a86f883318afa11c8c10466f1bf4efaeb6ded28a52cbe57217a8fa0d0a2a87df",
    ),
    (
        "cross-encoder/nli-deberta-v3-xsmall",
        "onnx/model.onnx",
        "7105da41f625c42eca24e9465ec99150d02a80e644659d7a1daa93a6357155d4",
    ),
    (
        "cross-encoder/nli-deberta-v3-xsmall",
        "tokenizer.json",
        "5124ef2ead1a10a717703bc436de7f353da76d6340e4587719b42b1693707964",
    ),
    // Reranker `jina-turbo-q` (the default), uint8.
    (
        "jinaai/jina-reranker-v1-turbo-en",
        "onnx/model_uint8.onnx",
        "4f6edff0634c97c4ede6724da19d1c536e95fc5844a8b4597e027c3531cdbf71",
    ),
    (
        "jinaai/jina-reranker-v1-turbo-en",
        "tokenizer.json",
        "0046da43cc8c424b317f56b092b0512aaaa65c4f925d2f16af9d9eeb4d0ef902",
    ),
    (
        "jinaai/jina-reranker-v1-turbo-en",
        "config.json",
        "e050ff6a15ae9295e84882fa0e98051bd8754856cd5201395ebf00ce9f2d609b",
    ),
    (
        "jinaai/jina-reranker-v1-turbo-en",
        "special_tokens_map.json",
        "06e405a36dfe4b9604f484f6a1e619af1a7f7d09e34a8555eb0b77b66318067f",
    ),
    (
        "jinaai/jina-reranker-v1-turbo-en",
        "tokenizer_config.json",
        "d291c6652d96d56ffdbcf1ea19d9bae5ed79003f7648c627e725a619227ce8fa",
    ),
];

/// The expected SHA-256 of `file` in `repo`, when one is known.
pub fn expected_sha256(repo: &str, file: &str) -> Option<&'static str> {
    DIGESTS
        .iter()
        .find(|(r, f, _)| *r == repo && *f == file)
        .map(|(_, _, d)| *d)
}

/// Refuse `path` (the local copy of `file` in `repo`) when its SHA-256 is
/// listed and differs. A file with no listed digest passes unchecked.
pub fn verify(repo: &str, file: &str, path: &Path) -> Result<()> {
    let Some(expected) = expected_sha256(repo, file) else {
        return Ok(());
    };
    let memo = engram_storage::paths::model_cache_dir()
        .ok()
        .map(|c| c.join(".verified"));
    verify_against(path, expected, memo.as_deref())
        .with_context(|| format!("model file {repo}/{file}"))
}

/// [`verify`] against an explicit digest and memo folder (`None`: no memo).
pub fn verify_against(path: &Path, expected: &str, memo_dir: Option<&Path>) -> Result<()> {
    let resolved =
        std::fs::canonicalize(path).with_context(|| format!("resolve {}", path.display()))?;
    let meta =
        std::fs::metadata(&resolved).with_context(|| format!("stat {}", resolved.display()))?;
    let stamp = format!(
        "{} {}",
        meta.len(),
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let memo_file =
        memo_dir.map(|d| d.join(hex(&Sha256::digest(resolved.to_string_lossy().as_bytes()))));
    if let Some(memo) = &memo_file {
        if let Ok(text) = std::fs::read_to_string(memo) {
            if text == format!("{stamp} {expected}") {
                return Ok(());
            }
        }
    }
    let actual = sha256_file(&resolved)?;
    if actual != expected {
        bail!(
            "{} has SHA-256 {actual}, expected {expected}; refusing to load it. \
             Delete it from the model cache to download it again.",
            resolved.display()
        );
    }
    if let (Some(dir), Some(memo)) = (memo_dir, &memo_file) {
        // Best effort: a memo that cannot be written only costs a re-hash.
        let _ = write_memo(dir, memo, &format!("{stamp} {expected}"));
    }
    Ok(())
}

fn write_memo(dir: &Path, memo: &Path, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, text.as_bytes())?;
    tmp.persist(memo).map_err(|e| e.error)?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    engram_types::hex::lower_hex(bytes)
}

/// The cached copy of `file` in `repo` under `cache_dir`, if `main` has one
/// (`models--<org>--<name>/refs/main` → `snapshots/<commit>/<file>`). Reads
/// only; never downloads.
pub fn cached_file(cache_dir: &Path, repo: &str, file: &str) -> Option<PathBuf> {
    let root = cache_dir.join(format!("models--{}", repo.replace('/', "--")));
    let commit = std::fs::read_to_string(root.join("refs").join("main")).ok()?;
    let path = root.join("snapshots").join(commit.trim()).join(file);
    path.is_file().then_some(path)
}

/// Every cached built-in file under `cache_dir` whose digest does not match,
/// as `(repo/file, reason)`. For `doctor`; memoized like [`verify`].
pub fn check_cache(cache_dir: &Path) -> Vec<(String, String)> {
    let memo = cache_dir.join(".verified");
    DIGESTS
        .iter()
        .filter_map(|(repo, file, expected)| {
            let path = cached_file(cache_dir, repo, file)?;
            verify_against(&path, expected, Some(&memo))
                .err()
                .map(|e| (format!("{repo}/{file}"), format!("{e:#}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha_of(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    #[test]
    fn a_model_file_with_the_wrong_digest_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("model.onnx");
        std::fs::write(&path, b"tampered weights").unwrap();
        let err = verify_against(&path, &sha_of(b"reviewed weights"), None)
            .expect_err("a mismatching file must be refused");
        assert!(err.to_string().contains("refusing to load"), "{err}");
    }

    /// A hand-staged cache holds plain files, not blob symlinks: it passes by
    /// hashing, and the memo then stands in for the hash until the file
    /// changes.
    #[test]
    fn a_hand_staged_cache_passes_by_hash() {
        let tmp = tempfile::TempDir::new().unwrap();
        let memo = tmp.path().join(".verified");
        let path = tmp.path().join("tokenizer.json");
        std::fs::write(&path, b"{\"version\":1}").unwrap();
        let good = sha_of(b"{\"version\":1}");
        verify_against(&path, &good, Some(&memo)).expect("the right digest passes");
        assert_eq!(std::fs::read_dir(&memo).unwrap().count(), 1, "memo written");
        verify_against(&path, &good, Some(&memo)).expect("and passes from the memo");

        // Same length, new bytes: the stamp no longer matches, so it is
        // hashed again and refused.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, b"{\"version\":2}").unwrap();
        assert!(verify_against(&path, &good, Some(&memo)).is_err());
    }

    #[test]
    fn a_file_with_no_digest_passes_unchecked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("x");
        std::fs::write(&path, b"anything").unwrap();
        verify("someone/custom-repo", "onnx/model.onnx", &path).unwrap();
    }

    #[test]
    fn cached_file_follows_refs_main() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("models--org--name");
        std::fs::create_dir_all(root.join("refs")).unwrap();
        std::fs::write(root.join("refs/main"), "abc\n").unwrap();
        std::fs::create_dir_all(root.join("snapshots/abc/onnx")).unwrap();
        std::fs::write(root.join("snapshots/abc/onnx/m.onnx"), b"m").unwrap();
        assert_eq!(
            cached_file(tmp.path(), "org/name", "onnx/m.onnx"),
            Some(root.join("snapshots/abc/onnx/m.onnx"))
        );
        assert_eq!(cached_file(tmp.path(), "org/name", "absent"), None);
    }

    /// Every digest is 64 lowercase hex characters, and listed once.
    #[test]
    fn digests_are_well_formed_and_unique() {
        for (i, (repo, file, d)) in DIGESTS.iter().enumerate() {
            assert!(
                d.len() == 64
                    && d.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{repo}/{file}: {d}"
            );
            assert!(
                !DIGESTS[..i].iter().any(|(r, f, _)| r == repo && f == file),
                "{repo}/{file} listed twice"
            );
        }
    }

    /// The shipped defaults are all covered.
    #[cfg(feature = "onnxruntime")]
    #[test]
    fn every_default_model_file_has_a_digest() {
        let nli = crate::nli::DEFAULT_NLI_MODEL;
        let t5 = crate::title::t5::DEFAULT_T5_MODEL;
        let rerank = crate::rerank::DEFAULT_RERANK_SPEC;
        let emb = crate::embeddings::DEFAULT_ONNX_EMBEDDING
            .hf_override
            .expect("the default embedding is fetched through hf-hub");
        let mut files = vec![
            (nli.repo, nli.model_file),
            (nli.repo, nli.tokenizer_file),
            (t5.repo, t5.encoder_file),
            (t5.repo, t5.decoder_file),
            (t5.repo, t5.tokenizer_file),
            (rerank.repo, rerank.model_file),
            (emb.repo, emb.model_file),
        ];
        for aux in [
            "tokenizer.json",
            "config.json",
            "special_tokens_map.json",
            "tokenizer_config.json",
        ] {
            files.push((rerank.repo, aux));
            files.push((emb.repo, aux));
        }
        for (repo, file) in files {
            assert!(expected_sha256(repo, file).is_some(), "{repo}/{file}");
        }
    }
}
