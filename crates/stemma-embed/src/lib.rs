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

/// Stable identity for the model registry. The query template is part of the
/// identity, not a global convention: endpoint, model and template have to
/// agree with each other, and a template that matches one model family is
/// noise prepended to every query on another (issue #5 measured unrelated
/// queries collapsing to 0.82 mean pairwise cosine under a foreign prefix).
#[derive(Debug, Clone)]
pub struct ModelIdentity {
    pub backend: String,
    pub model: String,
    pub dimension: usize,
    /// Query-side template with a `{query}` placeholder; empty = queries are
    /// embedded bare. Recorded in the store's `model_registry` beside the
    /// model name.
    pub query_template: String,
}

pub trait Embedder: Send + Sync {
    /// Embed a batch of texts, order-preserving.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn identity(&self) -> ModelIdentity;

    /// Renders the query-side text of the asymmetric retrieval scheme:
    /// mentions and full queries pass through the backend's template,
    /// documents and cards are embedded raw. Derived from
    /// [`ModelIdentity::query_template`] so the template travels with the
    /// model identity rather than living as a free function.
    fn format_query(&self, mention: &str) -> String {
        let template = self.identity().query_template;
        if template.is_empty() {
            mention.to_string()
        } else {
            template.replace("{query}", mention)
        }
    }
}

/// The Qwen3-Embedding family's retrieval instruction, as published with the
/// models: an instruction on the query side, documents raw.
pub const QWEN3_QUERY_TEMPLATE: &str =
    "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: {query}";

/// Default query template by model family, for deployments that configure a
/// model but no template. Qwen3-Embedding models get [`QWEN3_QUERY_TEMPLATE`]
/// (they were trained with it); any other model gets bare queries, because a
/// convention the model was not trained for is ~20 tokens of constant text
/// averaged into every query vector. A deployment overrides either way with
/// an explicit template (`"{query}"` for forced-bare).
pub fn default_query_template(model: &str) -> Option<String> {
    if model.to_lowercase().contains("qwen3-embedding") {
        Some(QWEN3_QUERY_TEMPLATE.to_string())
    } else {
        None
    }
}

/// An OpenAI-compatible `/v1/embeddings` client.
pub struct OpenAiEmbedder {
    endpoint: String,
    model: String,
    query_template: Option<String>,
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
    /// `query_template` carries the model's query-side convention (`{query}`
    /// placeholder); `None` embeds queries bare. Callers that only know the
    /// model name can consult [`default_query_template`].
    pub fn new(endpoint: &str, model: &str, query_template: Option<String>) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model: model.to_string(),
            query_template,
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
            query_template: self.query_template.clone().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templated_backend_formats_queries_through_its_template() {
        let e = OpenAiEmbedder::new(
            "http://example.invalid/v1",
            "Qwen3-Embedding-0.6B",
            Some(QWEN3_QUERY_TEMPLATE.to_string()),
        );
        let out = e.format_query("denim trousers");
        assert!(out.starts_with("Instruct: Given a search query"));
        assert!(out.ends_with("Query: denim trousers"));
        assert_eq!(e.identity().query_template, QWEN3_QUERY_TEMPLATE);
    }

    #[test]
    fn bare_backend_leaves_queries_untouched() {
        let e = OpenAiEmbedder::new("http://example.invalid/v1", "some-encoder", None);
        assert_eq!(e.format_query("denim trousers"), "denim trousers");
        assert_eq!(e.identity().query_template, "");
        // An explicit "{query}" template is the spelled-out form of bare.
        let e = OpenAiEmbedder::new(
            "http://example.invalid/v1",
            "Qwen3-Embedding-0.6B",
            Some("{query}".to_string()),
        );
        assert_eq!(e.format_query("denim trousers"), "denim trousers");
    }

    #[test]
    fn default_template_is_looked_up_by_model_family() {
        assert_eq!(
            default_query_template("Qwen3-Embedding-0.6B").as_deref(),
            Some(QWEN3_QUERY_TEMPLATE)
        );
        assert_eq!(
            default_query_template("qwen3-embedding-8b").as_deref(),
            Some(QWEN3_QUERY_TEMPLATE)
        );
        assert_eq!(default_query_template("text-embedding-3-small"), None);
        assert_eq!(default_query_template("bge-m3"), None);
    }
}
