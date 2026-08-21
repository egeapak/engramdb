//! Measure what `reindex --incremental` actually saves.
//!
//! Phase 3 of the index-currency work is opt-in precisely because its ceiling
//! is bounded by something the skip cannot avoid: deciding whether a file
//! changed requires reading and hashing it. Only the parse, the keyword-stem
//! derivation and the row write are skipped.
//!
//! Run with `cargo run --release --example reindex_incremental_bench`.
//! Debug timings are meaningless here — the parse and stem work this measures
//! is exactly what optimisation affects most.

use engramdb::storage::{InMemoryRegistry, MemoryStore};
use engramdb::types::{Memory, MemoryType, Provenance};
use std::time::Instant;

const SIZES: [usize; 3] = [100, 500, 2000];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!(
        "{:>7}  {:>12}  {:>12}  {:>10}  {:>9}",
        "memories", "full (ms)", "incr (ms)", "saved (ms)", "saved (%)"
    );

    for n in SIZES {
        let tmp = tempfile::TempDir::new()?;
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new()).await?;

        for i in 0..n {
            let m = Memory::new(
                MemoryType::Decision,
                format!("Memory number {i} about a decision that was taken"),
                format!(
                    "Body text for memory {i}. It is long enough to be worth parsing and \
                     stemming, mentioning modules, functions and conventions the way a real \
                     memory would, so the keyword derivation has genuine work to do."
                ),
                Provenance::human(),
            );
            store.create(&m).await?;
        }

        // Warm: both arms measure a steady state, not first-touch I/O.
        store.reindex().await?;

        let t = Instant::now();
        store.reindex().await?;
        let full = t.elapsed().as_secs_f64() * 1000.0;

        let t = Instant::now();
        let counts = store.reindex_incremental().await?;
        let incr = t.elapsed().as_secs_f64() * 1000.0;

        assert_eq!(
            counts.skipped, n,
            "every row should skip in the steady state"
        );

        println!(
            "{n:>7}  {full:>12.1}  {incr:>12.1}  {:>10.1}  {:>8.1}%",
            full - incr,
            (full - incr) / full * 100.0
        );
    }

    println!(
        "\nBoth arms enumerate, read and hash every file; only the parse, the \
         stem derivation and the row write differ."
    );
    Ok(())
}
