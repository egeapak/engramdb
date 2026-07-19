//! Verify a memory is still accurate (§10.4).

use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::ops;
use engramdb::storage::MemoryStore;
use std::path::Path;

/// Stamp `verified_at = now` on a memory; clear a doctor-initiated
/// `NeedsReview` (see `ops::verify_memory`).
pub async fn run_verify(
    dir: &Path,
    global: bool,
    id: &str,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };

    let result = ops::verify_memory(&store, id).await?;

    if formatter.is_json() {
        println!(
            "{}",
            serde_json::json!({
                "id": result.id,
                "verified": true,
                "review_cleared": result.review_cleared,
            })
        );
    } else {
        formatter.print_success(&format!(
            "Verified memory {}{}",
            crate::output::short_id(&result.id),
            if result.review_cleared {
                " (doctor review cleared → active)"
            } else {
                ""
            }
        ));
    }
    Ok(())
}
