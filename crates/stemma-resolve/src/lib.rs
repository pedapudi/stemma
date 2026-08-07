//! stemma-resolve: the resolution pipeline.
//!
//! Current stage of the roadmap: the lexical cascade (milestone 2) — span
//! enumeration, exact + BM25 + trigram candidate generation over the lexical
//! index built by stemma-ingest, reciprocal-rank fusion, and greedy
//! non-overlapping mention selection — plus the knowledge-graph assists:
//! KG-aided mention detection, the term-coherence bonus, and collective
//! disambiguation of candidate tuples over join paths.
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
    #[error("knowledge store error: {0}")]
    Kg(#[from] stemma_kg::Error),
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
/// The candidate unit for value hits is the *interpretation* — one distinct
/// (table, column, normalized value) — not the row. Each interpretation
/// carries up to this many concrete sample rowids (ascending; the first is
/// the representative `rowid`), enough for citation and for the collective
/// stage's instance probes without hauling whole row sets around.
const SAMPLE_ROWIDS: usize = 3;
/// Document hits keep per-row identity (each document is its own reading),
/// but within a channel one (table, column) may fill at most this many of
/// the PER_CHANNEL_LIMIT slots — max(2, PER_CHANNEL_LIMIT / 2) — so a table
/// with many matching documents can no longer starve every other table out
/// of the channel budget.
const DOC_COLUMN_QUOTA: usize = if PER_CHANNEL_LIMIT / 2 > 2 {
    PER_CHANNEL_LIMIT / 2
} else {
    2
};
/// Dense KNN over-fetch factor: vec0 applies `k` inside the KNN, before any
/// grouping is possible, so on a denormalized corpus copies of one repeated
/// string (identical text ⇒ identical vector ⇒ adjacent in the ordering) can
/// consume every slot. Fetch `PER_CHANNEL_LIMIT * DENSE_OVERFETCH`, collapse
/// to one hit per interpretation, then truncate: 4 covers realistic join
/// fan-out at a small constant cost — the collapse is linear.
const DENSE_OVERFETCH: usize = 4;
/// Fused score below which a candidate is kept in the trace but not selected.
const SELECT_THRESHOLD: f64 = 0.35;
/// Max selected candidates per mention.
const TOP_K: usize = 5;
/// Dense KNN is a full scan of the vector table per probe; spend it only on
/// spans the lexical channels left uncertain, at most this many per query.
const DENSE_MAX_SPANS: usize = 4;
/// A mention is ambiguous — and routed to LM adjudication — when its top two
/// candidates are within this fused-score margin and neither has an exact
/// hit. Rationale: a single rank-step of disagreement in one RRF channel
/// moves the normalized base by (1/K − 1/(K+1))/(3/K) ≈ 0.067 at K = 4, and
/// the doc/affinity factors only shrink that. So a gap under 0.08 is what
/// fusion produces when the channels ordered two candidates by roughly one
/// rank inversion — noise-level evidence — while a larger gap reflects
/// genuine multi-channel agreement that needs no model's opinion.
const ADJUDICATION_MARGIN: f64 = 0.08;
/// Collective disambiguation jointly scores at most this many provisional
/// mentions (strongest first): tuple count is bounded by
/// MAX_TUPLE_K^MAX_TUPLE_MENTIONS.
const MAX_TUPLE_MENTIONS: usize = 4;
/// Top candidates per mention entering joint tuple scoring.
const MAX_TUPLE_K: usize = 4;
/// A schema path connecting two candidates' tables may use at most this
/// many fk/inferred_fk edges.
const MAX_PATH_HOPS: usize = 2;
/// Schema paths probed per table pair. Instance probes are LIMIT-1 joins
/// and run only on pairs a schema path survived, at most this many each.
const MAX_PATHS_PER_PAIR: usize = 4;
/// Added to both candidates of an instance-verified pair in the winning
/// tuple. Sized above the RRF gap between adjacent-rank rivals of the same
/// span in the value branch (≈0.12) so a verified connection can overturn
/// a purely lexical ordering; capped by COHERENCE_CAP.
const COHERENCE_BOOST: f64 = 0.15;
/// Coherence never lifts a candidate into the exact band (0.9+): if the
/// user typed the stored value, they meant the stored value.
const COHERENCE_CAP: f64 = 0.9;
/// Context coherence: bonus per distinct query context term (a non-mention
/// content token that is a compiled KG term) whose col_affinity edge points
/// at a value candidate's (table, column). Sized below the ≈ 0.067 RRF gap
/// of one rank step so context refines an ordering the channels left close
/// rather than overriding lexical evidence; two supporting terms clear it.
const CONTEXT_TERM_BONUS: f64 = 0.05;
/// At most this many distinct context terms may support one candidate —
/// the same tiebreaker-not-retrieval-signal discipline as the doc-coherence
/// bonus; the summed bonus is further capped at COHERENCE_CAP.
const CONTEXT_TERM_MAX: usize = 2;

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
    /// True when the LM adjudication band chose this candidate; the choice
    /// is applied as a reorder, so an adjudicated candidate sits at rank 0.
    pub adjudicated: bool,
    /// Instance-level connection to a co-mention's candidate, verified in
    /// the user database during collective disambiguation — a human-readable
    /// path like "people #2 ←lead_id— teams #43".
    pub coherence: Option<String>,
    /// For value candidates: how many user rows share this interpretation
    /// (table, column, normalized value) — "40 brands named Ellis". `rowid`
    /// is a representative of that set (its smallest rowid). Always 1 for
    /// document candidates, which keep per-row identity.
    pub row_count: u32,
    /// Up to [`SAMPLE_ROWIDS`] concrete rowids carrying the interpretation,
    /// ascending; the first is `rowid`.
    pub sample_rowids: Vec<i64>,
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
    /// Distinct readings remained tied after every disambiguation stage —
    /// context coherence, encoder affinity, adjudication. The honest
    /// resolution is a question; consumers should ask, not guess.
    pub ambiguous: bool,
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

/// Resolve `query` against the lexical index alone.
pub fn resolve_lexical(db: &StemmaDb, query: &str) -> Result<Trace> {
    resolve(db, query, None)
}

/// Resolve `query` with every available channel. The embedder is optional
/// and fallible: absent or down, resolution is lexical+kg only.
pub fn resolve(
    db: &StemmaDb,
    query: &str,
    embedder: Option<&dyn stemma_embed::Embedder>,
) -> Result<Trace> {
    resolve_full(db, query, embedder, None)
}

/// Resolve `query` with every available channel plus the LM adjudication
/// band. Like the embedder, the LM is optional and fallible: absent or down,
/// the trace is exactly what [`resolve`] would have produced.
pub fn resolve_full(
    db: &StemmaDb,
    query: &str,
    embedder: Option<&dyn stemma_embed::Embedder>,
    lm: Option<&dyn stemma_lm::LmBackend>,
) -> Result<Trace> {
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

    // With a dense channel available, the whole query is itself a semantic
    // unit: mentions like "getting fired from a state job" have no lexical
    // anchor at any n-gram width, but the full phrase lands near the right
    // documents in vector space. Give the full query its own span and let
    // greedy selection arbitrate — strong lexical anchors (exact ≈ 0.9)
    // outrank it and mark it overlapped; anchor-free semantic queries are
    // won by it, which also suppresses incidental substring junk.
    if embedder.is_some() && tokens.len() > MAX_SPAN_TOKENS {
        let (start, end) = (tokens[0].start, tokens[tokens.len() - 1].end);
        spans.push(Span {
            id: spans.len(),
            text: query[start..end].to_string(),
            start,
            end,
            status: "selected".into(),
            candidates: Vec::new(),
            kg_alias: false,
            ambiguous: false,
        });
    }

    // KG-assisted mention detection: spans matching a compiled phrase/term
    // entity are marked and favored in selection — multi-word entities like
    // "coastal development permit" beat their fragments.
    {
        let mut stmt =
            conn.prepare("SELECT count(*) FROM sqlite_master WHERE name = 'kg_nodes'")?;
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

    // Phase 1: lexical raw hits for every live span.
    let mut raw: std::collections::HashMap<usize, Vec<RawHit>> = std::collections::HashMap::new();
    for span in spans.iter() {
        if span.status == "skipped" {
            continue;
        }
        raw.insert(span.id, gather_lexical_hits(db, &span.text)?);
    }

    // Phase 2: the dense channel, targeted. KNN over vec0 is a full scan of
    // the vector table per probe, so it is spent only where lexical evidence
    // is thin — spans without an exact hit — longest spans first, capped.
    // One batched embedding call; failures degrade, never abort.
    if let Some(embedder) = embedder {
        let has_dense: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'vec_dense'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if has_dense > 0 {
            let mut targets: Vec<&Span> = spans
                .iter()
                .filter(|s| s.status != "skipped")
                .filter(|s| {
                    raw.get(&s.id)
                        .map(|h| !h.iter().any(|x| x.channel == "exact"))
                        .unwrap_or(false)
                })
                .collect();
            targets.sort_by_key(|s| std::cmp::Reverse(s.end - s.start));
            targets.truncate(DENSE_MAX_SPANS);
            let texts: Vec<String> = targets
                .iter()
                .map(|s| embedder.format_query(&s.text))
                .collect();
            let ids: Vec<usize> = targets.iter().map(|s| s.id).collect();
            match embedder.embed(&texts) {
                Ok(vecs) => {
                    for (id, v) in ids.into_iter().zip(vecs) {
                        let hits = dense_hits(db, &v)?;
                        raw.entry(id).or_default().extend(hits);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "dense channel degraded"),
            }
        }
    }

    // Phase 3: fuse and refine.
    for span in spans.iter_mut() {
        if span.status == "skipped" {
            continue;
        }
        let hits = raw.remove(&span.id).unwrap_or_default();
        let mut candidates = fuse(&span.text, hits);
        apply_kg_coherence(db, &span.text, &mut candidates)?;
        apply_context_coherence(db, &tokens, span.start, span.end, &mut candidates)?;
        span.candidates = candidates;
        if span.candidates.is_empty() {
            span.status = "no_candidates".into();
        } else if span.candidates[0].score < SELECT_THRESHOLD {
            span.status = "weak".into();
        }
    }

    // Phase 4: collective disambiguation — candidates of the provisional
    // mentions are scored jointly against the knowledge graph and the data,
    // before final selection orders on the boosted scores.
    apply_collective_coherence(db, &mut spans)?;

    // Phase 4b: context affinity — tied value interpretations (same value in
    // two columns) separated by conditioning the interpretation cards on the
    // full query. Self-contained section below; degrades silently.
    apply_context_affinity(db, embedder, query, &mut spans);

    let mentions = select_mentions(&mut spans);

    let mut trace = Trace {
        query: query.to_string(),
        tokens,
        spans,
        mentions,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    };
    if let Some(lm) = lm {
        adjudicate(&mut trace, lm);
        trace.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    }
    mark_ambiguous(&mut trace);
    Ok(trace)
}

/// The end of the escalation: after context coherence, encoder affinity and
/// (when permitted) adjudication have all had their turn, a mention whose
/// top readings are still tied across DISTINCT interpretations is marked
/// `ambiguous` — the resolution's honest answer is a question. A span the
/// adjudicator settled (top candidate `adjudicated`) or already marked is
/// left alone; same-interpretation ties (two rows of one reading) are not
/// ambiguity, they are the same answer.
fn mark_ambiguous(trace: &mut Trace) {
    for &sid in &trace.mentions {
        let span = &mut trace.spans[sid];
        if span.ambiguous {
            continue;
        }
        if !is_ambiguous(span) {
            continue;
        }
        let mut selected = span.candidates.iter().filter(|c| c.selected);
        let (Some(top), Some(second)) = (selected.next(), selected.next()) else {
            continue;
        };
        if top.adjudicated {
            continue;
        }
        if top.table != second.table || top.column != second.column {
            span.ambiguous = true;
        }
    }
}

/// The constrained-adjudication band: for each mention whose top candidates
/// fusion could not order (see [`ADJUDICATION_MARGIN`]), show the LM the
/// selected candidates and ask for a constrained choice — a candidate index
/// or an explicit nil. The LM decides among presented options only; it never
/// retrieves. Its verdict is applied as a reorder (marked `adjudicated`) or,
/// on nil, a demotion of the span to "weak". LM failure is a no-op: the
/// trace stays exactly as fusion left it.
fn adjudicate(trace: &mut Trace, lm: &dyn stemma_lm::LmBackend) {
    let mut routed = 0usize;
    for &sid in &trace.mentions {
        let span = &trace.spans[sid];
        if !is_ambiguous(span) {
            continue;
        }
        routed += 1;
        let presented: Vec<usize> = span
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.selected)
            .map(|(i, _)| i)
            .take(TOP_K)
            .collect();
        let (messages, schema) = adjudication_prompt(&trace.query, span, &presented);
        let verdict = match lm.chat(&messages, Some(&schema)) {
            Ok(reply) => parse_verdict(&reply, presented.len()),
            Err(e) => {
                tracing::warn!(error = %e, span = %span.text, "adjudication degraded");
                continue;
            }
        };
        let span = &mut trace.spans[sid];
        match verdict {
            Some(Verdict::Choice(i)) => {
                let mut chosen = span.candidates.remove(presented[i]);
                chosen.adjudicated = true;
                chosen.selected = true;
                chosen.reject_reason = None;
                span.candidates.insert(0, chosen);
            }
            Some(Verdict::Nil) => span.status = "weak".into(),
            Some(Verdict::Ambiguous) => span.ambiguous = true,
            None => {
                tracing::warn!(span = %span.text, "adjudication reply unusable; ignored");
            }
        }
    }
    tracing::debug!(
        adjudicated = routed,
        mentions = trace.mentions.len(),
        "adjudication routing"
    );
}

/// The ambiguous band: two or more selected candidates with the top two
/// within [`ADJUDICATION_MARGIN`]. An exact-channel winner normally exits
/// the band — an exact match is definitionally right about the value — with
/// one exception: when BOTH top candidates are exact matches of DISTINCT
/// interpretations (the same string in two columns), exactness settles
/// nothing and the tie is the canonical ambiguity (issue #1's Ellis case).
fn is_ambiguous(span: &Span) -> bool {
    if span.status != "selected" {
        return false;
    }
    let mut selected = span.candidates.iter().filter(|c| c.selected);
    let (Some(top), Some(second)) = (selected.next(), selected.next()) else {
        return false;
    };
    if top.score - second.score >= ADJUDICATION_MARGIN {
        return false;
    }
    let exact = |c: &Candidate| c.channels.iter().any(|ch| ch.channel == "exact");
    let distinct = top.table != second.table || top.column != second.column;
    !exact(top) || (exact(second) && distinct)
}

enum Verdict {
    Choice(usize),
    Nil,
    /// More than one presented reading genuinely fits the query — distinct
    /// from Nil (none fit). Routes to the ask-back path.
    Ambiguous,
}

/// Terse, deterministic prompt: the mention in its query, each presented
/// candidate as `index. table.column #rowid — value (channels)`, and a JSON
/// schema whose enum is exactly the presented indices plus "nil" — the NIL
/// option is a schema member, not prose.
fn adjudication_prompt(
    query: &str,
    span: &Span,
    presented: &[usize],
) -> (Vec<stemma_lm::ChatMessage>, serde_json::Value) {
    let mut listing = String::new();
    for (i, &ci) in presented.iter().enumerate() {
        let c = &span.candidates[ci];
        let shown = c.snippet.as_deref().unwrap_or(&c.value);
        let channels: Vec<&str> = c.channels.iter().map(|ch| ch.channel.as_str()).collect();
        let mut extras = String::new();
        if c.row_count > 1 {
            extras.push_str(&format!(" · {} rows share this value", c.row_count));
        }
        if let Some(path) = &c.coherence {
            extras.push_str(&format!(" · verified path: {path}"));
        }
        listing.push_str(&format!(
            "{i}. {}.{} #{} — {:?} (channels: {}{extras})\n",
            c.table,
            c.column,
            c.rowid,
            shown,
            channels.join(", ")
        ));
    }
    let mut options: Vec<String> = (0..presented.len()).map(|i| i.to_string()).collect();
    options.push("nil".into());
    options.push("ambiguous".into());
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "choice": { "enum": options } },
        "required": ["choice"],
        "additionalProperties": false,
    });
    let messages = vec![
        stemma_lm::ChatMessage::system(
            "You disambiguate references to database records. Pick the candidate \
             record the mention refers to in this query; answer nil if none \
             fits, or ambiguous if the query genuinely supports more than one.",
        ),
        stemma_lm::ChatMessage::user(format!(
            "Query: {query}\nMention: {:?}\nCandidates:\n{listing}",
            span.text
        )),
    ];
    (messages, schema)
}

