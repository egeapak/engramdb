//! Ollama-based embedding provider.
//!
//! Connects to a running Ollama server to generate embeddings via its
//! `/api/embed` endpoint.  The server address is read from the `OLLAMA_HOST`
//! environment variable (default `http://localhost:11434`).

use super::{EmbeddingError, EmbeddingProvider};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Static specification for an Ollama-hosted embedding model.
#[derive(Debug, Clone, Copy)]
pub struct OllamaModelSpec {
    pub model_name: &'static str,
    pub dimensions: usize,
    pub max_tokens: usize,
}

pub const NOMIC_EMBED_TEXT: OllamaModelSpec = OllamaModelSpec {
    model_name: "nomic-embed-text",
    dimensions: 768,
    max_tokens: 8192,
};

pub const MXBAI_EMBED_LARGE: OllamaModelSpec = OllamaModelSpec {
    model_name: "mxbai-embed-large",
    dimensions: 1024,
    max_tokens: 512,
};

pub const ALL_MINILM: OllamaModelSpec = OllamaModelSpec {
    model_name: "all-minilm",
    dimensions: 384,
    max_tokens: 256,
};

/// Embedding provider backed by an Ollama server.
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    spec: OllamaModelSpec,
}

#[derive(Serialize)]
struct EmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
}

#[derive(Serialize)]
struct PullRequest {
    name: String,
}

impl OllamaProvider {
    /// Create a new Ollama provider for the given model spec.
    ///
    /// Reads `OLLAMA_HOST` env var for the server address (default
    /// `http://localhost:11434`).  The reqwest client is configured with a
    /// 30-second timeout for embed requests.
    pub fn new(spec: OllamaModelSpec) -> Result<Self> {
        let base_url =
            std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            base_url,
            spec,
        })
    }

    /// Try to create a new provider, returning `None` on failure.
    pub fn try_new(spec: OllamaModelSpec) -> Option<Self> {
        Self::new(spec).ok()
    }

    /// Check whether the configured model is already pulled on the server.
    pub async fn check_model_available(&self) -> Result<bool> {
        let url = format!("{}/api/tags", self.base_url);
        let resp: TagsResponse = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to Ollama server")?
            .json()
            .await
            .context("Failed to parse Ollama tags response")?;

        let target = self.spec.model_name;
        Ok(resp.models.iter().any(|m| {
            let name = m.name.as_str();
            // Ollama stores names as "model:latest" — match with or without tag
            name == target
                || name == format!("{}:latest", target)
                || name.starts_with(&format!("{}:", target))
        }))
    }

    /// Pull the model from the Ollama library (streaming; consumes the
    /// response with a 10-minute timeout).
    pub async fn pull_model(&self) -> Result<()> {
        let url = format!("{}/api/pull", self.base_url);
        let pull_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .context("Failed to build pull client")?;

        let resp = pull_client
            .post(&url)
            .json(&PullRequest {
                name: self.spec.model_name.to_string(),
            })
            .send()
            .await
            .context("Failed to send pull request to Ollama")?;

        if !resp.status().is_success() {
            anyhow::bail!("Ollama pull failed with status {}", resp.status());
        }

        // Consume the streaming response to completion
        let _body = resp
            .bytes()
            .await
            .context("Failed to read pull response stream")?;

        Ok(())
    }

    /// Return a human-readable hint for installing the model.
    pub fn installation_hint(&self) -> String {
        format!(
            "Install Ollama (https://ollama.ai), then: ollama pull {}",
            self.spec.model_name
        )
    }
}

