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
    /// The endpoint is alive and refused this request (4xx). Distinct from
    /// [`Error::Http`] because the two demand opposite reactions: transport
    /// failures and 5xx mean the endpoint is unhealthy and callers should
    /// back off, a rejection means this payload is bad and retrying it —
    /// or marking the endpoint down over it — is wrong.
    #[error("embedding endpoint rejected the request ({status}): {detail}")]
    Rejected { status: u16, detail: String },
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

/// Bare queries have two spellings: the empty template and the explicit
/// identity template `"{query}"`. Identity comparisons must treat them as
/// one convention — they produce byte-identical query text.
pub fn is_bare_query_template(template: &str) -> bool {
    template.is_empty() || template == "{query}"
}

/// True when two template spellings denote the same query-side convention.
pub fn query_templates_agree(a: &str, b: &str) -> bool {
    a == b || (is_bare_query_template(a) && is_bare_query_template(b))
}

/// A configured query template disagreeing with the one the store's
/// registry recorded for the vector space. Appending queries of one
/// convention into another convention's space is the same corruption as
/// mixing models (a fine-tuned checkpoint served under a name the family
/// lookup misses measured paraphrase recall@5 halved, 0.18 → 0.08, when
/// queries went out bare against templated anchors), so the disagreement
/// is an error, never a preference.
#[derive(Debug, thiserror::Error)]
#[error(
    "registry stores query template {registered:?} but {configured:?} is \
     configured; endpoint, model and template must agree with the space \
     they query — refusing, like a model mismatch"
)]
pub struct QueryTemplateMismatch {
    pub registered: String,
    pub configured: String,
}

/// Picks the query template for a vector space, in identity order:
///
/// 1. the registry's stored template (`registered`) is the space's recorded
///    convention and wins whenever present — `''`/`None` means the row
///    predates the column and recorded nothing;
/// 2. an explicitly `configured` template must AGREE with a stored one
///    (see [`QueryTemplateMismatch`]) and wins only when nothing is stored;
/// 3. the model-family default ([`default_query_template`]) is the fallback
///    for spaces that never recorded a convention — a name-based guess,
///    which is exactly why the registry outranks it.
pub fn resolve_query_template(
    configured: Option<&str>,
    registered: Option<&str>,
    model: &str,
) -> std::result::Result<Option<String>, QueryTemplateMismatch> {
    let registered = registered.filter(|t| !t.is_empty());
    match (configured, registered) {
        (Some(c), Some(r)) if !query_templates_agree(c, r) => Err(QueryTemplateMismatch {
            registered: r.to_string(),
            configured: c.to_string(),
        }),
        (_, Some(r)) => Ok(Some(r.to_string())),
        (Some(c), None) => Ok(Some(c.to_string())),
        (None, None) => Ok(default_query_template(model)),
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
        // Read timeout sized for the worst-case drain chunk — 256 documents
        // at the model cap is ~8M tokens, ~40s on a saturated 3-replica
        // endpoint — not for the interactive case, which stays snappy via
        // the connect timeout and the cooldown wrapper.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(180))
            .build();
        // `truncate_prompt_tokens: -1` asks the server to clip each input at
        // its own model cap instead of rejecting the whole request when one
        // document exceeds it (vLLM semantics; endpoints without the
        // extension ignore the field). Embedding a legal opinion's first 32k
        // tokens is a bounded approximation; failing 255 neighbours over one
        // oversized document is an exclusion.
        let resp = match agent
            .post(&format!("{}/embeddings", self.endpoint))
            .timeout(std::time::Duration::from_secs(180))
            .send_json(ureq::json!({
                "model": self.model,
                "input": texts,
                "truncate_prompt_tokens": -1
            })) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) if (400..500).contains(&code) => {
                let detail: String = r
                    .into_string()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                return Err(Error::Rejected {
                    status: code,
                    detail,
                });
            }
            Err(e) => return Err(Error::Http(e.to_string())),
        };
        let resp: EmbeddingsResponse = resp.into_json().map_err(|e| Error::Http(e.to_string()))?;
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

    #[test]
    fn bare_spellings_are_one_convention() {
        assert!(query_templates_agree("", "{query}"));
        assert!(query_templates_agree("{query}", ""));
        assert!(query_templates_agree(
            QWEN3_QUERY_TEMPLATE,
            QWEN3_QUERY_TEMPLATE
        ));
        assert!(!query_templates_agree("", QWEN3_QUERY_TEMPLATE));
        assert!(!query_templates_agree("{query}", QWEN3_QUERY_TEMPLATE));
    }

    #[test]
    fn registry_template_outranks_the_family_guess() {
        // A fine-tuned checkpoint whose serving name misses the family
        // substring: bare by name-lookup, but the registry recorded the
        // convention its vectors were staged under — the registry wins.
        assert_eq!(
            resolve_query_template(None, Some(QWEN3_QUERY_TEMPLATE), "qwen3-emb-legal-v3")
                .unwrap()
                .as_deref(),
            Some(QWEN3_QUERY_TEMPLATE)
        );
        // The inverse: a registry that recorded explicit bare beats the
        // family template the model name would have guessed.
        assert_eq!(
            resolve_query_template(None, Some("{query}"), "Qwen3-Embedding-0.6B")
                .unwrap()
                .as_deref(),
            Some("{query}")
        );
    }

    #[test]
    fn family_fallback_only_covers_an_unrecorded_registry() {
        // '' and absent both mean "the row predates the column".
        for registered in [None, Some("")] {
            assert_eq!(
                resolve_query_template(None, registered, "Qwen3-Embedding-0.6B")
                    .unwrap()
                    .as_deref(),
                Some(QWEN3_QUERY_TEMPLATE)
            );
            assert_eq!(
                resolve_query_template(None, registered, "qwen3-emb-legal-v3").unwrap(),
                None
            );
        }
        // Configured wins when nothing is stored.
        assert_eq!(
            resolve_query_template(Some("Doc: {query}"), Some(""), "bge-m3")
                .unwrap()
                .as_deref(),
            Some("Doc: {query}")
        );
    }

    #[test]
    fn configured_template_disagreeing_with_registry_is_an_error() {
        let err = resolve_query_template(
            Some("{query}"),
            Some(QWEN3_QUERY_TEMPLATE),
            "Qwen3-Embedding-0.6B",
        )
        .unwrap_err();
        assert_eq!(err.registered, QWEN3_QUERY_TEMPLATE);
        assert_eq!(err.configured, "{query}");
        // Agreement across bare spellings is not a mismatch.
        assert_eq!(
            resolve_query_template(Some("{query}"), Some("{query}"), "bge-m3")
                .unwrap()
                .as_deref(),
            Some("{query}")
        );
    }
}

