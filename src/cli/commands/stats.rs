//! Display statistics about the memory store.

use crate::cli::output::{OutputFormatter, Stats};
#[cfg(feature = "ollama")]
use crate::embeddings::{OllamaProvider, ALL_MINILM, MXBAI_EMBED_LARGE, NOMIC_EMBED_TEXT};
use crate::embeddings::{OnnxProvider, ONNX_MXBAI_EMBED_LARGE, ONNX_NOMIC_EMBED_TEXT};
use crate::ops::compute_stats;
use crate::storage::MemoryStore;
use crate::types::{EmbeddingBackend, Status};
use anyhow::Result;
use std::path::Path;

/// Display statistics about the memory store.
///
/// Shows total memory count, breakdown by type and status, logical scopes,
/// and average criticality.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `formatter` - Output formatter for displaying statistics
pub async fn run_stats(
    dir: &Path,
    embedding_backend: Option<EmbeddingBackend>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir).await?;
    let store_stats = compute_stats(&store).await?;

    // Extract health warning counts before moving data into Stats
    let challenged_count = store_stats
        .by_status
        .iter()
        .find(|(s, _)| matches!(s, Status::Challenged))
        .map(|(_, count)| *count)
        .unwrap_or(0);

    let needs_review_count = store_stats
        .by_status
        .iter()
        .find(|(s, _)| matches!(s, Status::NeedsReview))
        .map(|(_, count)| *count)
        .unwrap_or(0);

    let stats = Stats {
        total: store_stats.total,
        by_type: store_stats.by_type,
        by_status: store_stats.by_status,
        by_scope: store_stats.by_scope,
        expired: store_stats.expired,
        oldest: store_stats.oldest,
        newest: store_stats.newest,
        avg_criticality: store_stats.avg_criticality,
    };

    formatter.print_stats(&stats);

    // Print embeddings status
    println!();
    let config_path = dir.join(".engramdb/config.toml");
    let config = crate::storage::config::load_config(&config_path)
        .await
        .unwrap_or_default();
    let model = config.embeddings.provider.as_str();
    let backend = crate::ops::resolve_backend(config.embeddings.backend, embedding_backend);
    print_embeddings_status(model, backend).await;

    if challenged_count > 0 || needs_review_count > 0 {
        println!();
        println!("Health Warnings:");
        if challenged_count > 0 {
            formatter.print_error(&format!(
                "  {} memories are challenged (run 'engramdb review --challenged-only')",
                challenged_count
            ));
        }
        if needs_review_count > 0 {
            formatter.print_error(&format!(
                "  {} memories need review (run 'engramdb review --stale-only')",
                needs_review_count
            ));
        }
    }

    Ok(())
}

/// Print the embeddings availability status for the given model name and backend.
async fn print_embeddings_status(model: &str, backend: EmbeddingBackend) {
    let onnx_spec = match model {
        "onnx" | "all-minilm" => None, // uses default OnnxProvider::try_new()
        "nomic-embed-text" => Some(ONNX_NOMIC_EMBED_TEXT),
        "mxbai-embed-large" => Some(ONNX_MXBAI_EMBED_LARGE),
        other => {
            println!("Embeddings: Not available (unknown provider '{}')", other);
            return;
        }
    };

    let display_name = match model {
        "onnx" => "all-minilm",
        other => other,
    };

    // Check ONNX if backend allows it
    if backend != EmbeddingBackend::Ollama {
        let available = match &onnx_spec {
            None => OnnxProvider::try_new().is_some(),
            Some(spec) => OnnxProvider::try_with_model(spec.clone()).is_some(),
        };
        if available {
            println!("Embeddings: Available ({} via ONNX)", display_name);
            return;
        }
        if backend == EmbeddingBackend::Onnx {
            println!("Embeddings: Not available (run 'engramdb init' to download model)");
            return;
        }
    }

    // Check Ollama if backend allows it
    #[cfg(feature = "ollama")]
    if backend != EmbeddingBackend::Onnx {
        let ollama_spec = match model {
            "onnx" | "all-minilm" => ALL_MINILM,
            "nomic-embed-text" => NOMIC_EMBED_TEXT,
            _ => MXBAI_EMBED_LARGE,
        };
        if let Some(provider) = OllamaProvider::try_new(ollama_spec) {
            match provider.check_model_available().await {
                Ok(true) => {
                    println!("Embeddings: Available ({} via Ollama)", display_name);
                    return;
                }
                Ok(false) => {
                    println!("Embeddings: Not available (run 'engramdb init' to download model)");
                    return;
                }
                Err(_) => {}
            }
        }
    }

    println!("Embeddings: Not available (run 'engramdb init' to download model)");
}
