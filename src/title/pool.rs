//! Round-robin pool of title generators.
//!
//! T5 titling is the dominant `create`-path chokepoint at multi-tenant
//! scale: a single encoder+decoder session serializes behind its mutex, so
//! with K concurrent agents each `create` waits behind every predecessor
//! (measured: K=4 p99 ~370ms, K=8 ~710ms on an 8-core box). Spreading
//! generation across a small number of independent sessions cuts that tail.
//!
//! Unlike the embedding pool, T5 sessions are *direct* ORT sessions with an
//! explicit `intra_threads`, so the `pool_size × intra_threads ≤ cores`
//! oversubscription rule applies — callers size the pool and reduce each
//! session's `intra_threads` accordingly (see `resolve_engine_providers`).

use super::TitleGenerator;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A pool of independent title generators that round-robins requests across
/// them so concurrent `create`s don't all serialize behind one T5 session.
pub struct PooledTitleGenerator {
    members: Vec<Arc<dyn TitleGenerator>>,
    next: AtomicUsize,
}

impl PooledTitleGenerator {
    /// Build a pool from pre-constructed generators. Collapses to the sole
    /// member when there is one (no round-robin/`Arc` indirection), and
    /// yields `None` when empty so the caller treats T5 as unavailable
    /// exactly as it would for a single failed load. Returns a trait object
    /// (not `Self`) because the 0/1 cases are not a `Self`.
    pub fn build(members: Vec<Arc<dyn TitleGenerator>>) -> Option<Arc<dyn TitleGenerator>> {
        match members.len() {
            0 => None,
            1 => members.into_iter().next(),
            _ => Some(Arc::new(Self {
                members,
                next: AtomicUsize::new(0),
            })),
        }
    }

    /// Pick the next member, round-robin. `Relaxed` suffices — we only need
    /// even work distribution, not ordering between callers.
    fn pick(&self) -> &Arc<dyn TitleGenerator> {
        let i = self.next.fetch_add(1, Ordering::Relaxed) % self.members.len();
        &self.members[i]
    }
}

#[async_trait]
impl TitleGenerator for PooledTitleGenerator {
    async fn generate(&self, text: &str) -> Result<String> {
        self.pick().generate(text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    struct CountingGen {
        id: usize,
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl TitleGenerator for CountingGen {
        async fn generate(&self, _text: &str) -> Result<String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(format!("title-{}", self.id))
        }
    }

    fn make_gen(id: usize, calls: &Arc<AtomicU32>) -> Arc<dyn TitleGenerator> {
        Arc::new(CountingGen {
            id,
            calls: Arc::clone(calls),
        })
    }

    #[test]
    fn empty_pool_is_none() {
        assert!(PooledTitleGenerator::build(vec![]).is_none());
    }

    #[tokio::test]
    async fn single_member_collapses_to_that_member() {
        let calls = Arc::new(AtomicU32::new(0));
        let g = PooledTitleGenerator::build(vec![make_gen(9, &calls)]).expect("one member");
        assert_eq!(g.generate("x").await.unwrap(), "title-9");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn round_robins_across_members() {
        let c0 = Arc::new(AtomicU32::new(0));
        let c1 = Arc::new(AtomicU32::new(0));
        let pool = PooledTitleGenerator::build(vec![make_gen(0, &c0), make_gen(1, &c1)])
            .expect("two members");
        let mut seen = Vec::new();
        for _ in 0..6 {
            seen.push(pool.generate("x").await.unwrap());
        }
        assert_eq!(
            seen,
            vec!["title-0", "title-1", "title-0", "title-1", "title-0", "title-1"]
        );
        assert_eq!(c0.load(Ordering::Relaxed), 3);
        assert_eq!(c1.load(Ordering::Relaxed), 3);
    }
}
