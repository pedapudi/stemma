//! stemma-embed: the embedder seam.
//!
//! Backends implement [`Embedder`]; the pipeline programs against the trait
//! and treats the embedder as fallible and optional — when it is absent or
//! down, resolution degrades to the lexical channels instead of failing.
//!
//! The first backend speaks the OpenAI-compatible `/v1/embeddings` protocol,
//! which covers vLLM, llama.cpp (`--embeddings`), LiteLLM proxies and hosted
//! compatibility endpoints with one implementation. The model identity a
//! backend reports is what lands in the store's `model_registry`; vectors
//! from different identities are never mixed in one table.

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("embedding endpoint: {0}")]
    Http(String),
    #[error("embedding endpoint returned {got} vectors for {want} inputs")]
    CountMismatch { got: usize, want: usize },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Stable identity for the model registry.
#[derive(Debug, Clone)]
pub struct ModelIdentity {
    pub backend: String,
    pub model: String,
    pub dimension: usize,
}

pub trait Embedder: Send + Sync {
    /// Embed a batch of texts, order-preserving.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn identity(&self) -> ModelIdentity;
}

/// Qwen3-Embedding-style retrieval formatting: queries carry an instruction,
/// documents are embedded raw. Mentions are queries.
pub fn format_query(mention: &str) -> String {
    format!(
        "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: {mention}"
    )
}

/// An OpenAI-compatible `/v1/embeddings` client.
pub struct OpenAiEmbedder {
    endpoint: String,
    model: String,
    dimension: std::sync::OnceLock<usize>,
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingRow>,
}

#[derive(Deserialize)]
struct EmbeddingRow {
    index: usize,
    embedding: Vec<f32>,
}

impl OpenAiEmbedder {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model: model.to_string(),
            dimension: std::sync::OnceLock::new(),
        }
    }
}

impl Embedder for OpenAiEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let resp: EmbeddingsResponse = ureq::post(&format!("{}/embeddings", self.endpoint))
            .timeout(std::time::Duration::from_secs(60))
            .send_json(ureq::json!({ "model": self.model, "input": texts }))
            .map_err(|e| Error::Http(e.to_string()))?
            .into_json()
            .map_err(|e| Error::Http(e.to_string()))?;
        if resp.data.len() != texts.len() {
            return Err(Error::CountMismatch {
                got: resp.data.len(),
                want: texts.len(),
            });
        }
        let mut rows = resp.data;
        rows.sort_by_key(|r| r.index);
        if let Some(first) = rows.first() {
            let _ = self.dimension.set(first.embedding.len());
        }
        Ok(rows.into_iter().map(|r| r.embedding).collect())
    }

    fn identity(&self) -> ModelIdentity {
        ModelIdentity {
            backend: "openai-compat".into(),
            model: self.model.clone(),
            dimension: *self.dimension.get().unwrap_or(&0),
        }
    }
}