/// Parse `{"choice": "<index>" | "nil"}`, tolerating a bare integer for
/// index, and rejecting out-of-range indices.
fn parse_verdict(reply: &str, presented: usize) -> Option<Verdict> {
    let v: serde_json::Value = serde_json::from_str(reply).ok()?;
    let choice = v.get("choice")?;
    if choice.as_str() == Some("nil") {
        return Some(Verdict::Nil);
    }
    if choice.as_str() == Some("ambiguous") {
        return Some(Verdict::Ambiguous);
    }
    let i = match choice {
        serde_json::Value::String(s) => s.parse::<usize>().ok()?,
        serde_json::Value::Number(n) => usize::try_from(n.as_u64()?).ok()?,
        _ => return None,
    };
    (i < presented).then_some(Verdict::Choice(i))
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
                ambiguous: false,
            });
        }
    }
    spans
}

struct RawHit {
    table: String,
    column: String,
    /// Representative rowid: for value hits, the smallest rowid of the
    /// interpretation; for document hits, the matched row itself.
    rowid: i64,
    value: String,
    channel: &'static str,
    rank: usize,
    raw: f64,
    is_doc: bool,
    snippet: Option<String>,
    /// Rows sharing the interpretation (1 for documents).
    row_count: u32,
    /// Up to SAMPLE_ROWIDS rowids, ascending, first = `rowid`.
    sample_rowids: Vec<i64>,
}

/// Up to SAMPLE_ROWIDS concrete rowids of one value interpretation,
/// ascending, cached per (table, column, value_norm) across channels.
fn interpretation_samples(
    conn: &stemmadb::rusqlite::Connection,
    cache: &mut std::collections::HashMap<(String, String, String), Vec<i64>>,
    table: &str,
    column: &str,
    value_norm: &str,
) -> Result<Vec<i64>> {
    let key = (table.to_string(), column.to_string(), value_norm.to_string());
    if let Some(hit) = cache.get(&key) {
        return Ok(hit.clone());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT src_rowid FROM lex_values
         WHERE src_table = ?1 AND src_column = ?2 AND value_norm = ?3
         ORDER BY src_rowid LIMIT ?4",
    )?;
    let rowids: Vec<i64> = stmt
        .query_map(
            stemmadb::rusqlite::params![table, column, value_norm, SAMPLE_ROWIDS as i64],
            |r| r.get(0),
        )?
        .collect::<std::result::Result<_, _>>()?;
    cache.insert(key, rowids.clone());
    Ok(rowids)
}