/// A down endpoint must cost one failed probe per cooldown window, not one
/// connect/DNS timeout per query: degradation is the designed behaviour,
/// and it has to be cheap to be usable. Wraps any [`Embedder`]; after a
/// failure, calls short-circuit with the last error until the window
/// elapses, then one live probe is allowed through.
pub struct CooldownEmbedder<E> {
    inner: E,
    /// Millis since UNIX epoch of the last failure; 0 = healthy.
    failed_at: std::sync::atomic::AtomicU64,
    cooldown: std::time::Duration,
}

impl<E> CooldownEmbedder<E> {
    pub fn new(inner: E, cooldown: std::time::Duration) -> Self {
        Self {
            inner,
            failed_at: 0.into(),
            cooldown,
        }
    }
    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

impl<E> CooldownEmbedder<E> {
    /// True while the endpoint is marked down.
    pub fn is_down(&self) -> bool {
        self.failed_at.load(std::sync::atomic::Ordering::Relaxed) != 0
    }
}

impl<E: Embedder> CooldownEmbedder<E> {
    /// One live attempt that BYPASSES the short-circuit — the recovery
    /// probe. Query paths never call this; a background owner does, so no
    /// user query ever pays a DNS or connect wait on a dead endpoint
    /// (getaddrinfo is not bounded by connect timeouts). Success clears
    /// the down marker; failure re-stamps it.
    pub fn probe(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        match self.inner.embed(&["probe".to_string()]) {
            Ok(_) => {
                self.failed_at.store(0, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => {
                self.failed_at.store(Self::now_ms(), Ordering::Relaxed);
                Err(e)
            }
        }
    }
}

impl<E: Embedder> Embedder for CooldownEmbedder<E> {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        use std::sync::atomic::Ordering;
        // Marked down: short-circuit unconditionally; recovery belongs to
        // the background probe, never to a user query. The cooldown window
        // only bounds how often the first failure can be rediscovered when
        // no prober is attached.
        let failed = self.failed_at.load(Ordering::Relaxed);
        if failed != 0 {
            if Self::now_ms().saturating_sub(failed) < self.cooldown.as_millis() as u64 {
                return Err(Error::Http("endpoint marked down".into()));
            }
            return match self.probe() {
                Ok(()) => self.inner.embed(texts),
                Err(e) => Err(e),
            };
        }
        match self.inner.embed(texts) {
            Ok(v) => Ok(v),
            // A rejection (4xx) is the endpoint working: the payload is bad,
            // the endpoint is not. Marking down over it would let one
            // malformed request short-circuit every healthy sibling and cost
            // a probe window for nothing.
            Err(e @ Error::Rejected { .. }) => Err(e),
            Err(e) => {
                self.failed_at.store(Self::now_ms(), Ordering::Relaxed);
                Err(e)
            }
        }
    }
    fn identity(&self) -> ModelIdentity {
        self.inner.identity()
    }
}
