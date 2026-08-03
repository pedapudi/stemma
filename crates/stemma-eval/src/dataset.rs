//! The evaluation dataset format: one JSONL file per corpus, one JSON object
//! per line. Files live under eval/datasets/ and are versioned like code —
//! regenerating a frozen set is a reviewed change.
//!
//! The format is shared between the BIRD deriver (this crate) and the
//! synthetic legal generator, so it tolerates both producers:
//! - a line whose object carries `"type": "header"` (or no `"question"` key)
//!   is metadata and is skipped by the loader;
//! - `id` and `corpus` are optional on question lines — absent, they are
//!   synthesized from the file name and line number;
//! - `match_mode` names both producers' semantics: `exact` / `value`
//!   (normalized equality against a stored value), `like` (gold predicate was
//!   a LIKE; wildcards preserved in `literal`, stripped before matching), and
//!   `doc` (the gold row is a document the mention resolves *into*;
//!   containment semantics, `literal` optional).

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// A value the resolver must link: the gold `(table, column, rows)` behind
/// one predicate of the gold SQL (BIRD) or one sampled record (synthetic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub table: String,
    pub column: String,
    /// The surface literal from the gold predicate (wildcards preserved for
    /// `like`). Optional for `doc` targets, where the row itself is gold.
    #[serde(default)]
    pub literal: String,
    /// "exact" | "value" | "like" | "doc" — see module docs.
    pub match_mode: String,
    /// Denotation-verified gold rowids: the rows the gold predicate selects
    /// in the actual database instance. Capped at [`MAX_GOLD_ROWIDS`];
    /// `rowids_truncated` marks a capped set, and scoring falls back to
    /// probing the user database for membership.
    pub rowids: Vec<i64>,
    #[serde(default)]
    pub rowids_truncated: bool,
}

pub const MAX_GOLD_ROWIDS: usize = 256;

/// One evaluation question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalQuestion {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub corpus: String,
    pub question: String,
    /// "L1" | "L2" | "L3" | "L4" | "NIL"
    pub tier: String,
    #[serde(default)]
    pub nil: bool,
    #[serde(default)]
    pub targets: Vec<Target>,
    /// Producer bookkeeping: source, gold SQL, BIRD evidence (reference only,
    /// never shown to the resolver), verification notes.
    #[serde(default)]
    pub provenance: serde_json::Value,
}

pub const TIERS: [&str; 5] = ["L1", "L2", "L3", "L4", "NIL"];

/// Loads a dataset file, skipping header/metadata lines.
pub fn load(path: &Path) -> anyhow::Result<Vec<EvalQuestion>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading dataset {}", path.display()))?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "dataset".into());
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("{}:{}: invalid JSON", path.display(), lineno + 1))?;
        if value.get("type").and_then(|t| t.as_str()) == Some("header")
            || value.get("question").is_none()
        {
            continue; // metadata line
        }
        let mut q: EvalQuestion = serde_json::from_value(value)
            .with_context(|| format!("{}:{}: bad question record", path.display(), lineno + 1))?;
        if q.id.is_empty() {
            q.id = format!("{stem}#{}", lineno + 1);
        }
        if q.corpus.is_empty() {
            q.corpus = stem.clone();
        }
        if !TIERS.contains(&q.tier.as_str()) {
            anyhow::bail!(
                "{}:{}: unknown tier {:?}",
                path.display(),
                lineno + 1,
                q.tier
            );
        }
        out.push(q);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_skips_headers_and_fills_defaults() {
        let dir = std::env::temp_dir().join(format!("stemma-eval-ds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo-corpus.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"header\",\"dataset\":\"demo\",\"version\":1}\n",
                "{\"question\":\"q one\",\"tier\":\"L1\",\"targets\":[{\"table\":\"t\",\"column\":\"c\",\"literal\":\"x\",\"match_mode\":\"exact\",\"rowids\":[1]}]}\n",
                "{\"question\":\"absent\",\"tier\":\"NIL\",\"nil\":true,\"targets\":[]}\n",
            ),
        )
        .unwrap();
        let qs = load(&path).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].id, "demo-corpus#2");
        assert_eq!(qs[0].corpus, "demo-corpus");
        assert_eq!(qs[0].targets[0].rowids, vec![1]);
        assert!(qs[1].nil);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loader_rejects_unknown_tier() {
        let dir = std::env::temp_dir().join(format!("stemma-eval-ds2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.jsonl");
        std::fs::write(&path, "{\"question\":\"q\",\"tier\":\"L9\"}\n").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