fn gather_lexical_hits(db: &StemmaDb, span: &str) -> Result<Vec<RawHit>> {
    let conn = db.conn();
    let mut hits: Vec<RawHit> = Vec::new();
    let mut samples = std::collections::HashMap::new();

    // Channel 1: exact (case/whitespace-normalized), short values only.
    // Aggregated per interpretation — (table, column, normalized value) —
    // so 40 rows sharing one value spend one candidate slot, not eight, and
    // every distinct reading of the span surfaces. Every exact hit is equal
    // evidence about the value, so every interpretation enters at rank 0:
    // no fabricated decay across identical values.
    {
        let mut stmt = conn.prepare_cached(
            "SELECT src_table, src_column, min(src_rowid), value, value_norm, count(*)
             FROM lex_values
             WHERE value_norm = lower(trim(?1)) AND length(value) <= ?2
             GROUP BY src_table, src_column
             ORDER BY src_table, src_column
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
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            },
        )?;
        for row in rows {
            let (table, column, rowid, value, value_norm, n) = row?;
            let sample_rowids =
                interpretation_samples(&conn, &mut samples, &table, &column, &value_norm)?;
            hits.push(RawHit {
                table,
                column,
                rowid,
                value,
                channel: "exact",
                rank: 0,
                raw: 1.0,
                is_doc: false,
                snippet: None,
                row_count: n as u32,
                sample_rowids,
            });
        }
    }

    // Channels 2 & 3: BM25 token search and trigram fuzzy/substring search.
    // Value hits are aggregated per interpretation in SQL (best bm25, row
    // count, representative rowid); document hits keep per-row identity but
    // are windowed to DOC_COLUMN_QUOTA per (table, column) before the
    // channel-wide LIMIT, so one document table cannot starve another.
    for (channel, fts_table) in [("bm25", "lex_fts"), ("trigram", "lex_trigram")] {
        let sql = format!(
            // MATERIALIZED: the CTE is read by both UNION arms, and FTS5
            // auxiliary functions (bm25, snippet) are only usable inside
            // the MATCH query itself — materialization keeps them there.
            "WITH matched AS MATERIALIZED (
                SELECT v.src_table AS t, v.src_column AS c, v.src_rowid AS r,
                       v.value AS value, v.value_norm AS vn, v.is_doc AS is_doc,
                       bm25({fts}) AS b,
                       CASE WHEN v.is_doc = 1
                            THEN snippet({fts}, 0, '⟨', '⟩', '…', 10) END AS snip
                FROM {fts} f JOIN lex_values v ON v.id = f.rowid
                WHERE {fts} MATCH ?1
             )
             SELECT t, c, rep, value, vn, b, is_doc, n, snip FROM (
                SELECT t, c, min(r) AS rep, min(value) AS value, vn,
                       min(b) AS b, 0 AS is_doc, count(*) AS n, NULL AS snip
                FROM matched WHERE is_doc = 0 GROUP BY t, c, vn
                UNION ALL
                SELECT t, c, r AS rep, value, NULL AS vn, b, 1 AS is_doc,
                       1 AS n, snip
                FROM (SELECT *, row_number() OVER (
                          PARTITION BY t, c ORDER BY b) AS rn
                      FROM matched WHERE is_doc = 1)
                WHERE rn <= ?3
             )
             ORDER BY b, t, c LIMIT ?2",
            fts = fts_table
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        // Quote as an FTS5 string so query punctuation isn't FTS syntax.
        let fts_query = format!("\"{}\"", span.replace('"', "\"\""));
        let rows = stmt.query_map(
            stemmadb::rusqlite::params![
                fts_query,
                PER_CHANNEL_LIMIT as i64,
                DOC_COLUMN_QUOTA as i64
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, f64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            },
        );
        let rows = match rows {
            Ok(rows) => rows,
            // Spans under 3 chars (or odd tokenizations) can make a trigram
            // query legitimately unmatchable — treat as zero hits.
            Err(_) => continue,
        };
        // Competition ranking on the channel-native score: entries with an
        // identical bm25 are identical evidence and share a rank, so two
        // interpretations of the same surface string fuse identically.
        let mut prev_raw = f64::INFINITY;
        let mut rank = 0usize;
        for (idx, row) in rows.enumerate() {
            let (table, column, rowid, value, vn, bm25, is_doc, n, snippet) = match row {
                Ok(v) => v,
                Err(_) => continue,
            };
            let is_doc = is_doc != 0;
            let raw = -bm25; // SQLite bm25() is lower-is-better; negate.
            if raw < prev_raw {
                rank = idx;
                prev_raw = raw;
            }
            let sample_rowids = match &vn {
                Some(vn) => {
                    interpretation_samples(&conn, &mut samples, &table, &column, vn)?
                }
                None => vec![rowid],
            };
            hits.push(RawHit {
                table,
                column,
                rowid,
                value,
                channel,
                rank,
                raw,
                is_doc,
                snippet: if is_doc { snippet } else { None },
                row_count: n as u32,
                sample_rowids,
            });
        }
    }

    Ok(hits)
}

