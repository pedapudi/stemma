//! stemma-lm: the decoder seam.
//!
//! Backends implement [`LmBackend`]; the pipeline programs against the trait
//! and treats the LM as fallible and optional — when it is absent or down,
//! resolution proceeds unadjudicated instead of failing. The LM is never the
//! retrieval mechanism: it decides among presented options
//! (docs/design/05-encoders-decoders.md), so the trait is a single
//! chat-completion call with an optional JSON schema for the reply.
//!
//! The first backend speaks the OpenAI-compatible `/v1/chat/completions`
//! protocol, which covers vLLM, llama.cpp and hosted compatibility endpoints
//! with one implementation. Structured output is negotiated per call: try the
//! native `response_format: json_schema` (vLLM supports it); if the endpoint
//! rejects it with a 4xx, fall back to embedding the schema in the
//! instructions and validating the reply, with one corrective retry.

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("lm endpoint: {0}")]
    Http(String),
    #[error("lm reply not valid JSON after retry: {0}")]
    Malformed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Stable identity for traces and evidence records.
#[derive(Debug, Clone)]
pub struct LmIdentity {
    pub backend: String,
    pub model: String,
}

/// One chat turn. Roles follow the OpenAI convention:
/// "system" | "user" | "assistant".
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
}

pub trait LmBackend: Send + Sync {
    /// One chat-completion round trip; returns the assistant's content.
    /// With `schema` set, the content is a JSON document conforming to it
    /// (enforced natively where the backend can, by validate-and-retry where
    /// it cannot).
    fn chat(&self, messages: &[ChatMessage], schema: Option<&serde_json::Value>)
        -> Result<String>;
    /// Whether the backend enforces `schema` natively (constrained decoding)
    /// rather than by instruction and validation.
    fn native_structured_output(&self) -> bool;
    fn identity(&self) -> LmIdentity;
}

/// Map a configured `(endpoint, model)` to a backend. Every model string
/// currently routes to the OpenAI-compatible backend; a model family needing
/// a different protocol earns a new arm here, not a new abstraction.
///
/// `extra_body` is merged verbatim into every request body — the escape
/// hatch for backend-specific serving knobs the config must control, e.g.
/// vLLM's `chat_template_kwargs {"enable_thinking": false}`: adjudication
/// is constrained select-among-k, and reasoning tokens spent before a
/// forced-choice JSON answer are pure latency (measured 24s median per
/// adjudicated resolve with thinking on).
pub fn backend_for(
    endpoint: &str,
    model: &str,
    extra_body: Option<serde_json::Value>,
) -> Box<dyn LmBackend> {
    Box::new(OpenAiChat::new(endpoint, model, extra_body))
}

/// An OpenAI-compatible `/v1/chat/completions` client. Deterministic by
/// construction: temperature 0 on every request.
pub struct OpenAiChat {
    endpoint: String,
    model: String,
    extra_body: Option<serde_json::Value>,
    /// Cleared the first time the endpoint 4xx-rejects `response_format`;
    /// later schema calls then go straight to the instruction fallback.
    native_schema: std::sync::atomic::AtomicBool,
}

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

impl OpenAiChat {
    pub fn new(endpoint: &str, model: &str, extra_body: Option<serde_json::Value>) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model: model.to_string(),
            extra_body,
            native_schema: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn request(&self, messages: &[serde_json::Value], response_format: Option<serde_json::Value>)
        -> std::result::Result<String, ureq::Error>
    {
        let mut body = ureq::json!({
            "model": self.model,
            "temperature": 0,
            "messages": messages,
        });
        if let Some(rf) = response_format {
            body["response_format"] = rf;
        }
        if let Some(serde_json::Value::Object(extra)) = &self.extra_body {
            for (k, v) in extra {
                body[k] = v.clone();
            }
        }
        let resp: ChatResponse = ureq::post(&format!("{}/chat/completions", self.endpoint))
            .timeout(TIMEOUT)
            .send_json(body)?
            .into_json()?;
        Ok(resp
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default())
    }

    /// Instruction-embedded fallback: state the schema, parse the reply, and
    /// on a parse failure send the broken reply back once for correction.
    fn chat_instructed(&self, messages: &[serde_json::Value], schema: &serde_json::Value)
        -> Result<String>
    {
        let mut messages = messages.to_vec();
        messages.push(ureq::json!({
            "role": "user",
            "content": format!(
                "Reply with a single JSON object conforming to this JSON schema and nothing else:\n{schema}"
            ),
        }));
        let reply = self.request(&messages, None).map_err(|e| Error::Http(e.to_string()))?;
        if let Some(json) = extract_json(&reply) {
            return Ok(json);
        }
        messages.push(ureq::json!({ "role": "assistant", "content": reply.clone() }));
        messages.push(ureq::json!({
            "role": "user",
            "content": "That was not valid JSON. Reply with only the JSON object.",
        }));
        let retry = self.request(&messages, None).map_err(|e| Error::Http(e.to_string()))?;
        extract_json(&retry).ok_or(Error::Malformed(retry))
    }
}

/// Pull the first JSON document out of a reply that may wrap it in code
/// fences or prose; returns it re-serialized iff it parses.
fn extract_json(reply: &str) -> Option<String> {
    let start = reply.find(['{', '['])?;
    let text = &reply[start..];
    let mut de = serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>();
    match de.next() {
        Some(Ok(v)) => Some(v.to_string()),
        _ => None,
    }
}

impl LmBackend for OpenAiChat {
    fn chat(&self, messages: &[ChatMessage], schema: Option<&serde_json::Value>)
        -> Result<String>
    {
        use std::sync::atomic::Ordering;
        let messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| ureq::json!({ "role": m.role, "content": m.content }))
            .collect();
        let Some(schema) = schema else {
            return self.request(&messages, None).map_err(|e| Error::Http(e.to_string()));
        };
        if self.native_schema.load(Ordering::Relaxed) {
            let rf = ureq::json!({
                "type": "json_schema",
                "json_schema": { "name": "reply", "strict": true, "schema": schema },
            });
            match self.request(&messages, Some(rf)) {
                Ok(content) => return Ok(content),
                // A 4xx means the endpoint does not take response_format;
                // remember that and fall back. 5xx/transport errors are the
                // endpoint being down, not a capability signal.
                Err(ureq::Error::Status(code, _)) if (400..500).contains(&code) => {
                    self.native_schema.store(false, Ordering::Relaxed);
                }
                Err(e) => return Err(Error::Http(e.to_string())),
            }
        }
        self.chat_instructed(&messages, schema)
    }

    fn native_structured_output(&self) -> bool {
        self.native_schema.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn identity(&self) -> LmIdentity {
        LmIdentity {
            backend: "openai-compat".into(),
            model: self.model.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_handles_fences_and_prose() {
        assert_eq!(
            extract_json("```json\n{\"choice\": \"1\"}\n```").as_deref(),
            Some("{\"choice\":\"1\"}")
        );
        assert_eq!(
            extract_json("Sure: {\"choice\": \"nil\"} — done").as_deref(),
            Some("{\"choice\":\"nil\"}")
        );
        assert_eq!(extract_json("no json here"), None);
        assert_eq!(extract_json("{broken"), None);
    }

    #[test]
    fn registry_routes_to_openai_compat() {
        let lm = backend_for("http://example.invalid/v1/", "m", None);
        let id = lm.identity();
        assert_eq!(id.backend, "openai-compat");
        assert_eq!(id.model, "m");
        assert!(lm.native_structured_output());
    }
}
