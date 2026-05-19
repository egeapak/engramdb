//! Round-robin pool of embedding providers.
//!
//! A single fastembed `TextEmbedding` serializes all inference behind its
//! `Arc<Mutex<_>>`, so under concurrent load — the multi-tenant daemon model:
//! one daemon serving N agent sessions — aggregate embedding throughput is
//! mutex-bound (flat as concurrency rises; per-caller latency scales
//! linearly). Spreading requests across several independent sessions lifts
//! that ceiling.
//!
//! Measured on an 8-core Apple Silicon box (int8 all-MiniLM), a pool of
//! `cores/2` delivers ~+166–195% aggregate throughput at 4–8 concurrent
//! callers over a single session, and beats a pool of 2 on both throughput
//! and p99. fastembed manages its own internal threadpool, so the
//! `pool_size × intra_threads ≤ cores` rule that bounds the direct-ORT
//! NLI/T5 sessions does not constrain the embedding pool.

use super::EmbeddingProvider;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A pool of independent embedding providers that round-robins requests
/// across them so concurrent callers don't all serialize behind one model
/// session's mutex. Every member must be the same model (callers build them
/// from a single spec), so metadata (`dimensions` / `max_tokens` /
/// `model_id`) is answered from the first member.
pub struct PooledEmbeddingProvider {
    members: Vec<Arc<dyn EmbeddingProvider>>,
    next: AtomicUsize,
}

impl PooledEmbeddingProvider {
    /// Build a pool from pre-constructed providers.
    ///
    /// Collapses to the sole provider when `members.len() == 1` (no point
    /// paying the round-robin indirection or an extra `Arc` hop), and yields
    /// `None` when empty so the caller treats embeddings as unavailable
    /// exactly as it would for a failed single load. Returns a trait object
    /// (not `Self`) precisely because the 0/1 cases are not a `Self`.
    pub fn build(members: Vec<Arc<dyn EmbeddingProvider>>) -> Option<Arc<dyn EmbeddingProvider>> {
        match members.len() {
            0 => None,
            // A 1-element iterator's `next()` is `Some(member)` — no unwrap.
            1 => members.into_iter().next(),
            _ => Some(Arc::new(Self {
                members,
                next: AtomicUsize::new(0),
            })),
        }
    }

    /// Pick the next member, round-robin. `Relaxed` suffices: we only need
    /// even work distribution, not a happens-before relationship between
    /// callers.
    fn pick(&self) -> &Arc<dyn EmbeddingProvider> {
        let i = self.next.fetch_add(1, Ordering::Relaxed) % self.members.len();
        &self.members[i]
    }
}

#[async_trait]
impl EmbeddingProvider for PooledEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.pick().embed(text).await
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.pick().embed_batch(texts).await
    }

    fn dimensions(&self) -> usize {
        self.members[0].dimensions()
    }

    fn max_tokens(&self) -> usize {
        self.members[0].max_tokens()
    }

    fn model_id(&self) -> String {
        self.members[0].model_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// Records how many times it was called and reports its own id as the
    /// embedding, so a test can see which member served each request.
    struct CountingProvider {
        id: usize,
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl EmbeddingProvider for CountingProvider {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![self.id as f32])
        }
        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(texts.iter().map(|_| vec![self.id as f32]).collect())
        }
        fn dimensions(&self) -> usize {
            384
        }
        fn max_tokens(&self) -> usize {
            256
        }
        fn model_id(&self) -> String {
            "mock/model".to_string()
        }
    }

    fn provider(id: usize, calls: &Arc<AtomicU32>) -> Arc<dyn EmbeddingProvider> {
        Arc::new(CountingProvider {
            id,
            calls: Arc::clone(calls),
        })
    }

    #[test]
    fn empty_pool_is_none() {
        assert!(PooledEmbeddingProvider::build(vec![]).is_none());
    }

    #[tokio::test]
    async fn single_member_collapses_to_that_member() {
        let calls = Arc::new(AtomicU32::new(0));
        let p = PooledEmbeddingProvider::build(vec![provider(7, &calls)])
            .expect("one member yields a provider");
        assert_eq!(p.embed("x").await.unwrap(), vec![7.0]);
        assert_eq!(p.model_id(), "mock/model");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn round_robins_across_members_and_delegates_metadata() {
        let c0 = Arc::new(AtomicU32::new(0));
        let c1 = Arc::new(AtomicU32::new(0));
        let c2 = Arc::new(AtomicU32::new(0));
        let pool = PooledEmbeddingProvider::build(vec![
            provider(0, &c0),
            provider(1, &c1),
            provider(2, &c2),
        ])
        .expect("three members yield a pool");

        // 9 sequential calls over 3 members → strict 0,1,2 rotation.
        let mut seen = Vec::new();
        for _ in 0..9 {
            seen.push(pool.embed("x").await.unwrap()[0] as usize);
        }
        assert_eq!(seen, vec![0, 1, 2, 0, 1, 2, 0, 1, 2]);
        assert_eq!(c0.load(Ordering::Relaxed), 3);
        assert_eq!(c1.load(Ordering::Relaxed), 3);
        assert_eq!(c2.load(Ordering::Relaxed), 3);

        // batch path rotates too (continues the same cursor).
        let _ = pool.embed_batch(&["a", "b"]).await.unwrap();
        assert_eq!(c0.load(Ordering::Relaxed), 4);

        // metadata answered from member 0.
        assert_eq!(pool.dimensions(), 384);
        assert_eq!(pool.max_tokens(), 256);
        assert_eq!(pool.model_id(), "mock/model");
    }
}
