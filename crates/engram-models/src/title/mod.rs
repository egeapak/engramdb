//! Title generation module for automatic memory title creation.
//!
//! Provides two strategies for generating short titles from memory content:
//! - **Keyword extraction** (default): Uses RAKE algorithm to extract key phrases.
//!   Lightweight, no model download required.
//! - **T5-Small ONNX**: Uses a T5-small summarization model for abstractive titles.
//!   Requires ~60MB model download on first use.

pub mod keyword;
mod pool;
#[cfg(feature = "onnxruntime")]
pub mod t5;

pub use pool::PooledTitleGenerator;

use anyhow::Result;
use async_trait::async_trait;

// `TitleStrategy` is a configuration value and lives in the `types` foundation
// (see `types::title_strategy`) so `types::config` can reference it without
// depending "upward" on this ONNX-backed module. Re-exported here for ergonomic
// `crate::title::TitleStrategy` access at the call sites that drive generation.
pub use engram_types::TitleStrategy;

/// Trait for title generators.
///
/// Implementations take memory content (summary + content) and produce
/// a short title (a few words) suitable for filenames.
#[async_trait]
pub trait TitleGenerator: Send + Sync {
    /// Generate a short title from the given text.
    ///
    /// The input is typically the memory's summary or content.
    /// Returns a title of a few words (typically 2-5 words).
    async fn generate(&self, text: &str) -> Result<String>;
}

/// Create a title generator for the given strategy.
///
/// Returns `None` if the strategy is `None` (disabled).
pub fn create_generator(strategy: TitleStrategy) -> Result<Option<Box<dyn TitleGenerator>>> {
    match strategy {
        TitleStrategy::Keyword => Ok(Some(Box::new(keyword::KeywordTitleGenerator::new()))),
        #[cfg(feature = "onnxruntime")]
        TitleStrategy::T5 => {
            let gen = t5::T5TitleGenerator::new()?;
            Ok(Some(Box::new(gen)))
        }
        // On a pure-`tract` build there is no ONNX Runtime, so T5 (a quantized
        // ONNX export) is unavailable — fall back to keyword titling rather
        // than failing the create path.
        #[cfg(not(feature = "onnxruntime"))]
        TitleStrategy::T5 => Ok(Some(Box::new(keyword::KeywordTitleGenerator::new()))),
        TitleStrategy::None => Ok(None),
    }
}

/// Generate a title from memory content using the specified strategy.
///
/// Convenience function that creates a generator and invokes it.
/// Returns `None` if the strategy is disabled or generation fails gracefully.
///
/// The T5 generator is cached process-wide: it is two ONNX sessions
/// (encoder + decoder) whose construction costs hundreds of ms — this is the
/// FALLBACK path used when the caller has no pooled engine generator (the
/// in-process CLI, or a failed bundle load), and rebuilding it per created
/// memory dwarfed the create itself. Keyword generation is allocation-cheap
/// and stays per-call. A failed T5 build is NOT cached, so a transient cause
/// (model download in progress) can recover on a later call.
pub async fn generate_title(strategy: TitleStrategy, text: &str) -> Option<String> {
    use std::sync::OnceLock;
    static T5_CACHE: OnceLock<tokio::sync::Mutex<Option<std::sync::Arc<dyn TitleGenerator>>>> =
        OnceLock::new();

    let generator: std::sync::Arc<dyn TitleGenerator> = match strategy {
        TitleStrategy::None => return None,
        TitleStrategy::Keyword => std::sync::Arc::new(keyword::KeywordTitleGenerator::new()),
        TitleStrategy::T5 => {
            let cell = T5_CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
            let mut guard = cell.lock().await;
            match guard.as_ref() {
                Some(cached) => std::sync::Arc::clone(cached),
                None => match t5::T5TitleGenerator::new() {
                    Ok(gen) => {
                        let gen: std::sync::Arc<dyn TitleGenerator> = std::sync::Arc::new(gen);
                        *guard = Some(std::sync::Arc::clone(&gen));
                        gen
                    }
                    Err(e) => {
                        tracing::warn!("Failed to create title generator: {}", e);
                        return None;
                    }
                },
            }
        }
    };

    match generator.generate(text).await {
        Ok(title) if !title.is_empty() => Some(title),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("Title generation failed: {}", e);
            None
        }
    }
}
