//! stemma-resolve: the resolution pipeline.
//!
//! Current stage of the roadmap: the lexical cascade (milestone 2) — span
//! enumeration, exact + BM25 + trigram candidate generation over the lexical
//! index built by stemma-ingest, reciprocal-rank fusion, and greedy
//! non-overlapping mention selection.
//!
//! Every resolution produces a full [`Trace`]: not just what was selected but
//! everything that was considered and why it lost — near-miss candidates,
//! rejected spans, per-channel scores. The trace is served over the Explain
//! RPC and drives the UI's query-plan trajectory; honesty here is a design
//! requirement, not a debugging convenience.

use serde::Serialize;
use stemmadb::StemmaDb;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] stemmadb::rusqlite::Error),
    #[error("lexical index missing — run ingest first")]
    IndexMissing,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Spans shorter than this (in chars) are not looked up: the trigram index
/// needs 3+ chars, and 1–2 char mentions are noise.
const MIN_SPAN_CHARS: usize = 3;
/// Longest mention considered, in tokens.
const MAX_SPAN_TOKENS: usize = 4;
/// Candidates fetched per channel per span.
const PER_CHANNEL_LIMIT: usize = 8;
/// Fused score below which a candidate is kept in the trace but not selected.
const SELECT_THRESHOLD: f64 = 0.35;
/// Max selected candidates per mention.
const TOP_K: usize = 5;

/// Stopwords: never a mention on their own (still allowed inside longer spans).
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "at", "by", "did", "do", "does", "for", "from", "how", "in", "is",
    "it", "of", "on", "or", "s", "that", "the", "to", "was", "were", "what", "when", "where",
    "which", "who", "with",
];