#[async_trait]
impl EmbeddingProvider for OllamaProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed_batch(&[text]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::Failed("No embedding returned".to_string()).into())
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let url = format!("{}/api/embed", self.base_url);
        let request = EmbedRequest {
            model: self.spec.model_name.to_string(),
            input: texts.iter().map(|t| t.to_string()).collect(),
        };

        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to send embed request to Ollama")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Ollama embed failed ({}): {}", status, body);
        }

        let embed_resp: EmbedResponse = resp
            .json()
            .await
            .context("Failed to parse Ollama embed response")?;

        if embed_resp.embeddings.len() != texts.len() {
            anyhow::bail!(
                "Ollama returned {} embeddings for {} inputs",
                embed_resp.embeddings.len(),
                texts.len()
            );
        }

        Ok(embed_resp.embeddings)
    }

    fn dimensions(&self) -> usize {
        self.spec.dimensions
    }

    fn max_tokens(&self) -> usize {
        self.spec.max_tokens
    }

    fn model_id(&self) -> String {
        format!("ollama/{}", self.spec.model_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_spec_constants() {
        assert_eq!(NOMIC_EMBED_TEXT.dimensions, 768);
        assert_eq!(NOMIC_EMBED_TEXT.max_tokens, 8192);
        assert_eq!(NOMIC_EMBED_TEXT.model_name, "nomic-embed-text");

        assert_eq!(MXBAI_EMBED_LARGE.dimensions, 1024);
        assert_eq!(MXBAI_EMBED_LARGE.max_tokens, 512);
        assert_eq!(MXBAI_EMBED_LARGE.model_name, "mxbai-embed-large");

        assert_eq!(ALL_MINILM.dimensions, 384);
        assert_eq!(ALL_MINILM.max_tokens, 256);
        assert_eq!(ALL_MINILM.model_name, "all-minilm");
    }

    #[test]
    fn test_provider_creation() {
        let provider = OllamaProvider::new(NOMIC_EMBED_TEXT);
        assert!(provider.is_ok());
    }

    #[test]
    fn test_try_new_succeeds() {
        let provider = OllamaProvider::try_new(NOMIC_EMBED_TEXT);
        assert!(provider.is_some());
    }

    #[test]
    fn test_base_url_is_string() {
        // Provider should have a non-empty base_url regardless of env
        let provider = OllamaProvider::new(ALL_MINILM).unwrap();
        assert!(!provider.base_url.is_empty());
        assert!(provider.base_url.starts_with("http"));
    }

    #[test]
    fn test_dimensions_and_max_tokens() {
        let provider = OllamaProvider::new(NOMIC_EMBED_TEXT).unwrap();
        assert_eq!(provider.dimensions(), 768);
        assert_eq!(provider.max_tokens(), 8192);
    }

    #[test]
    fn test_installation_hint() {
        let provider = OllamaProvider::new(NOMIC_EMBED_TEXT).unwrap();
        let hint = provider.installation_hint();
        assert!(hint.contains("ollama pull nomic-embed-text"));
        assert!(hint.contains("https://ollama.ai"));
    }

    /// Helper: returns true if Ollama is reachable and the given model is pulled.
    async fn ollama_available(spec: OllamaModelSpec) -> bool {
        let provider = match OllamaProvider::new(spec) {
            Ok(p) => p,
            Err(_) => return false,
        };
        provider.check_model_available().await.unwrap_or(false)
    }

    #[tokio::test]
    async fn test_embed_single_ollama() {
        if !ollama_available(ALL_MINILM).await {
            eprintln!("Skipping: Ollama not available with all-minilm");
            return;
        }
        let provider = OllamaProvider::new(ALL_MINILM).unwrap();
        let result = provider.embed("Hello, world!").await;
        assert!(
            result.is_ok(),
            "Embedding should succeed: {:?}",
            result.err()
        );
        let embedding = result.unwrap();
        assert_eq!(embedding.len(), 384);
        assert!(embedding.iter().any(|&x| x != 0.0));
    }

    #[tokio::test]
    async fn test_embed_batch_ollama() {
        if !ollama_available(ALL_MINILM).await {
            eprintln!("Skipping: Ollama not available with all-minilm");
            return;
        }
        let provider = OllamaProvider::new(ALL_MINILM).unwrap();
        let texts = vec!["First text", "Second text", "Third text"];
        let result = provider.embed_batch(&texts).await;
        assert!(result.is_ok(), "Batch should succeed: {:?}", result.err());
        let embeddings = result.unwrap();
        assert_eq!(embeddings.len(), 3);
        for emb in &embeddings {
            assert_eq!(emb.len(), 384);
        }
    }

    #[tokio::test]
    async fn test_embed_nomic_embed_text() {
        if !ollama_available(NOMIC_EMBED_TEXT).await {
            eprintln!("Skipping: Ollama not available with nomic-embed-text");
            return;
        }
        let provider = OllamaProvider::new(NOMIC_EMBED_TEXT).unwrap();
        let result = provider.embed("Hello, world!").await;
        assert!(
            result.is_ok(),
            "Embedding should succeed: {:?}",
            result.err()
        );
        let embedding = result.unwrap();
        assert_eq!(embedding.len(), 768);
        assert!(embedding.iter().any(|&x| x != 0.0));
    }

    #[tokio::test]
    async fn test_check_model_available_nomic() {
        let provider = match OllamaProvider::new(NOMIC_EMBED_TEXT) {
            Ok(p) => p,
            Err(_) => return,
        };
        // If Ollama is reachable, check_model_available should succeed (not error)
        if let Ok(available) = provider.check_model_available().await {
            // nomic-embed-text should be pulled if Ollama is reachable
            assert!(available, "nomic-embed-text should be available");
        }
    }

    #[tokio::test]
    async fn test_check_model_available_unreachable() {
        // Point at a port nothing is listening on
        let provider = OllamaProvider {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            base_url: "http://127.0.0.1:19999".to_string(),
            spec: ALL_MINILM,
        };
        let result = provider.check_model_available().await;
        assert!(result.is_err());
    }
}