/// Dense KNN over vec0. Documents were embedded whole; the span vector
/// carries the retrieval instruction. L2 on unit vectors → cos = 1 − d²/2.
///
/// Mirrors the lexical channels' interpretation semantics (issue #3): the
/// KNN is over-fetched by [`DENSE_OVERFETCH`], hits are collapsed to one per
/// `(table, column, value_norm)` keeping the nearest member as the
/// representative and counting the collapsed copies into `row_count`, the
/// collapsed *document* hits then pass the same per-(table, column)
/// [`DOC_COLUMN_QUOTA`] window the FTS channels apply, and the result is
/// truncated to [`PER_CHANNEL_LIMIT`]. Dense and lexical candidates report
/// the same shape, and `row_count` means the same thing in every channel.
fn dense_hits(db: &StemmaDb, v: &[f32]) -> Result<Vec<RawHit>> {
    let conn = db.conn();
    let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    let mut stmt = conn.prepare_cached(
        "SELECT src_table, src_column, src_rowid, distance FROM vec_dense
         WHERE embedding MATCH ?1 AND k = ?2",
    )?;
    let rows = stmt.query_map(
        stemmadb::rusqlite::params![blob, (PER_CHANNEL_LIMIT * DENSE_OVERFETCH) as i64],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        },
    );

    /// One interpretation's collapsed KNN members, nearest first.
    struct Group {
        table: String,
        column: String,
        /// Nearest member — the representative.
        rowid: i64,
        value: String,
        cosine: f64,
        is_doc: bool,
        row_count: u32,
        member_rowids: Vec<i64>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut index: std::collections::HashMap<(String, String, String), usize> =
        std::collections::HashMap::new();
    if let Ok(rows) = rows {
        // vec0 returns ascending distance, so the first member of each
        // interpretation seen is its nearest — the representative.
        for row in rows {
            let Ok((table, column, rowid, dist)) = row else {
                continue;
            };
            let cosine = 1.0 - (dist * dist) / 2.0;
            let looked: Option<(String, String, i64)> = conn
                .query_row(
                    "SELECT value, value_norm, is_doc FROM lex_values
                     WHERE src_table = ?1 AND src_column = ?2 AND src_rowid = ?3",
                    stemmadb::rusqlite::params![table, column, rowid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            // A vector whose lex row vanished has no collapse key; keep it
            // as its own group (rowid-keyed) rather than merging unknowns.
            let (value, norm, is_doc) = match looked {
                Some((v, n, d)) => (v, n, d != 0),
                None => (String::new(), format!("\u{0}missing:{rowid}"), true),
            };
            let key = (table.clone(), column.clone(), norm);
            match index.get(&key) {
                Some(&i) => {
                    groups[i].row_count += 1;
                    groups[i].member_rowids.push(rowid);
                }
                None => {
                    index.insert(key, groups.len());
                    groups.push(Group {
                        table,
                        column,
                        rowid,
                        value,
                        cosine,
                        is_doc,
                        row_count: 1,
                        member_rowids: vec![rowid],
                    });
                }
            }
        }
    }

    // The lexical window semantics, post-collapse: one (table, column) of
    // documents may fill at most DOC_COLUMN_QUOTA slots; value hits are
    // already one-per-interpretation and get no quota.
    let mut doc_seen: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    let mut hits: Vec<RawHit> = Vec::new();
    for g in groups {
        if g.is_doc {
            let seen = doc_seen
                .entry((g.table.clone(), g.column.clone()))
                .or_insert(0);
            *seen += 1;
            if *seen > DOC_COLUMN_QUOTA {
                continue;
            }
        }
        // Representative first, remaining members ascending — the same
        // "first = rowid" contract the lexical samples keep.
        let mut rest: Vec<i64> = g
            .member_rowids
            .iter()
            .copied()
            .filter(|&r| r != g.rowid)
            .collect();
        rest.sort_unstable();
        let mut sample_rowids = vec![g.rowid];
        sample_rowids.extend(rest);
        sample_rowids.truncate(SAMPLE_ROWIDS);
        hits.push(RawHit {
            table: g.table,
            column: g.column,
            rowid: g.rowid,
            value: g.value,
            channel: "dense",
            rank: 0, // assigned below
            raw: g.cosine,
            is_doc: g.is_doc,
            snippet: None,
            row_count: g.row_count,
            sample_rowids,
        });
    }
    hits.truncate(PER_CHANNEL_LIMIT);
    // Competition ranking on the cosine, as in the lexical channels:
    // identical evidence shares a rank.
    let mut prev_raw = f64::INFINITY;
    let mut rank = 0usize;
    for (idx, h) in hits.iter_mut().enumerate() {
        if h.raw < prev_raw {
            rank = idx;
            prev_raw = h.raw;
        }
        h.rank = rank;
    }
    Ok(hits)
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
        .query_map(stemmadb::rusqlite::params_from_iter(tokens.iter()), |r| {
            r.get(0)
        })?
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

/// Context coherence for *value* candidates: the query's own non-mention
/// content words disambiguate between interpretations of the same string,
/// with no model call. Each context token that is a compiled KG term and has
/// a col_affinity edge to a candidate's (table, column) is one supporting
/// term; a candidate earns CONTEXT_TERM_BONUS per distinct supporting term
/// (at most CONTEXT_TERM_MAX), capped at COHERENCE_CAP — context refines
/// orderings below the exact band and never demotes or outranks an exact
/// match. The support is recorded as a "kg" channel entry (raw = the bonus)
/// so trajectories show it even where the cap left the score unchanged.
fn apply_context_coherence(
    db: &StemmaDb,
    tokens: &[Token],
    span_start: usize,
    span_end: usize,
    candidates: &mut Vec<Candidate>,
) -> Result<()> {
    if candidates.iter().all(|c| c.is_doc) {
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

    // Context terms: content tokens outside the current span.
    let mut context: Vec<String> = tokens
        .iter()
        .filter(|t| t.end <= span_start || t.start >= span_end)
        .filter(|t| !t.stopword && t.text.chars().count() >= MIN_SPAN_CHARS)
        .map(|t| t.text.to_lowercase())
        .collect();
    context.sort();
    context.dedup();
    if context.is_empty() {
        return Ok(());
    }

    // For each context term that is a compiled KG term, the set of column
    // node keys its col_affinity edges point at (≤ 4 per term by
    // construction — see stemma-kg's affinity pass).
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT cn.key FROM kg_nodes tn
         JOIN kg_edges e ON e.src = tn.id AND e.kind = 'col_affinity'
         JOIN kg_nodes cn ON cn.id = e.dst
         WHERE tn.kind = 'term' AND lower(tn.label) = ?1",
    )?;
    let mut supports: Vec<std::collections::HashSet<String>> = Vec::new();
    for term in &context {
        let cols: std::collections::HashSet<String> = stmt
            .query_map([term], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        if !cols.is_empty() {
            supports.push(cols);
        }
    }
    if supports.is_empty() {
        return Ok(());
    }

    let mut touched = false;
    for c in candidates.iter_mut().filter(|c| !c.is_doc) {
        let key = format!("column:{}.{}", c.table, c.column);
        let m = supports
            .iter()
            .filter(|cols| cols.contains(&key))
            .count()
            .min(CONTEXT_TERM_MAX);
        if m > 0 {
            let bonus = CONTEXT_TERM_BONUS * m as f64;
            c.score = c.score.max((c.score + bonus).min(COHERENCE_CAP));
            c.channels.push(ChannelScore {
                channel: "kg".into(),
                rank: 0,
                raw: bonus,
            });
            touched = true;
        }
    }
    if touched {
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
    Ok(())
}

/// Collective disambiguation (AIDA-lineage joint tuple scoring): the
/// associative mention — "Chen's team" — is unresolvable span by span when
/// there are two Chens, but the *pair* is: the right Chen is the one with a
/// path to the team. Candidate tuples across the provisional mentions are
/// scored as local score sum plus pairwise coherence, and the winning
/// tuple's connected candidates earn COHERENCE_BOOST with the connecting
/// path recorded as evidence. Coherence between two candidates requires a
/// schema path between their tables (fk/inferred_fk, ≤ MAX_PATH_HOPS) AND
/// an instance probe showing the two rows actually connect along it.
fn apply_collective_coherence(db: &StemmaDb, spans: &mut [Span]) -> Result<()> {
    use stemma_kg::KnowledgeStore;
    let has_kg: i64 = db
        .conn()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'kg_edges'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_kg == 0 {
        return Ok(());
    }

    // Provisional mentions: the same greedy walk select_mentions runs later,
    // without committing statuses. `selection_order` is strongest-first, so
    // truncation keeps the strongest mentions.
    let mut taken: Vec<(usize, usize)> = Vec::new();
    let mut winners: Vec<usize> = Vec::new();
    for i in selection_order(spans) {
        let (start, end) = (spans[i].start, spans[i].end);
        if taken.iter().any(|&(s, e)| start < e && s < end) {
            continue;
        }
        taken.push((start, end));
        winners.push(i);
    }
    winners.truncate(MAX_TUPLE_MENTIONS);
    if winners.len() < 2 {
        return Ok(());
    }
    winners.sort_by_key(|&i| spans[i].start); // evidence reads in query order

    let store = stemma_kg::SqliteKnowledgeStore::new(db)?;
    let ks: Vec<usize> = winners
        .iter()
        .map(|&i| spans[i].candidates.len().min(MAX_TUPLE_K))
        .collect();

    // Pairwise verification, cached: schema paths once per table pair, then
    // for each surviving candidate pair a LIMIT-1 probe per path until one
    // verifies. Everything downstream reads this map.
    let mut path_cache: std::collections::HashMap<(String, String), Vec<Vec<stemma_kg::PathHop>>> =
        std::collections::HashMap::new();
    let mut verified: std::collections::HashMap<(usize, usize, usize, usize), String> =
        std::collections::HashMap::new();
    for p in 0..winners.len() {
        for q in p + 1..winners.len() {
            for a in 0..ks[p] {
                for b in 0..ks[q] {
                    let ca = &spans[winners[p]].candidates[a];
                    let cb = &spans[winners[q]].candidates[b];
                    if ca.table == cb.table {
                        continue;
                    }
                    let key = (ca.table.clone(), cb.table.clone());
                    if !path_cache.contains_key(&key) {
                        let paths =
                            store.table_paths(&key.0, &key.1, MAX_PATH_HOPS, MAX_PATHS_PER_PAIR)?;
                        path_cache.insert(key.clone(), paths);
                    }
                    // An interpretation candidate stands for up to
                    // SAMPLE_ROWIDS concrete rows: probe the representative
                    // first and fall back to the remaining samples, since
                    // any row of the interpretation verifying the link
                    // verifies the reading. The probed rowids are the ones
                    // recorded in the evidence.
                    let ra = probe_rowids(ca);
                    let rb = probe_rowids(cb);
                    'paths: for path in &path_cache[&key] {
                        for &ar in &ra {
                            for &br in &rb {
                                if probe_instance_link(db, path, ar, br)? {
                                    verified.insert(
                                        (p, q, a, b),
                                        render_kg_path(path, &ca.table, ar, br),
                                    );
                                    break 'paths;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if verified.is_empty() {
        return Ok(());
    }

    // Exhaustive joint scoring: ≤ MAX_TUPLE_K^MAX_TUPLE_MENTIONS tuples,
    // each with ≤ C(MAX_TUPLE_MENTIONS, 2) map lookups — microseconds.
    let mut best: Option<(f64, Vec<usize>)> = None;
    let mut idx = vec![0usize; winners.len()];
    loop {
        let mut score = 0.0;
        for (m, &i) in winners.iter().enumerate() {
            score += spans[i].candidates[idx[m]].score;
        }
        for p in 0..winners.len() {
            for q in p + 1..winners.len() {
                if verified.contains_key(&(p, q, idx[p], idx[q])) {
                    score += COHERENCE_BOOST;
                }
            }
        }
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, idx.clone()));
        }
        let mut m = 0;
        while m < winners.len() {
            idx[m] += 1;
            if idx[m] < ks[m] {
                break;
            }
            idx[m] = 0;
            m += 1;
        }
        if m == winners.len() {
            break;
        }
    }
    let (_, tuple) = best.unwrap();

    // Boost the winning tuple's verified candidates (once each) and record
    // the path so the trajectory can show why the ordering changed.
    let mut touched: Vec<usize> = Vec::new();
    for p in 0..winners.len() {
        for q in p + 1..winners.len() {
            let Some(evidence) = verified.get(&(p, q, tuple[p], tuple[q])) else {
                continue;
            };
            for (m, ci) in [(p, tuple[p]), (q, tuple[q])] {
                let c = &mut spans[winners[m]].candidates[ci];
                if c.coherence.is_none() {
                    c.score = c.score.max((c.score + COHERENCE_BOOST).min(COHERENCE_CAP));
                    c.coherence = Some(evidence.clone());
                    touched.push(winners[m]);
                }
            }
        }
    }
    for i in touched {
        spans[i]
            .candidates
            .sort_by(|a, b| b.score.total_cmp(&a.score));
    }
    Ok(())
}

/// The rowids collective disambiguation may probe for a candidate: its
/// sample rowids (representative first), bounded at SAMPLE_ROWIDS by
/// construction; the bare representative when no samples were carried.
fn probe_rowids(c: &Candidate) -> Vec<i64> {
    if c.sample_rowids.is_empty() {
        vec![c.rowid]
    } else {
        c.sample_rowids.clone()
    }
}

/// Verifies that two concrete rows connect along `path` in the user
/// database: one LIMIT-1 join anchored by rowid at both ends, with the fk
/// columns taken from the compiled graph's edges.
fn probe_instance_link(
    db: &StemmaDb,
    path: &[stemma_kg::PathHop],
    from_rowid: i64,
    to_rowid: i64,
) -> Result<bool> {
    let Some(first) = path.first() else {
        return Ok(false);
    };
    let start = if first.forward {
        &first.src_table
    } else {
        &first.dst_table
    };
    let mut sql = format!("SELECT 1 FROM {}.\"{start}\" j0", stemmadb::SRC_SCHEMA);
    for (i, hop) in path.iter().enumerate() {
        let (next, cond) = if hop.forward {
            (
                &hop.dst_table,
                format!(
                    "j{i}.\"{}\" = j{}.\"{}\"",
                    hop.src_column,
                    i + 1,
                    hop.dst_column
                ),
            )
        } else {
            (
                &hop.src_table,
                format!(
                    "j{}.\"{}\" = j{i}.\"{}\"",
                    i + 1,
                    hop.src_column,
                    hop.dst_column
                ),
            )
        };
        sql.push_str(&format!(
            " JOIN {}.\"{next}\" j{} ON {cond}",
            stemmadb::SRC_SCHEMA,
            i + 1
        ));
    }
    sql.push_str(&format!(
        " WHERE j0.rowid = ?1 AND j{}.rowid = ?2 LIMIT 1",
        path.len()
    ));
    let mut stmt = db.conn().prepare(&sql)?;
    Ok(stmt.exists(stemmadb::rusqlite::params![from_rowid, to_rowid])?)
}

/// "people #2 ←lead_id— teams #43": the arrow points from referencing
/// column to referenced table regardless of traversal direction, with "?"
/// marking inferred (undeclared) joins. Intermediate tables carry no rowid —
/// the probe checked existence, not a specific connecting row.
fn render_kg_path(
    path: &[stemma_kg::PathHop],
    from_table: &str,
    from_rowid: i64,
    to_rowid: i64,
) -> String {
    let mut out = format!("{from_table} #{from_rowid}");
    for (i, hop) in path.iter().enumerate() {
        let next = if hop.forward {
            &hop.dst_table
        } else {
            &hop.src_table
        };
        let node = if i + 1 == path.len() {
            format!("{next} #{to_rowid}")
        } else {
            next.clone()
        };
        let q = if hop.inferred { "?" } else { "" };
        if hop.forward {
            out.push_str(&format!(" —{}{q}→ {node}", hop.src_column));
        } else {
            out.push_str(&format!(" ←{}{q}— {node}", hop.src_column));
        }
    }
    out
}

// ===========================================================================
// Context affinity over interpretation cards (vec_interp) — self-contained.
//
// Motivation: on relational corpora the dense channel is inert (nothing
// crosses DOC_MIN_LEN), and a value that appears in two columns — the same
// string as a city and as a product name — produces two lexically identical
// candidates fusion cannot order. The ingest layer embeds one interpretation
// card per distinct (table, column, value); the card carries the column's
// context, so conditioning on the FULL query separates the tie.
//
// Mechanics: for a span whose top two candidates are tied value
// interpretations, the query is embedded once (Embedder::format_query — the
// query side of the asymmetric scheme, rendered through the backend's own
// template) and the two cards' vectors are fetched directly
// from vec_interp by their provenance key — a plain filtered read, no KNN —
// with the cosine computed in-process, which is exact. Both candidates gain
// a "context" ChannelScore (rank by cosine order, raw = cosine), and the
// winner gets a bounded boost. Without an embedder, without vec_interp, on a
// registry model mismatch, or on any per-span lookup failure, the pass
// silently does nothing.
// ===========================================================================

/// Top-2 fused-score gap under which two value interpretations count as
/// tied. Same rationale as [`ADJUDICATION_MARGIN`]: a gap under 0.08 is what
/// fusion produces from roughly one rank inversion — noise, not evidence.
const CONTEXT_TIE_GAP: f64 = 0.08;
/// Minimum query-conditioned cosine gap between the two cards before the
/// winner is boosted. At d = 1024 the null sd of a pair cosine is
/// 1/√d ≈ 0.031, so 0.05 demands better than noise-level separation.
const CONTEXT_COS_GAP: f64 = 0.05;
/// Boost applied to the context winner: half the tie gap, so it can flip
/// only ties tighter than itself and never overturns genuine multi-channel
/// agreement.
const CONTEXT_BOOST: f64 = 0.04;
/// Context affinity never lifts a candidate into the exact band (0.9+): if
/// the user typed the stored value, they meant the stored value.
const CONTEXT_CAP: f64 = 0.9;

/// Separates spans whose top two candidates are tied value interpretations —
/// fused-score gap under [`CONTEXT_TIE_GAP`], both non-doc, distinct
/// (table, column) — by cosine between the full query's embedding and each
/// interpretation's card vector. Infallible by design: every missing signal
/// degrades to a no-op.
fn apply_context_affinity(
    db: &StemmaDb,
    embedder: Option<&dyn stemma_embed::Embedder>,
    query: &str,
    spans: &mut [Span],
) {
    let Some(embedder) = embedder else { return };
    let conn = db.conn();
    let has_interp: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'vec_interp'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_interp == 0 {
        return;
    }
    // Same-space discipline as the drain: a registry row naming a different
    // model makes the cosine meaningless — skip, don't guess.
    let registered: Option<String> = conn
        .query_row(
            "SELECT model FROM model_registry WHERE vector_table = 'vec_interp'",
            [],
            |r| r.get(0),
        )
        .ok();
    if registered.as_deref() != Some(embedder.identity().model.as_str()) {
        return;
    }

    // The query embedding is shared by every tied span; computed lazily so a
    // resolution with no ties costs no embedding call.
    let mut query_vec: Option<Vec<f32>> = None;
    for span in spans.iter_mut() {
        if span.status != "selected" || span.candidates.len() < 2 {
            continue;
        }
        let tied = {
            let (a, b) = (&span.candidates[0], &span.candidates[1]);
            !a.is_doc
                && !b.is_doc
                && (a.table != b.table || a.column != b.column)
                && a.score - b.score < CONTEXT_TIE_GAP
        };
        if !tied {
            continue;
        }
        let key = |c: &Candidate| (c.table.clone(), c.column.clone(), c.rowid);
        let (ka, kb) = (key(&span.candidates[0]), key(&span.candidates[1]));
        let Some(va) = interp_vector(db, &ka.0, &ka.1, ka.2) else {
            continue;
        };
        let Some(vb) = interp_vector(db, &kb.0, &kb.1, kb.2) else {
            continue;
        };
        if query_vec.is_none() {
            match embedder.embed(&[embedder.format_query(query)]) {
                Ok(mut v) if !v.is_empty() => query_vec = Some(v.remove(0)),
                _ => return, // embedder down: the whole pass degrades
            }
        }
        let q = query_vec.as_ref().unwrap();
        let (Some(cos_a), Some(cos_b)) = (cosine(q, &va), cosine(q, &vb)) else {
            continue;
        };
        let winner = if cos_a >= cos_b { 0 } else { 1 };
        for (i, cos) in [cos_a, cos_b].into_iter().enumerate() {
            span.candidates[i].channels.push(ChannelScore {
                channel: "context".into(),
                rank: usize::from(i != winner),
                raw: cos,
            });
        }
        if (cos_a - cos_b).abs() > CONTEXT_COS_GAP {
            let c = &mut span.candidates[winner];
            // Never reduce a score already above the cap (exact band).
            c.score = c.score.max((c.score + CONTEXT_BOOST).min(CONTEXT_CAP));
            span.candidates.sort_by(|x, y| y.score.total_cmp(&x.score));
        }
    }
}

/// The card vector for the interpretation a candidate cell belongs to. The
/// index keys interpretations by their representative MIN(src_rowid) over
/// rows sharing the value, so the candidate's rowid is first mapped to that
/// representative; the embedding is then read straight out of vec_interp by
/// provenance key (vec0 returns the stored blob on a plain filtered scan)
/// and decoded from little-endian f32s. None on any miss.
fn interp_vector(db: &StemmaDb, table: &str, column: &str, rowid: i64) -> Option<Vec<f32>> {
    let conn = db.conn();
    let rep: Option<i64> = conn
        .query_row(
            "SELECT MIN(l2.src_rowid) FROM lex_values l1
             JOIN lex_values l2
               ON l2.src_table = l1.src_table AND l2.src_column = l1.src_column
              AND l2.value_norm = l1.value_norm AND l2.is_doc = 0
             WHERE l1.src_table = ?1 AND l1.src_column = ?2 AND l1.src_rowid = ?3",
            stemmadb::rusqlite::params![table, column, rowid],
            |r| r.get(0),
        )
        .ok()?;
    let blob: Vec<u8> = conn
        .query_row(
            "SELECT embedding FROM vec_interp
             WHERE src_table = ?1 AND src_column = ?2 AND src_rowid = ?3",
            stemmadb::rusqlite::params![table, column, rep?],
            |r| r.get(0),
        )
        .ok()?;
    if blob.is_empty() || blob.len() % 4 != 0 {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Cosine over raw vectors, computed in-process — exact, with no unit-norm
/// assumption. None on dimension mismatch or a zero vector.
fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

// ======================= end context-affinity section ======================

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
        row_count: u32,
        sample_rowids: Vec<i64>,
    }
    // The candidate key for values is the interpretation: channels report
    // one hit per (table, column, normalized value) with the interpretation's
    // smallest rowid as representative, so grouping on (table, column,
    // representative rowid) is grouping on the reading, not on a cell.
    // Documents keep per-row identity, and their rowid is the row itself.
    let mut grouped: BTreeMap<(String, String, i64), Group> = BTreeMap::new();
    for h in hits {
        let entry = grouped
            .entry((h.table.clone(), h.column.clone(), h.rowid))
            .or_insert_with(|| Group {
                channels: Vec::new(),
                value: h.value.clone(),
                is_doc: h.is_doc,
                snippet: None,
                row_count: h.row_count,
                sample_rowids: h.sample_rowids.clone(),
            });
        entry.is_doc |= h.is_doc;
        if entry.snippet.is_none() {
            entry.snippet = h.snippet.clone();
        }
        entry.row_count = entry.row_count.max(h.row_count);
        if h.sample_rowids.len() > entry.sample_rowids.len() {
            entry.sample_rowids = h.sample_rowids.clone();
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
            let rrf: f64 = g
                .channels
                .iter()
                .map(|c| 1.0 / (RRF_K + c.rank as f64))
                .sum();
            // Normalize: three channels at rank 0 -> 1.0. Docs never have the
            // exact channel, but since dense landed they can still reach three
            // (bm25 + trigram + dense), so their base can saturate too.
            let base = (rrf / (3.0 / RRF_K)).min(1.0);
            let mut score = if has_exact {
                // Exact matches are definitionally right about the value.
                (0.9 + 0.1 * base).min(1.0)
            } else if g.is_doc {
                // A mention resolves *into* a document; punishing the doc for
                // its length would break retrieval (the careg failure mode).
                (base * 0.85).min(0.85)
            } else {
                let affinity = (span_len / (g.value.chars().count() as f64).max(span_len)).sqrt();
                (base * (0.4 + 0.6 * affinity)).min(1.0)
            };
            // The dense channel's cosine is absolute evidence, not a rank:
            // calibrate it to the score scale and let it floor the fusion —
            // a 0.6 cosine match must survive having no lexical company.
            if let Some(best_cos) = g
                .channels
                .iter()
                .filter(|c| c.channel == "dense")
                .map(|c| c.raw)
                .fold(None::<f64>, |m, x| Some(m.map_or(x, |m| m.max(x))))
            {
                let calibrated = (((best_cos - 0.30) / 0.30).clamp(0.0, 1.0)) * 0.78;
                score = score.max(calibrated);
            }
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
                adjudicated: false,
                coherence: None,
                row_count: g.row_count.max(1),
                sample_rowids: if g.sample_rowids.is_empty() {
                    vec![rowid]
                } else {
                    g.sample_rowids
                },
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

/// Selection priority shared by collective disambiguation (provisional
/// mentions) and final selection: strongest candidate first; KG-entity
/// spans get a nudge (a compiled phrase is better evidence of mention-hood
/// than raw match strength); longer span wins ties (more specific).
fn selection_order(spans: &[Span]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..spans.len())
        .filter(|&i| spans[i].status == "selected")
        .collect();
    order.sort_by(|&a, &b| {
        let key = |i: usize| {
            let s = spans[i].candidates.first().map(|c| c.score).unwrap_or(0.0);
            if spans[i].kg_alias {
                s * 1.08
            } else {
                s
            }
        };
        key(b)
            .total_cmp(&key(a))
            .then((spans[b].end - spans[b].start).cmp(&(spans[a].end - spans[a].start)))
    });
    order
}

/// Greedy non-overlapping selection: strongest span wins its byte range;
/// overlapping spans are marked "overlapped". Within a selected span, top-k
/// candidates above threshold are selected, the rest annotated.
fn select_mentions(spans: &mut [Span]) -> Vec<usize> {
    let order = selection_order(spans);
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
                // Selection only picks "selected" spans, so a weak span here
                // can only mean the adjudication band answered NIL — the
                // affirmative no-record-matches conclusion.
                nil: s.status == "weak",
                ambiguous: s.ambiguous,
                readings: if s.ambiguous {
                    let top_score = s
                        .candidates
                        .iter()
                        .find(|c| c.selected)
                        .map(|c| c.score)
                        .unwrap_or(0.0);
                    s.candidates
                        .iter()
                        .filter(|c| c.selected && top_score - c.score < ADJUDICATION_MARGIN)
                        .take(4)
                        .map(|c| pb::Reading {
                            table: c.table.clone(),
                            column: c.column.clone(),
                            value: c.value.clone(),
                            row_count: c.row_count,
                            rowid: c.rowid,
                        })
                        .collect()
                } else {
                    Vec::new()
                },
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
                ambiguous: s.ambiguous,
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
                        adjudicated: c.adjudicated,
                        coherence: c.coherence.clone().unwrap_or_default(),
                        row_count: c.row_count,
                        sample_rowids: c.sample_rowids.clone(),
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
            .all(|c| !c.selected && c.reject_reason.as_deref() == Some("span_not_selected")));
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

    /// Builds a scratch user DB from the given SQL batch and ingests it.
    fn custom_db(tag: &str, ddl: &str) -> StemmaDb {
        let dir = std::env::temp_dir().join(format!("stemma-resolve-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let store = dir.join("user.stemmadb");
        let _ = std::fs::remove_file(&user);
        let _ = std::fs::remove_file(&store);
        {
            let c = stemmadb::rusqlite::Connection::open(&user).unwrap();
            c.execute_batch(ddl).unwrap();
        }
        let db = StemmaDb::open(&store, &user).unwrap();
        stemma_ingest::build_lexical_index(&db, false).unwrap();
        db
    }

    #[test]
    fn duplicate_rows_do_not_hide_rival_interpretations() {
        // The issue #1 repro, verbatim: "Ellis" is both a brand and a
        // surname. 40 brand rows share the value, 5 people rows share it,
        // and both tables carry filler so neither is degenerate. Before
        // interpretation candidacy, the 40 brand rows consumed every
        // candidate slot and people.surname never appeared at all.
        let mut ddl = String::from(
            "CREATE TABLE brands (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE people (id INTEGER PRIMARY KEY, surname TEXT NOT NULL);",
        );
        for _ in 0..40 {
            ddl.push_str("INSERT INTO brands (name) VALUES ('Ellis');");
        }
        for filler in ["Acme", "Zenith", "Northwind"] {
            ddl.push_str(&format!("INSERT INTO brands (name) VALUES ('{filler}');"));
        }
        for _ in 0..5 {
            ddl.push_str("INSERT INTO people (surname) VALUES ('Ellis');");
        }
        for filler in ["Okafor", "Natarajan", "Silva"] {
            ddl.push_str(&format!("INSERT INTO people (surname) VALUES ('{filler}');"));
        }
        let db = custom_db("ellis", &ddl);

        let trace = resolve_lexical(&db, "Ellis").unwrap();
        let span = trace
            .mentions
            .iter()
            .map(|&i| &trace.spans[i])
            .find(|s| s.text == "Ellis")
            .expect("Ellis mention");
        for c in &span.candidates {
            println!(
                "{}.{} #{} '{}' score={:.3} row_count={} samples={:?} selected={}",
                c.table, c.column, c.rowid, c.value, c.score, c.row_count,
                c.sample_rowids, c.selected
            );
        }

        // One candidate per interpretation — duplicate rows collapsed.
        let brands: Vec<_> = span.candidates.iter().filter(|c| c.table == "brands").collect();
        let people: Vec<_> = span.candidates.iter().filter(|c| c.table == "people").collect();
        assert_eq!(brands.len(), 1, "one candidate per interpretation");
        assert_eq!(people.len(), 1, "the rival reading must surface");
        let (brand, person) = (brands[0], people[0]);

        // Interpretation bookkeeping: row counts, representative rowid,
        // bounded ascending samples led by the representative.
        assert_eq!(brand.row_count, 40);
        assert_eq!(person.row_count, 5);
        for c in [brand, person] {
            assert_eq!(c.sample_rowids[0], c.rowid);
            assert!(c.sample_rowids.len() <= 3);
            assert!(c.sample_rowids.windows(2).all(|w| w[0] < w[1]));
        }

        // Equal evidence, equal score: both interpretations enter the exact
        // channel at rank 0 — no fabricated decay over identical values.
        for c in [brand, person] {
            let exact = c
                .channels
                .iter()
                .find(|ch| ch.channel == "exact")
                .expect("exact channel");
            assert_eq!(exact.rank, 0);
            assert_eq!(exact.raw, 1.0);
        }
        assert_eq!(brand.score, person.score, "identical evidence, identical score");

        // TOP_K selection carries both readings.
        assert!(brand.selected && person.selected);

        // And the interpretation fields survive into the Explain proto.
        let explain = trace_to_explain_proto(&trace);
        let pb_span = &explain.spans[span.id];
        let pb_brand = pb_span
            .candidates
            .iter()
            .find(|c| c.table == "brands")
            .unwrap();
        assert_eq!(pb_brand.row_count, 40);
        assert_eq!(pb_brand.sample_rowids.len(), 3);
        assert_eq!(pb_brand.sample_rowids[0], pb_brand.rowid);
    }

    #[test]
    fn doc_quota_keeps_rival_tables_in_the_channel() {
        // A document table with many strong matches must not starve another
        // document table out of the FTS channel budget.
        let pad = "Further procedural language follows to reach the document \
                   classification threshold with room to spare in every row. "
            .repeat(3);
        let mut ddl = String::from(
            "CREATE TABLE manuals (id INTEGER PRIMARY KEY, body TEXT);
             CREATE TABLE memos (id INTEGER PRIMARY KEY, body TEXT);",
        );
        for i in 0..10 {
            ddl.push_str(&format!(
                "INSERT INTO manuals (body) VALUES ('Turbine maintenance step {i}: {pad}');"
            ));
        }
        for i in 0..2 {
            ddl.push_str(&format!(
                "INSERT INTO memos (body) VALUES ('Turbine budget note {i}: {pad}');"
            ));
        }
        let db = custom_db("quota", &ddl);
        let trace = resolve_lexical(&db, "turbine").unwrap();
        let span = trace
            .mentions
            .iter()
            .map(|&i| &trace.spans[i])
            .find(|s| s.text == "turbine")
            .expect("turbine mention");
        let tables: std::collections::HashSet<&str> =
            span.candidates.iter().map(|c| c.table.as_str()).collect();
        assert!(
            tables.contains("memos"),
            "the smaller table must survive the channel budget, got {tables:?}"
        );
        let manuals = span.candidates.iter().filter(|c| c.table == "manuals").count();
        assert!(manuals <= DOC_COLUMN_QUOTA, "per-column quota holds");
    }

    /// Deterministic embedder for the dense channel: each text hashes to a
    /// unit vector, so identical texts embed identically and distinct texts
    /// land apart — the exact geometry that makes duplicate rows adjacent
    /// in a KNN.
    struct HashEmbedder;

    impl HashEmbedder {
        const DIM: usize = 8;
        fn vector(text: &str) -> Vec<f32> {
            let mut state: u64 = 0xcbf29ce484222325;
            for b in text.bytes() {
                state ^= b as u64;
                state = state.wrapping_mul(0x100000001b3);
            }
            let mut z = state;
            let mut v: Vec<f32> = (0..Self::DIM)
                .map(|_| {
                    z = z.wrapping_add(0x9e3779b97f4a7c15);
                    let mut x = z;
                    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                    x ^= x >> 31;
                    (x as f64 / u64::MAX as f64) as f32 - 0.5
                })
                .collect();
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= n);
            v
        }
    }

    impl stemma_embed::Embedder for HashEmbedder {
        fn embed(&self, texts: &[String]) -> stemma_embed::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| Self::vector(t)).collect())
        }
        fn identity(&self) -> stemma_embed::ModelIdentity {
            stemma_embed::ModelIdentity {
                backend: "fake".into(),
                model: "hash-embedder".into(),
                dimension: Self::DIM,
                query_template: String::new(),
            }
        }
    }

    #[test]
    fn dense_channel_collapses_duplicate_documents() {
        // Issue #3: a fact table repeating one document string across many
        // rows made every copy a separate KNN hit — identical text, identical
        // vector, adjacent in the ordering — so with k = PER_CHANNEL_LIMIT
        // the copies consumed all eight slots and the corpus's other
        // documents were unreachable through the channel. Over-fetch plus
        // collapse must return the repeated string ONCE, with row_count
        // carrying the fan-out, and surface the previously crowded-out rest.
        let pad = "Additional descriptive language follows so every record \
                   clears the document classification threshold comfortably. "
            .repeat(3);
        let repeated =
            format!("PT903W Womens Cut Single Ply Light Weight Track Singlet. {pad}");
        let others = [
            format!("Marathon foam trainer with recycled mesh upper. {pad}"),
            format!("Alpine down parka rated for deep winter conditions. {pad}"),
            format!("Trail running vest with soft flask pockets. {pad}"),
        ];
        let mut ddl =
            String::from("CREATE TABLE inventory_items (id INTEGER PRIMARY KEY, product_name TEXT);");
        for _ in 0..8 {
            ddl.push_str(&format!(
                "INSERT INTO inventory_items (product_name) VALUES ('{repeated}');"
            ));
        }
        for o in &others {
            ddl.push_str(&format!(
                "INSERT INTO inventory_items (product_name) VALUES ('{o}');"
            ));
        }
        let db = custom_db("densedup", &ddl);
        stemma_ingest::enqueue_missing_embeddings(&db).unwrap();
        stemma_ingest::drain_embed_queue(&db, &HashEmbedder, stemma_ingest::EMBED_BATCH).unwrap();
        let vectors: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM vec_dense", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 11, "8 copies + 3 distinct documents");

        let hits = dense_hits(&db, &HashEmbedder::vector(&repeated)).unwrap();
        for h in &hits {
            println!(
                "{}.{} #{} rank={} cos={:.4} row_count={} samples={:?} '{}…'",
                h.table, h.column, h.rowid, h.rank, h.raw, h.row_count,
                h.sample_rowids, &h.value[..24]
            );
        }
        // One hit per interpretation, not per row.
        assert_eq!(hits.len(), 4, "4 distinct documents, 4 candidates");
        let top = &hits[0];
        assert_eq!(top.value, repeated);
        assert_eq!(top.rank, 0);
        assert!(top.raw > 0.999, "query text is the stored text");
        // The collapse counts copies the same way the lexical GROUP BY does.
        assert_eq!(top.row_count, 8, "row_count carries the fan-out");
        assert_eq!(top.sample_rowids.len(), SAMPLE_ROWIDS);
        assert_eq!(top.sample_rowids[0], top.rowid, "representative leads");
        assert!(top.sample_rowids.windows(2).skip(1).all(|w| w[0] < w[1]));
        // The previously crowded-out documents surface, one hit each.
        for o in &others {
            let hit = hits
                .iter()
                .find(|h| h.value == *o)
                .expect("every distinct document reachable through the channel");
            assert_eq!(hit.row_count, 1);
        }
        // The per-column quota bounds the collapsed set like the FTS window.
        assert!(hits.len() <= DOC_COLUMN_QUOTA.max(PER_CHANNEL_LIMIT));
    }

    #[test]
    fn collective_disambiguation_prefers_connected_chen() {
        // The associative-mention case: "Chen" alone matches both Wei Chen
        // and Dana Chen, and length affinity ranks Wei first. Only the
        // knowledge graph plus the data connect Dana to the Billing team.
        let db = readme_db("collective");
        stemma_kg::compile(&db, false).unwrap();
        let trace = resolve_lexical(&db, "what did Chen's Billing team ship").unwrap();
        let chen = trace
            .mentions
            .iter()
            .map(|&i| &trace.spans[i])
            .find(|s| s.text == "Chen")
            .expect("Chen mention");
        let top = &chen.candidates[0];
        assert!(
            top.value.contains("Dana"),
            "the Billing-connected Chen must win, got {:?}",
            chen.candidates
                .iter()
                .map(|c| (c.value.as_str(), c.score))
                .collect::<Vec<_>>()
        );
        assert!(top.selected);
        let evidence = top.coherence.as_deref().expect("coherence evidence");
        assert!(
            evidence.contains("people #2") && evidence.contains("teams #43"),
            "got {evidence:?}"
        );
        // The rival Chen stays visible, unboosted, and now outranked.
        let wei = chen
            .candidates
            .iter()
            .find(|c| c.value.contains("Wei"))
            .expect("Wei Chen near-miss");
        assert!(wei.coherence.is_none());
        assert!(wei.score < top.score);
        // The partner mention carries the same evidence.
        let billing = trace
            .mentions
            .iter()
            .map(|&i| &trace.spans[i])
            .find(|s| s.text == "Billing")
            .expect("Billing mention");
        assert_eq!(billing.candidates[0].coherence.as_deref(), Some(evidence));
    }

    /// The context-coherence fixture: the value 'Atlas Freight' lives in
    /// two columns, and a document corpus gives the compiler a term
    /// ("cargo") whose content affinity points at exactly one of them
    /// (vendors.name, via the recurring Cargo* values).
    fn context_db(tag: &str) -> StemmaDb {
        let pad = "Additional routine language follows so each record clears \
                   the document classification threshold comfortably. "
            .repeat(3);
        let themes = [
            "cargo manifest freight cargo manifest hold",
            "invoice ledger balance invoice ledger audit",
            "harbor berth channel harbor berth tide",
            "diesel engine piston diesel engine torque",
            "quota tariff duty quota tariff customs",
            "crane gantry hoist crane gantry winch",
        ];
        let mut ddl = String::from(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
             CREATE TABLE clients (id INTEGER PRIMARY KEY, company TEXT NOT NULL);
             CREATE TABLE vendors (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
        );
        for i in 0..30 {
            let theme = themes[i % 6];
            ddl.push_str(&format!(
                "INSERT INTO notes (body) VALUES ('whereof the {theme} whereof provisions apply {theme} whereof. {pad}');"
            ));
        }
        // The shared value, plus the recurring Cargo* values that give the
        // term its affinity to vendors.name — and none to clients.company.
        ddl.push_str(
            "INSERT INTO clients (company) VALUES
                 ('Atlas Freight'), ('Beacon Mills'), ('Coral Imports');
             INSERT INTO vendors (name) VALUES
                 ('Atlas Freight'), ('Cargo Line'), ('Cargo Express'), ('Delta Supply');",
        );
        custom_db(tag, &ddl)
    }

    #[test]
    fn context_term_affinity_flips_value_interpretations() {
        let db = context_db("ctxflip");
        stemma_kg::compile(&db, false).unwrap();
        let trace = resolve_lexical(&db, "cargo Atlas").unwrap();
        let span = trace
            .spans
            .iter()
            .find(|s| s.text == "Atlas")
            .expect("Atlas span");
        for c in &span.candidates {
            println!(
                "{}.{} '{}' score={:.3} channels={:?}",
                c.table, c.column, c.value, c.score,
                c.channels.iter().map(|ch| (ch.channel.as_str(), ch.rank, ch.raw)).collect::<Vec<_>>()
            );
        }
        let vendor = span.candidates.iter().find(|c| c.table == "vendors").unwrap();
        let client = span.candidates.iter().find(|c| c.table == "clients").unwrap();
        // Identical lexical evidence — the flip is the context term's doing,
        // and it is recorded as a "kg" channel entry carrying the bonus.
        let kg = vendor
            .channels
            .iter()
            .find(|ch| ch.channel == "kg")
            .expect("kg channel on the supported interpretation");
        assert!((kg.raw - CONTEXT_TERM_BONUS).abs() < 1e-9);
        assert!(client.channels.iter().all(|ch| ch.channel != "kg"));
        assert!(
            (vendor.score - client.score - CONTEXT_TERM_BONUS).abs() < 1e-9,
            "vendor {} vs client {}",
            vendor.score,
            client.score
        );
        assert_eq!(
            span.candidates[0].table, "vendors",
            "the context-supported interpretation must rank first"
        );
    }

    #[test]
    fn neutral_context_earns_no_bonus() {
        let db = context_db("ctxneutral");
        stemma_kg::compile(&db, false).unwrap();
        // "diesel" is a compiled term of the corpus, but it has no affinity
        // to either column holding 'Atlas Freight' — and a bare mention has
        // no context at all. Neither may move the ordering.
        for query in ["Atlas", "diesel Atlas"] {
            let trace = resolve_lexical(&db, query).unwrap();
            let span = trace
                .spans
                .iter()
                .find(|s| s.text == "Atlas")
                .expect("Atlas span");
            let vendor = span.candidates.iter().find(|c| c.table == "vendors").unwrap();
            let client = span.candidates.iter().find(|c| c.table == "clients").unwrap();
            assert_eq!(vendor.score, client.score, "query {query:?}");
            assert!(
                span.candidates
                    .iter()
                    .all(|c| c.channels.iter().all(|ch| ch.channel != "kg")),
                "no kg evidence without supporting context, query {query:?}"
            );
        }
    }

    #[test]
    fn without_kg_the_lexical_chen_wins() {
        // The ordering collective disambiguation exists to overturn: with no
        // compiled graph, length affinity puts Wei Chen first and nothing
        // carries coherence evidence.
        let db = readme_db("nokgchen");
        let trace = resolve_lexical(&db, "what did Chen's Billing team ship").unwrap();
        let chen = trace
            .mentions
            .iter()
            .map(|&i| &trace.spans[i])
            .find(|s| s.text == "Chen")
            .expect("Chen mention");
        assert!(chen.candidates[0].value.contains("Wei"));
        assert!(trace
            .spans
            .iter()
            .flat_map(|s| &s.candidates)
            .all(|c| c.coherence.is_none()));
    }

    #[test]
    fn coherence_evidence_reaches_trace_and_proto() {
        let db = readme_db("cohproto");
        stemma_kg::compile(&db, false).unwrap();
        let trace = resolve_lexical(&db, "what did Chen's Billing team ship").unwrap();
        let json = serde_json::to_value(&trace).unwrap();
        let in_json = json["spans"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["candidates"].as_array().unwrap())
            .any(|c| {
                c["coherence"]
                    .as_str()
                    .is_some_and(|p| p.contains("teams #43"))
            });
        assert!(in_json, "coherence must serialize in the JSON trace");
        let explain = trace_to_explain_proto(&trace);
        assert!(explain
            .spans
            .iter()
            .flat_map(|s| &s.candidates)
            .any(|c| c.coherence.contains("teams #43")));
    }

    #[test]
    fn missing_index_is_a_clear_error() {
        let db = StemmaDb::open_in_memory().unwrap();
        match resolve_lexical(&db, "anything") {
            Err(Error::IndexMissing) => {}
            other => panic!("expected IndexMissing, got {other:?}"),
        }
    }

    /// In-crate fake LM: a canned reply (or a canned failure) plus a call
    /// counter, which is all the trait demands.
    struct FakeLm {
        reply: Option<String>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeLm {
        fn replying(reply: &str) -> Self {
            Self {
                reply: Some(reply.to_string()),
                calls: 0.into(),
            }
        }
        fn failing() -> Self {
            Self {
                reply: None,
                calls: 0.into(),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl stemma_lm::LmBackend for FakeLm {
        fn chat(
            &self,
            _messages: &[stemma_lm::ChatMessage],
            _schema: Option<&serde_json::Value>,
        ) -> stemma_lm::Result<String> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match &self.reply {
                Some(r) => Ok(r.clone()),
                None => Err(stemma_lm::Error::Http("fake endpoint down".into())),
            }
        }
        fn native_structured_output(&self) -> bool {
            true
        }
        fn identity(&self) -> stemma_lm::LmIdentity {
            stemma_lm::LmIdentity {
                backend: "fake".into(),
                model: "fake".into(),
            }
        }
    }

    fn cand(rowid: i64, value: &str, score: f64, channels: &[&str]) -> Candidate {
        Candidate {
            table: "t".into(),
            column: "c".into(),
            rowid,
            value: value.into(),
            value_truncated: false,
            score,
            channels: channels
                .iter()
                .map(|ch| ChannelScore {
                    channel: ch.to_string(),
                    rank: 0,
                    raw: 1.0,
                })
                .collect(),
            selected: true,
            reject_reason: None,
            is_doc: false,
            snippet: None,
            adjudicated: false,
            coherence: None,
            row_count: 1,
            sample_rowids: vec![rowid],
        }
    }

    /// One mention whose top two candidates sit inside the margin with no
    /// exact hit — the ambiguous band by construction.
    fn ambiguous_trace() -> Trace {
        Trace {
            query: "q".into(),
            tokens: Vec::new(),
            spans: vec![Span {
                id: 0,
                text: "q".into(),
                start: 0,
                end: 1,
                status: "selected".into(),
                candidates: vec![
                    cand(1, "alpha", 0.60, &["bm25"]),
                    cand(2, "beta", 0.55, &["trigram"]),
                ],
                kg_alias: false,
                ambiguous: false,
            }],
            mentions: vec![0],
            elapsed_ms: 0.0,
        }
    }

    fn cand_at(table: &str, column: &str, rowid: i64, score: f64) -> Candidate {
        let mut c = cand(rowid, "Ellis", score, &["bm25", "trigram"]);
        c.table = table.into();
        c.column = column.into();
        c
    }

    /// Cross-interpretation tie, escalation exhausted: the span is marked
    /// ambiguous — the resolution's honest answer is a question.
    #[test]
    fn unresolved_cross_interpretation_ties_are_marked_ambiguous() {
        let mut trace = ambiguous_trace();
        trace.spans[0].candidates = vec![
            cand_at("brands", "name", 1, 0.60),
            cand_at("people", "surname", 2, 0.58),
        ];
        mark_ambiguous(&mut trace);
        assert!(trace.spans[0].ambiguous);
    }

    /// Two rows of ONE reading are the same answer, not ambiguity.
    #[test]
    fn same_interpretation_ties_are_not_ambiguous() {
        let mut trace = ambiguous_trace();
        trace.spans[0].candidates = vec![
            cand_at("brands", "name", 1, 0.60),
            cand_at("brands", "name", 2, 0.58),
        ];
        mark_ambiguous(&mut trace);
        assert!(!trace.spans[0].ambiguous);
    }

    /// A settled adjudication is a decision; ambiguity marking respects it.
    #[test]
    fn adjudicated_choice_suppresses_ambiguity() {
        let mut trace = ambiguous_trace();
        trace.spans[0].candidates = vec![
            cand_at("brands", "name", 1, 0.60),
            cand_at("people", "surname", 2, 0.58),
        ];
        let lm = FakeLm::replying(r#"{"choice": "1"}"#);
        adjudicate(&mut trace, &lm);
        mark_ambiguous(&mut trace);
        assert!(!trace.spans[0].ambiguous);
        assert!(trace.spans[0].candidates[0].adjudicated);
    }

    /// The third verdict: the LM may answer "ambiguous", which routes to the
    /// ask-back path without reordering anything.
    #[test]
    fn lm_ambiguous_verdict_marks_the_span() {
        let mut trace = ambiguous_trace();
        trace.spans[0].candidates = vec![
            cand_at("brands", "name", 1, 0.60),
            cand_at("people", "surname", 2, 0.58),
        ];
        let lm = FakeLm::replying(r#"{"choice": "ambiguous"}"#);
        adjudicate(&mut trace, &lm);
        assert!(trace.spans[0].ambiguous);
        assert_eq!(trace.spans[0].candidates[0].rowid, 1, "order untouched");
        mark_ambiguous(&mut trace);
        assert!(trace.spans[0].ambiguous);
    }

    /// Issue #1's flagship: two EXACT readings of the same string. Exactness
    /// settles nothing here — the tie routes and, unresolved, is ambiguous.
    #[test]
    fn exact_cross_interpretation_ties_route_and_mark() {
        let mut trace = ambiguous_trace();
        let mut a = cand_at("brands", "name", 1, 1.0);
        a.channels.push(ChannelScore { channel: "exact".into(), rank: 0, raw: 1.0 });
        let mut b = cand_at("people", "surname", 2, 1.0);
        b.channels.push(ChannelScore { channel: "exact".into(), rank: 0, raw: 1.0 });
        trace.spans[0].candidates = vec![a, b];
        assert!(is_ambiguous(&trace.spans[0]), "exact-vs-exact distinct readings route");
        mark_ambiguous(&mut trace);
        assert!(trace.spans[0].ambiguous);
    }

    #[test]
    fn adjudication_reorders_on_choice() {
        let mut trace = ambiguous_trace();
        let lm = FakeLm::replying(r#"{"choice": "1"}"#);
        adjudicate(&mut trace, &lm);
        assert_eq!(lm.calls(), 1);
        let span = &trace.spans[0];
        assert_eq!(span.status, "selected");
        assert_eq!(
            span.candidates[0].rowid, 2,
            "chosen candidate moves to front"
        );
        assert!(span.candidates[0].adjudicated);
        assert!(span.candidates[0].selected);
        assert_eq!(
            span.candidates[1].rowid, 1,
            "displaced candidate stays visible"
        );
        assert!(!span.candidates[1].adjudicated);
    }

    #[test]
    fn adjudication_nil_demotes_to_weak() {
        let mut trace = ambiguous_trace();
        let lm = FakeLm::replying(r#"{"choice": "nil"}"#);
        adjudicate(&mut trace, &lm);
        assert_eq!(lm.calls(), 1);
        assert_eq!(trace.spans[0].status, "weak");
        assert!(trace.spans[0].candidates.iter().all(|c| !c.adjudicated));
    }

    #[test]
    fn unambiguous_mentions_never_invoke_the_lm() {
        // An exact-channel winner is outside the band even when close...
        let mut exact = ambiguous_trace();
        exact.spans[0].candidates[0].channels[0].channel = "exact".into();
        // ...and so is a clear fused-score gap.
        let mut gapped = ambiguous_trace();
        gapped.spans[0].candidates[1].score = 0.40;
        // ...and a single-candidate mention has nothing to adjudicate.
        let mut single = ambiguous_trace();
        single.spans[0].candidates.truncate(1);
        let lm = FakeLm::replying(r#"{"choice": "0"}"#);
        adjudicate(&mut exact, &lm);
        adjudicate(&mut gapped, &lm);
        adjudicate(&mut single, &lm);
        assert_eq!(lm.calls(), 0);
        assert_eq!(exact.spans[0].candidates[0].rowid, 1);
        assert_eq!(exact.spans[0].status, "selected");
    }

    #[test]
    fn lm_failure_degrades_to_unadjudicated_trace() {
        let db = readme_db("lmdown");
        let plain = resolve(&db, "what did Wei Chen ship", None).unwrap();
        let lm = FakeLm::failing();
        let full = resolve_full(&db, "what did Wei Chen ship", None, Some(&lm)).unwrap();
        assert_eq!(
            serde_json::to_value(&plain.spans).unwrap(),
            serde_json::to_value(&full.spans).unwrap(),
            "a down LM must be a no-op"
        );
        assert_eq!(plain.mentions, full.mentions);
        // A malformed verdict is equally a no-op.
        let mut trace = ambiguous_trace();
        let lm = FakeLm::replying(r#"{"choice": "9"}"#);
        adjudicate(&mut trace, &lm);
        assert_eq!(trace.spans[0].candidates[0].rowid, 1);
        assert_eq!(trace.spans[0].status, "selected");
    }

    // ------------------- context-affinity section tests -------------------

    /// Test embedder with readable geometry: each marker word contributes an
    /// axis plus a shared bias axis, so the cosine ordering between a query
    /// and two interpretation cards is decided by which markers they share —
    /// deterministic and directionally meaningful, unlike a hash embedder.
    struct MarkerEmbedder;

    const MARKERS: &[&str] = &["office", "tower", "product", "sku"];

    impl MarkerEmbedder {
        fn vector(text: &str) -> Vec<f32> {
            let t = text.to_lowercase();
            let mut v: Vec<f32> = MARKERS
                .iter()
                .map(|m| if t.contains(m) { 1.0 } else { 0.0 })
                .collect();
            v.push(0.25); // bias axis: zero-marker texts stay embeddable
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= n);
            v
        }
    }

    impl stemma_embed::Embedder for MarkerEmbedder {
        fn embed(&self, texts: &[String]) -> stemma_embed::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| Self::vector(t)).collect())
        }
        fn identity(&self) -> stemma_embed::ModelIdentity {
            stemma_embed::ModelIdentity {
                backend: "fake".into(),
                model: "marker-embedder".into(),
                dimension: MARKERS.len() + 1,
                query_template: String::new(),
            }
        }
    }

    /// The BIRD-shaped tie in miniature: the same value 'Mercury' lives in
    /// offices.city and products.name, and only column context separates the
    /// two interpretations. `drain` populates vec_interp from real cards.
    fn interp_db(drain: bool) -> StemmaDb {
        let db = StemmaDb::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE src.offices(id INTEGER PRIMARY KEY, name TEXT, city TEXT);
                 INSERT INTO src.offices VALUES
                    (1, 'Mercury Tower', 'Mercury'),
                    (2, 'Vine Street', 'Fresno');
                 CREATE TABLE src.products(id INTEGER PRIMARY KEY, sku TEXT, name TEXT);
                 INSERT INTO src.products VALUES
                    (1, 'SKU-771', 'Mercury'),
                    (2, 'SKU-772', 'Saturn');",
            )
            .unwrap();
        stemma_ingest::build_lexical_index(&db, false).unwrap();
        if drain {
            stemma_ingest::enqueue_missing_interpretations(&db).unwrap();
            stemma_ingest::drain_embed_queue(&db, &MarkerEmbedder, stemma_ingest::EMBED_BATCH)
                .unwrap();
        }
        db
    }

    fn interp_cand(table: &str, column: &str, rowid: i64, score: f64) -> Candidate {
        Candidate {
            table: table.into(),
            column: column.into(),
            rowid,
            value: "Mercury".into(),
            value_truncated: false,
            score,
            channels: vec![ChannelScore {
                channel: "trigram".into(),
                rank: 0,
                raw: 1.0,
            }],
            selected: false,
            reject_reason: None,
            is_doc: false,
            snippet: None,
            adjudicated: false,
            coherence: None,
            row_count: 1,
            sample_rowids: vec![rowid],
        }
    }

    /// A span whose top two candidates are the two 'Mercury' interpretations,
    /// with the wrong one (for a product query) narrowly on top.
    fn tied_spans() -> Vec<Span> {
        vec![Span {
            id: 0,
            text: "Mercury".into(),
            start: 0,
            end: 7,
            status: "selected".into(),
            ambiguous: false,
            candidates: vec![
                interp_cand("offices", "city", 1, 0.60),
                interp_cand("products", "name", 1, 0.58),
            ],
            kg_alias: false,
        }]
    }

    #[test]
    fn context_affinity_separates_a_tie_in_the_right_direction() {
        let db = interp_db(true);
        let mut spans = tied_spans();
        // The query talks about products; the products card shares its
        // markers ("product", "sku"), the offices card does not.
        apply_context_affinity(
            &db,
            Some(&MarkerEmbedder),
            "which product sku is Mercury",
            &mut spans,
        );
        let span = &spans[0];
        assert_eq!(
            (span.candidates[0].table.as_str(), span.candidates[0].rowid),
            ("products", 1),
            "the context-matching interpretation must win: {:?}",
            span.candidates
                .iter()
                .map(|c| (c.table.as_str(), c.score))
                .collect::<Vec<_>>()
        );
        // Both tied candidates carry the evidence, ranked by cosine order.
        for c in &span.candidates {
            let ctx = c
                .channels
                .iter()
                .find(|ch| ch.channel == "context")
                .expect("both tied candidates carry a context ChannelScore");
            assert_eq!(ctx.rank, usize::from(c.table != "products"));
            assert!((-1.0..=1.0).contains(&ctx.raw));
        }
        let winner = &span.candidates[0];
        let loser = &span.candidates[1];
        assert!((winner.score - 0.62).abs() < 1e-9, "0.58 + CONTEXT_BOOST");
        assert!((loser.score - 0.60).abs() < 1e-9, "loser untouched");
        // The reorder is bounded: the winner never enters the exact band.
        assert!(winner.score <= CONTEXT_CAP);
    }

    #[test]
    fn context_affinity_is_a_noop_without_signals() {
        let baseline = serde_json::to_value(tied_spans()).unwrap();

        // No embedder.
        let db = interp_db(true);
        let mut spans = tied_spans();
        apply_context_affinity(&db, None, "which product sku is Mercury", &mut spans);
        assert_eq!(serde_json::to_value(&spans).unwrap(), baseline);

        // No vec_interp (queue never drained).
        let db = interp_db(false);
        let mut spans = tied_spans();
        apply_context_affinity(
            &db,
            Some(&MarkerEmbedder),
            "which product sku is Mercury",
            &mut spans,
        );
        assert_eq!(serde_json::to_value(&spans).unwrap(), baseline);

        // vec_interp registered to a different model: mixing spaces would be
        // worse than skipping, so the pass declines silently.
        let db = interp_db(true);
        db.conn()
            .execute(
                "UPDATE model_registry SET model = 'some-other-model'
                 WHERE vector_table = 'vec_interp'",
                [],
            )
            .unwrap();
        let mut spans = tied_spans();
        apply_context_affinity(
            &db,
            Some(&MarkerEmbedder),
            "which product sku is Mercury",
            &mut spans,
        );
        assert_eq!(serde_json::to_value(&spans).unwrap(), baseline);

        // Not a tie: a clear fused-score gap is left alone.
        let db = interp_db(true);
        let mut spans = tied_spans();
        spans[0].candidates[0].score = 0.80;
        spans[0].candidates[1].score = 0.58;
        apply_context_affinity(
            &db,
            Some(&MarkerEmbedder),
            "which product sku is Mercury",
            &mut spans,
        );
        assert!(spans[0]
            .candidates
            .iter()
            .all(|c| c.channels.iter().all(|ch| ch.channel != "context")));
        assert_eq!(spans[0].candidates[0].score, 0.80);
    }

    #[test]
    fn context_affinity_records_but_never_boosts_an_ambiguous_cosine() {
        // A query naming both contexts is equidistant from the two cards by
        // construction (each card holds exactly two markers plus the bias),
        // so the cosine gap is 0 — under CONTEXT_COS_GAP — and the order
        // must stand while the evidence is still recorded.
        let db = interp_db(true);
        let mut spans = tied_spans();
        apply_context_affinity(
            &db,
            Some(&MarkerEmbedder),
            "office product Mercury",
            &mut spans,
        );
        let span = &spans[0];
        assert_eq!(span.candidates[0].table, "offices", "order unchanged");
        assert!((span.candidates[0].score - 0.60).abs() < 1e-9);
        assert!((span.candidates[1].score - 0.58).abs() < 1e-9);
        let raws: Vec<f64> = span
            .candidates
            .iter()
            .map(|c| {
                c.channels
                    .iter()
                    .find(|ch| ch.channel == "context")
                    .expect("evidence recorded even without a boost")
                    .raw
            })
            .collect();
        assert!(
            (raws[0] - raws[1]).abs() < 1e-6,
            "symmetric by design: {raws:?}"
        );
    }
}