#[derive(Debug, Clone, Serialize)]
pub struct Token {
    pub text: String,
    /// Byte offsets into the query, end-exclusive.
    pub start: usize,
    pub end: usize,
    pub stopword: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelScore {
    /// "exact" | "bm25" | "trigram"
    pub channel: String,
    /// Rank within the channel's results for this span (0 = best).
    pub rank: usize,
    /// Channel-native score (1.0 for exact; SQLite bm25() value negated for
    /// the FTS channels, larger = better).
    pub raw: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub table: String,
    pub column: String,
    pub rowid: i64,
    /// Stored value, truncated for transport (full value stays in the DB).
    pub value: String,
    pub value_truncated: bool,
    /// Fused score in [0, 1].
    pub score: f64,
    pub channels: Vec<ChannelScore>,
    pub selected: bool,
    /// Why an unselected candidate lost: "below_threshold" | "outranked" |
    /// "span_not_selected".
    pub reject_reason: Option<String>,
    /// True when the stored value is a document the mention resolves *into*
    /// (BM25/snippet semantics) rather than a value it equals.
    pub is_doc: bool,
    /// FTS snippet with ⟨⟩ marking hit terms — document candidates only.
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Span {
    pub id: usize,
    pub text: String,
    pub start: usize,
    pub end: usize,
    /// "selected" — became a mention; "overlapped" — lost to an overlapping
    /// span; "no_candidates" — nothing matched; "weak" — best candidate under
    /// threshold; "skipped" — stopword-only or too short.
    pub status: String,
    pub candidates: Vec<Candidate>,
    /// The span matches a knowledge-graph phrase/term entity: the KG
    /// participated in mention detection, and selection favors this span.
    pub kg_alias: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Trace {
    pub query: String,
    pub tokens: Vec<Token>,
    pub spans: Vec<Span>,
    /// Ids (into `spans`) of the spans selected as mentions, in query order.
    pub mentions: Vec<usize>,
    pub elapsed_ms: f64,
}

/// Resolve `query` against the lexical index, returning the full trace.
pub fn resolve_lexical(db: &StemmaDb, query: &str) -> Result<Trace> {
    let started = std::time::Instant::now();
    let conn = db.conn();

    let indexed: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'lex_values'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if indexed == 0 {
        return Err(Error::IndexMissing);
    }

    let tokens = tokenize(query);
    let mut spans = enumerate_spans(query, &tokens);

    // KG-assisted mention detection: spans matching a compiled phrase/term
    // entity are marked and favored in selection — multi-word entities like
    // "coastal development permit" beat their fragments.
    {
        let mut stmt = conn.prepare(
            "SELECT count(*) FROM sqlite_master WHERE name = 'kg_nodes'",
        )?;
        let has_kg: i64 = stmt.query_row([], |r| r.get(0))?;
        if has_kg > 0 {
            let mut q = conn.prepare_cached(
                "SELECT count(*) FROM kg_nodes WHERE kind = 'term' AND lower(label) = ?1",
            )?;
            for span in spans.iter_mut() {
                if span.status == "skipped" {
                    continue;
                }
                let hit: i64 = q.query_row([span.text.to_lowercase()], |r| r.get(0))?;
                span.kg_alias = hit > 0;
            }
        }
    }

    for span in spans.iter_mut() {
        if span.status == "skipped" {
            continue;
        }
        span.candidates = gather_candidates(db, &span.text)?;
        if span.candidates.is_empty() {
            span.status = "no_candidates".into();
        } else if span.candidates[0].score < SELECT_THRESHOLD {
            span.status = "weak".into();
        }
    }

    let mentions = select_mentions(&mut spans);

    Ok(Trace {
        query: query.to_string(),
        tokens,
        spans,
        mentions,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

fn tokenize(query: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (i, ch) in query.char_indices() {
        if ch.is_alphanumeric() {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            tokens.push(make_token(query, s, i));
        }
    }
    if let Some(s) = start {
        tokens.push(make_token(query, s, query.len()));
    }
    tokens
}

fn make_token(query: &str, start: usize, end: usize) -> Token {
    let text = query[start..end].to_string();
    let stopword = STOPWORDS.contains(&text.to_lowercase().as_str());
    Token {
        text,
        start,
        end,
        stopword,
    }
}

/// All n-grams up to MAX_SPAN_TOKENS. Spans that are stopword-only or too
/// short are kept in the trace as "skipped" so the UI can show them greyed.
fn enumerate_spans(query: &str, tokens: &[Token]) -> Vec<Span> {
    let mut spans = Vec::new();
    for i in 0..tokens.len() {
        for n in 1..=MAX_SPAN_TOKENS.min(tokens.len() - i) {
            let start = tokens[i].start;
            let end = tokens[i + n - 1].end;
            let text = query[start..end].to_string();
            let all_stop = tokens[i..i + n].iter().all(|t| t.stopword);
            let status = if all_stop || text.chars().count() < MIN_SPAN_CHARS {
                "skipped"
            } else {
                "selected" // provisional; refined after candidate gathering
            };
            spans.push(Span {
                id: spans.len(),
                text,
                start,
                end,
                status: status.into(),
                candidates: Vec::new(),
                kg_alias: false,
            });
        }
    }
    spans
}

struct RawHit {
    table: String,
    column: String,
    rowid: i64,
    value: String,
    channel: &'static str,
    rank: usize,
    raw: f64,
    is_doc: bool,
    snippet: Option<String>,
}

fn gather_candidates(db: &StemmaDb, span: &str) -> Result<Vec<Candidate>> {
    let conn = db.conn();
    let mut hits: Vec<RawHit> = Vec::new();

    // Channel 1: exact (case/whitespace-normalized), short values only.
    {
        let mut stmt = conn.prepare_cached(
            "SELECT src_table, src_column, src_rowid, value FROM lex_values
             WHERE value_norm = lower(trim(?1)) AND length(value) <= ?2
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            stemmadb::rusqlite::params![
                span,
                stemma_ingest::EXACT_MAX_LEN as i64,
                PER_CHANNEL_LIMIT as i64
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )?;
        for (rank, row) in rows.enumerate() {
            let (table, column, rowid, value) = row?;
            hits.push(RawHit {
                table,
                column,
                rowid,
                value,
                channel: "exact",
                rank,
                raw: 1.0,
                is_doc: false,
                snippet: None,
            });
        }
    }

    // Channels 2 & 3: BM25 token search and trigram fuzzy/substring search.
    for (channel, fts_table) in [("bm25", "lex_fts"), ("trigram", "lex_trigram")] {
        let sql = format!(
            "SELECT v.src_table, v.src_column, v.src_rowid, v.value, bm25({fts}),
                    v.is_doc, snippet({fts}, 0, '⟨', '⟩', '…', 10)
             FROM {fts} f JOIN lex_values v ON v.id = f.rowid
             WHERE {fts} MATCH ?1 ORDER BY bm25({fts}) LIMIT ?2",
            fts = fts_table
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        // Quote as an FTS5 string so query punctuation isn't FTS syntax.
        let fts_query = format!("\"{}\"", span.replace('"', "\"\""));
        let rows = stmt.query_map(
            stemmadb::rusqlite::params![fts_query, PER_CHANNEL_LIMIT as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        );
        let rows = match rows {
            Ok(rows) => rows,
            // Spans under 3 chars (or odd tokenizations) can make a trigram
            // query legitimately unmatchable — treat as zero hits.
            Err(_) => continue,
        };
        for (rank, row) in rows.enumerate() {
            let (table, column, rowid, value, bm25, is_doc, snippet) = match row {
                Ok(v) => v,
                Err(_) => continue,
            };
            let is_doc = is_doc != 0;
            hits.push(RawHit {
                table,
                column,
                rowid,
                value,
                channel,
                rank,
                raw: -bm25, // SQLite bm25() is lower-is-better; negate.
                is_doc,
                snippet: if is_doc { snippet } else { None },
            });
        }
    }

    let mut candidates = fuse(span, hits);
    apply_kg_coherence(db, span, &mut candidates)?;
    Ok(candidates)
}

/// The GraphRAG-lite assist: when the span's tokens are characteristic terms
/// in the knowledge graph, document candidates that also contain the terms'
/// co-occurring neighbors earn a small, evidence-carrying bonus. Appears in
/// the trace as the "kg" channel.
fn apply_kg_coherence(db: &StemmaDb, span: &str, candidates: &mut [Candidate]) -> Result<()> {
    if candidates.iter().all(|c| !c.is_doc) {
        return Ok(());
    }
    let conn = db.conn();
    let has_kg: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'kg_edges'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_kg == 0 {
        return Ok(());
    }

    // Co-occurring terms of any span token, strongest first, at most 4.
    let tokens: Vec<String> = span
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; tokens.len()].join(",");
    let sql = format!(
        "SELECT DISTINCT n2.label FROM kg_nodes n1
         JOIN kg_edges e ON e.kind = 'cooccurs' AND (e.src = n1.id OR e.dst = n1.id)
         JOIN kg_nodes n2 ON n2.id = CASE WHEN e.src = n1.id THEN e.dst ELSE e.src END
         WHERE n1.kind = 'term' AND n1.label IN ({placeholders})
         LIMIT 4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let coterms: Vec<String> = stmt
        .query_map(
            stemmadb::rusqlite::params_from_iter(tokens.iter()),
            |r| r.get(0),
        )?
        .collect::<std::result::Result<_, _>>()?;
    let coterms: Vec<&String> = coterms.iter().filter(|c| !tokens.contains(c)).collect();
    if coterms.is_empty() {
        return Ok(());
    }

    for c in candidates.iter_mut().filter(|c| c.is_doc) {
        let mut matched = 0usize;
        for ct in &coterms {
            let hit: i64 = conn
                .query_row(
                    "SELECT count(*) FROM lex_fts
                     WHERE lex_fts MATCH ?1 AND rowid = (
                        SELECT id FROM lex_values
                        WHERE src_table = ?2 AND src_column = ?3 AND src_rowid = ?4)",
                    stemmadb::rusqlite::params![
                        format!("\"{}\"", ct.replace('"', "\"\"")),
                        c.table,
                        c.column,
                        c.rowid
                    ],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if hit > 0 {
                matched += 1;
            }
        }
        if matched > 0 {
            c.score = (c.score + 0.04 * matched as f64).min(0.9);
            c.channels.push(ChannelScore {
                channel: "kg".into(),
                rank: 0,
                raw: matched as f64,
            });
        }
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(())
}

/// Reciprocal-rank fusion across channels, with a length-affinity factor so
/// short stored values that closely match the span outrank long documents
/// that merely contain it.
fn fuse(span: &str, hits: Vec<RawHit>) -> Vec<Candidate> {
    use std::collections::BTreeMap;
    const RRF_K: f64 = 4.0;

    struct Group {
        channels: Vec<ChannelScore>,
        value: String,
        is_doc: bool,
        snippet: Option<String>,
    }
    let mut grouped: BTreeMap<(String, String, i64), Group> = BTreeMap::new();
    for h in hits {
        let entry = grouped
            .entry((h.table.clone(), h.column.clone(), h.rowid))
            .or_insert_with(|| Group {
                channels: Vec::new(),
                value: h.value.clone(),
                is_doc: h.is_doc,
                snippet: None,
            });
        entry.is_doc |= h.is_doc;
        if entry.snippet.is_none() {
            entry.snippet = h.snippet.clone();
        }
        entry.channels.push(ChannelScore {
            channel: h.channel.to_string(),
            rank: h.rank,
            raw: h.raw,
        });
    }

    let span_len = span.chars().count() as f64;
    let mut candidates: Vec<Candidate> = grouped
        .into_iter()
        .map(|((table, column, rowid), g)| {
            let has_exact = g.channels.iter().any(|c| c.channel == "exact");
            let rrf: f64 = g.channels.iter().map(|c| 1.0 / (RRF_K + c.rank as f64)).sum();
            // Normalize: three channels at rank 0 -> 1.0 (docs never have the
            // exact channel, so their base tops out at 2/3).
            let base = (rrf / (3.0 / RRF_K)).min(1.0);
            let score = if has_exact {
                // Exact matches are definitionally right about the value.
                (0.9 + 0.1 * base).min(1.0)
            } else if g.is_doc {
                // A mention resolves *into* a document; punishing the doc for
                // its length would break retrieval (the careg failure mode).
                (base * 0.85).min(0.85)
            } else {
                let affinity =
                    (span_len / (g.value.chars().count() as f64).max(span_len)).sqrt();
                (base * (0.4 + 0.6 * affinity)).min(1.0)
            };
            let (value, value_truncated) = truncate_value(&g.value);
            Candidate {
                table,
                column,
                rowid,
                value,
                value_truncated,
                score,
                channels: g.channels,
                selected: false,
                reject_reason: None,
                is_doc: g.is_doc,
                snippet: g.snippet,
            }
        })
        .collect();

    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    candidates
}

fn truncate_value(v: &str) -> (String, bool) {
    const MAX: usize = 160;
    if v.chars().count() <= MAX {
        (v.to_string(), false)
    } else {
        (v.chars().take(MAX).collect::<String>() + "…", true)
    }
}

/// Greedy non-overlapping selection: strongest span wins its byte range;
/// overlapping spans are marked "overlapped". Within a selected span, top-k
/// candidates above threshold are selected, the rest annotated.
fn select_mentions(spans: &mut [Span]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..spans.len())
        .filter(|&i| spans[i].status == "selected")
        .collect();
    // Strongest candidate first; KG-entity spans get a nudge (a compiled
    // phrase is better evidence of mention-hood than raw match strength);
    // longer span wins ties (more specific).
    order.sort_by(|&a, &b| {
        let key = |i: usize| {
            let s = spans[i].candidates.first().map(|c| c.score).unwrap_or(0.0);
            if spans[i].kg_alias { s * 1.08 } else { s }
        };
        key(b).total_cmp(&key(a))
            .then((spans[b].end - spans[b].start).cmp(&(spans[a].end - spans[a].start)))
    });

    let mut taken: Vec<(usize, usize)> = Vec::new();
    let mut mentions = Vec::new();
    for i in order {
        let (start, end) = (spans[i].start, spans[i].end);
        if taken.iter().any(|&(s, e)| start < e && s < end) {
            spans[i].status = "overlapped".into();
            for c in spans[i].candidates.iter_mut() {
                c.reject_reason = Some("span_not_selected".into());
            }
            continue;
        }
        taken.push((start, end));
        mentions.push(i);
        for (k, c) in spans[i].candidates.iter_mut().enumerate() {
            if k < TOP_K && c.score >= SELECT_THRESHOLD {
                c.selected = true;
            } else {
                c.reject_reason = Some(if c.score < SELECT_THRESHOLD {
                    "below_threshold".into()
                } else {
                    "outranked".into()
                });
            }
        }
    }
    mentions.sort_by_key(|&i| spans[i].start);
    mentions
}

/// Convert a trace into the gRPC Resolve response (selected mentions only —
/// the full trace is served by the Explain RPC).
pub fn trace_to_proto(trace: &Trace) -> stemma_proto::v1::ResolveResponse {
    use stemma_proto::v1 as pb;
    let mentions = trace
        .mentions
        .iter()
        .map(|&i| {
            let s = &trace.spans[i];
            pb::Mention {
                text: s.text.clone(),
                start: s.start as u32,
                end: s.end as u32,
                nil: false,
                candidates: s
                    .candidates
                    .iter()
                    .filter(|c| c.selected)
                    .map(|c| pb::Candidate {
                        table: c.table.clone(),
                        rowid: c.rowid,
                        column: c.column.clone(),
                        value: c.value.clone(),
                        score: c.score,
                        snippet: c.snippet.clone().unwrap_or_default(),
                        is_doc: c.is_doc,
                        evidence: c
                            .channels
                            .iter()
                            .map(|ch| pb::Evidence {
                                kind: Some(pb::evidence::Kind::Lexical(pb::LexicalMatch {
                                    channel: ch.channel.clone(),
                                    matched_text: c
                                        .snippet
                                        .clone()
                                        .unwrap_or_else(|| c.value.clone()),
                                    score: ch.raw,
                                })),
                            })
                            .collect(),
                    })
                    .collect(),
            }
        })
        .collect();
    pb::ResolveResponse {
        mentions,
        rewritten_query: String::new(),
    }
}

/// Convert a trace into the Explain RPC response (the full trajectory).
pub fn trace_to_explain_proto(trace: &Trace) -> stemma_proto::v1::ExplainResponse {
    use stemma_proto::v1 as pb;
    pb::ExplainResponse {
        query: trace.query.clone(),
        elapsed_ms: trace.elapsed_ms,
        tokens: trace
            .tokens
            .iter()
            .map(|t| pb::TraceToken {
                text: t.text.clone(),
                start: t.start as u32,
                end: t.end as u32,
                stopword: t.stopword,
            })
            .collect(),
        spans: trace
            .spans
            .iter()
            .map(|s| pb::TraceSpan {
                kg_alias: s.kg_alias,
                id: s.id as u32,
                text: s.text.clone(),
                start: s.start as u32,
                end: s.end as u32,
                status: s.status.clone(),
                candidates: s
                    .candidates
                    .iter()
                    .map(|c| pb::TraceCandidate {
                        table: c.table.clone(),
                        column: c.column.clone(),
                        rowid: c.rowid,
                        value: c.value.clone(),
                        value_truncated: c.value_truncated,
                        score: c.score,
                        selected: c.selected,
                        reject_reason: c.reject_reason.clone().unwrap_or_default(),
                        snippet: c.snippet.clone().unwrap_or_default(),
                        is_doc: c.is_doc,
                        channels: c
                            .channels
                            .iter()
                            .map(|ch| pb::TraceChannelScore {
                                channel: ch.channel.clone(),
                                rank: ch.rank as u32,
                                raw: ch.raw,
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect(),
        mentions: trace.mentions.iter().map(|&i| i as u32).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loads the canonical mini corpus (eval/testdata/mini.sql) into a temp
    /// user DB and ingests it.
    fn readme_db(tag: &str) -> StemmaDb {
        let dir = std::env::temp_dir().join(format!("stemma-resolve-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let store = dir.join("user.stemmadb");
        let _ = std::fs::remove_file(&user);
        let _ = std::fs::remove_file(&store);
        {
            let c = stemmadb::rusqlite::Connection::open(&user).unwrap();
            c.execute_batch(include_str!("../../../eval/testdata/mini.sql"))
                .unwrap();
        }
        let db = StemmaDb::open(&store, &user).unwrap();
        stemma_ingest::build_lexical_index(&db, false).unwrap();
        db
    }

    #[test]
    fn seattle_office_resolves_with_evidence() {
        let db = readme_db("seattle");
        let trace = resolve_lexical(&db, "the Q3 numbers for the Seattle office").unwrap();
        let seattle = trace
            .mentions
            .iter()
            .map(|&i| &trace.spans[i])
            .find(|s| s.text.contains("Seattle"))
            .expect("Seattle mention");
        let top = seattle.candidates.iter().find(|c| c.selected).unwrap();
        assert_eq!(top.table, "offices");
        assert!(top.score >= 0.9, "exact city match should score high");
    }

    #[test]
    fn overlapped_spans_keep_near_misses() {
        let db = readme_db("overlap");
        let trace = resolve_lexical(&db, "what did Wei Chen ship").unwrap();
        // "Wei Chen" (exact person match) must win its byte range...
        let mention_texts: Vec<_> = trace
            .mentions
            .iter()
            .map(|&i| trace.spans[i].text.as_str())
            .collect();
        assert!(mention_texts.contains(&"Wei Chen"), "got {mention_texts:?}");
        // ...and the losing sub-span "Chen" keeps its candidates as
        // near-misses, marked span_not_selected — including the OTHER Chen
        // (Dana), which a disambiguation UI must be able to show.
        let chen = trace
            .spans
            .iter()
            .find(|s| s.text == "Chen" && s.status == "overlapped")
            .expect("overlapped Chen span");
        assert!(chen
            .candidates
            .iter()
            .all(|c| !c.selected
                && c.reject_reason.as_deref() == Some("span_not_selected")));
        assert!(
            chen.candidates.iter().any(|c| c.value.contains("Dana")),
            "the rival Chen must remain visible as a near-miss"
        );
    }

    #[test]
    fn fuzzy_substring_match_finds_northgate() {
        let db = readme_db("northgate");
        let trace = resolve_lexical(&db, "revenue at Northgate").unwrap();
        let span = trace
            .mentions
            .iter()
            .map(|&i| &trace.spans[i])
            .find(|s| s.text == "Northgate")
            .expect("Northgate mention");
        assert!(span
            .candidates
            .iter()
            .any(|c| c.value.contains("Seattle - Northgate")
                && c.channels.iter().any(|ch| ch.channel == "trigram")));
    }

    #[test]
    fn proto_conversion_keeps_offsets_and_evidence() {
        let db = readme_db("proto");
        let trace = resolve_lexical(&db, "shipments from the Billing team").unwrap();
        let resp = trace_to_proto(&trace);
        assert!(!resp.mentions.is_empty());
        for m in &resp.mentions {
            assert_eq!(
                &trace.query[m.start as usize..m.end as usize],
                m.text.as_str()
            );
            for c in &m.candidates {
                assert!(!c.evidence.is_empty(), "candidates must carry evidence");
            }
        }
    }

    #[test]
    fn explain_proto_preserves_near_misses() {
        let db = readme_db("explain");
        let trace = resolve_lexical(&db, "what did Wei Chen ship").unwrap();
        let explain = trace_to_explain_proto(&trace);
        let rejected: usize = explain
            .spans
            .iter()
            .flat_map(|s| &s.candidates)
            .filter(|c| !c.selected)
            .count();
        assert!(rejected > 0, "explain must carry rejected candidates");
        assert_eq!(explain.spans.len(), trace.spans.len());
    }

    #[test]
    fn document_corpus_resolution_works() {
        // The careg failure mode in miniature: values are long documents, so
        // no exact channel and length affinity must not crush the scores.
        let dir = std::env::temp_dir().join(format!("stemma-resolve-{}-docs", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let store = dir.join("user.stemmadb");
        let _ = std::fs::remove_file(&user);
        let _ = std::fs::remove_file(&store);
        {
            let c = stemmadb::rusqlite::Connection::open(&user).unwrap();
            let pad = "The remainder of this section sets out procedural requirements, \
                       definitions, and cross-references to related provisions. "
                .repeat(3);
            c.execute_batch(&format!(
                "CREATE TABLE regs(id INTEGER PRIMARY KEY, body TEXT);
                 INSERT INTO regs VALUES
                   (1, 'Coastal development permits require commission approval. {pad}'),
                   (2, 'Insurance filings are reviewed by the commissioner. {pad}'),
                   (3, 'Coastal zone boundaries are established by the commission. {pad}');"
            ))
            .unwrap();
        }
        let db = StemmaDb::open(&store, &user).unwrap();
        stemma_ingest::build_lexical_index(&db, false).unwrap();

        let trace = resolve_lexical(&db, "coastal development permits").unwrap();
        assert!(
            !trace.mentions.is_empty(),
            "document corpus must produce mentions: {trace:?}"
        );
        let best = trace
            .mentions
            .iter()
            .map(|&i| &trace.spans[i])
            .flat_map(|s| &s.candidates)
            .find(|c| c.selected)
            .expect("a selected candidate");
        assert!(best.is_doc);
        assert_eq!(best.table, "regs");
        assert!(
            best.snippet.as_deref().unwrap_or("").contains('⟨'),
            "doc hits carry a marked snippet: {:?}",
            best.snippet
        );
        // The coastal-permit doc (1) must outrank the insurance doc (2).
        assert_eq!(best.rowid, 1);
    }

    #[test]
    fn missing_index_is_a_clear_error() {
        let db = StemmaDb::open_in_memory().unwrap();
        match resolve_lexical(&db, "anything") {
            Err(Error::IndexMissing) => {}
            other => panic!("expected IndexMissing, got {other:?}"),
        }
    }
}
