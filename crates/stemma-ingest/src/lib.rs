//! stemma-ingest: builds derived indexes in the .stemmadb store from the
//! attached (read-only) user database.
//!
//! Current scope: the lexical value index — every text value of every user
//! table, indexed three ways: normalized for exact lookup, FTS5/unicode61 for
//! BM25 token search, FTS5/trigram for fuzzy and substring matching. This is
//! the milestone-2 candidate-generation substrate; the dense (vec0) and
//! knowledge-store compilation passes join it in later milestones.

use stemma_embed::Embedder;
use stemmadb::{StemmaDb, SRC_SCHEMA};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] stemmadb::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] stemmadb::rusqlite::Error),
    #[error("embedder error: {0}")]
    Embed(#[from] stemma_embed::Error),
    #[error(
        "{table} is registered to model {registered:?} but the embedder \
         offers {offered:?}; refusing to mix vector spaces"
    )]
    ModelMismatch {
        table: String,
        registered: String,
        offered: String,
    },
    #[error("{table} exists with no model_registry row; provenance unknown, refusing to append")]
    UnregisteredVectorTable { table: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Values longer than this are indexed for full-text search but excluded from
/// the exact-match channel: a 3,000-char regulation body is a document, not a
/// value a mention equals.
pub const EXACT_MAX_LEN: usize = 120;

/// Values at or above this length are classified as documents (`is_doc`):
/// mentions resolve *into* them (BM25/snippet semantics) rather than *equal*
/// them, and scoring must not punish them for their length.
pub const DOC_MIN_LEN: usize = 200;

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStats {
    pub tables: usize,
    pub text_columns: usize,
    pub values: usize,
    pub rebuilt: bool,
    pub elapsed_ms: u128,
}

/// One text column of one user table.
#[derive(Debug)]
struct TextColumn {
    table: String,
    column: String,
}

const LEX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS lex_values (
    id         INTEGER PRIMARY KEY,
    src_table  TEXT NOT NULL,
    src_column TEXT NOT NULL,
    src_rowid  INTEGER NOT NULL,
    value      TEXT NOT NULL,
    value_norm TEXT NOT NULL,
    is_doc     INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX IF NOT EXISTS lex_values_norm ON lex_values(value_norm);
CREATE INDEX IF NOT EXISTS lex_values_src ON lex_values(src_table, src_rowid);

-- Token index: BM25-ranked word search.
CREATE VIRTUAL TABLE IF NOT EXISTS lex_fts USING fts5(
    value, content='lex_values', content_rowid='id', tokenize='unicode61'
);
-- Trigram index: fuzzy/substring search (needs query spans >= 3 chars).
CREATE VIRTUAL TABLE IF NOT EXISTS lex_trigram USING fts5(
    value, content='lex_values', content_rowid='id',
    tokenize='trigram case_sensitive 0'
);
"#;

/// Builds (or verifies) the lexical index. Skips work when the index already
/// has rows unless `force` is set.
pub fn build_lexical_index(db: &StemmaDb, force: bool) -> Result<IndexStats> {
    let start = std::time::Instant::now();
    let conn = db.conn();

    // Older stores predate the is_doc column; the index is derived state, so
    // shape changes are handled by dropping and rebuilding.
    let has_is_doc: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('lex_values') WHERE name = 'is_doc'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let lex_exists: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'lex_values'",
        [],
        |r| r.get(0),
    )?;
    let force = force || (lex_exists > 0 && has_is_doc == 0);
    if lex_exists > 0 && has_is_doc == 0 {
        conn.execute_batch(
            "DROP TABLE lex_values;
             DROP TABLE IF EXISTS lex_fts;
             DROP TABLE IF EXISTS lex_trigram;",
        )?;
    }
    conn.execute_batch(LEX_SCHEMA)?;

    let existing: i64 = conn.query_row("SELECT count(*) FROM lex_values", [], |r| r.get(0))?;
    let columns = text_columns(db)?;
    if existing > 0 && !force {
        return Ok(IndexStats {
            tables: count_tables(&columns),
            text_columns: columns.len(),
            values: existing as usize,
            rebuilt: false,
            elapsed_ms: start.elapsed().as_millis(),
        });
    }

    conn.execute_batch(
        "DELETE FROM lex_values;
         INSERT INTO lex_fts(lex_fts) VALUES('delete-all');
         INSERT INTO lex_trigram(lex_trigram) VALUES('delete-all');",
    )?;

    let mut values = 0usize;
    for tc in &columns {
        // Identifiers come from sqlite_master/table_info, quoted defensively.
        let sql = format!(
            "INSERT INTO lex_values (src_table, src_column, src_rowid, value, value_norm, is_doc)
             SELECT ?1, ?2, rowid, \"{col}\", lower(trim(\"{col}\")),
                    length(\"{col}\") >= {doc_min}
             FROM {src}.\"{tbl}\"
             WHERE \"{col}\" IS NOT NULL AND trim(\"{col}\") != ''",
            col = tc.column,
            tbl = tc.table,
            src = SRC_SCHEMA,
            doc_min = DOC_MIN_LEN,
        );
        values += conn.execute(&sql, stemmadb::rusqlite::params![tc.table, tc.column])?;
    }
    conn.execute_batch(
        "INSERT INTO lex_fts(rowid, value) SELECT id, value FROM lex_values;
         INSERT INTO lex_trigram(rowid, value) SELECT id, value FROM lex_values;",
    )?;

    Ok(IndexStats {
        tables: count_tables(&columns),
        text_columns: columns.len(),
        values,
        rebuilt: true,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DenseStats {
    pub vectors: usize,
    pub dimension: usize,
    pub model: String,
    pub promoted: bool,
}

/// Promotes externally staged vectors into the vec0 dense index.
///
/// Loaders (e.g. eval/legal/load_vectors.py) write rows into `vec_staging`
/// — a plain table, writable without the sqlite-vec extension — and this
/// pass, running inside the extension-bearing process, creates the `vec0`
/// virtual table, moves the vectors in, records the model identity in
/// `model_registry`, and drops the staging table. One model per dense
/// table, always: mixed identities in staging are a hard error.
pub fn build_dense_index(db: &StemmaDb) -> Result<Option<DenseStats>> {
    let conn = db.conn();
    let staged: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'vec_staging'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if staged == 0 {
        // No staging: report the existing dense index if one is registered.
        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT model, dimension FROM model_registry WHERE vector_table = 'vec_dense'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        return Ok(existing.map(|(model, dim)| {
            let vectors: i64 = conn
                .query_row("SELECT count(*) FROM vec_dense", [], |r| r.get(0))
                .unwrap_or(0);
            DenseStats {
                vectors: vectors as usize,
                dimension: dim as usize,
                model,
                promoted: false,
            }
        }));
    }

    let identities: Vec<(String, i64)> = conn
        .prepare("SELECT DISTINCT model, dim FROM vec_staging")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let (model, dim) = match identities.as_slice() {
        [one] => one.clone(),
        _ => panic!("vec_staging holds mixed model identities: {identities:?}"),
    };

    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS vec_dense;
         CREATE VIRTUAL TABLE vec_dense USING vec0(
             embedding float[{dim}],
             src_table text,
             src_column text,
             src_rowid integer
         );"
    ))?;
    let moved = conn.execute(
        "INSERT INTO vec_dense (embedding, src_table, src_column, src_rowid)
         SELECT embedding, src_table, src_column, src_rowid FROM vec_staging",
        [],
    )?;
    conn.execute(
        "INSERT INTO model_registry (vector_table, backend, model, dimension, quantization)
         VALUES ('vec_dense', 'staged', ?1, ?2, 'f32')
         ON CONFLICT(vector_table) DO UPDATE SET model = ?1, dimension = ?2",
        stemmadb::rusqlite::params![model, dim],
    )?;
    conn.execute_batch("DROP TABLE vec_staging;")?;

    Ok(Some(DenseStats {
        vectors: moved,
        dimension: dim as usize,
        model,
        promoted: true,
    }))
}

/// Items per embedding call when draining the queue.
pub const EMBED_BATCH: usize = 32;

/// A queue item failing this many embedding attempts is marked `failed` and
/// left for inspection rather than retried forever.
pub const EMBED_MAX_ATTEMPTS: i64 = 3;

/// Finds document cells (`lex_values.is_doc = 1`) with no vector in
/// `vec_dense` and enqueues them as pending work in `embed_queue`. Documents
/// only — short values travel through the interpretation-card path instead
/// ([`enqueue_missing_interpretations`]), which serializes column context the
/// raw value does not carry.
///
/// Idempotent: an item already pending (or failed) is left alone; an item
/// previously `done` whose vector has since disappeared is reset to pending.
/// Returns the number of items newly enqueued or reset.
pub fn enqueue_missing_embeddings(db: &StemmaDb) -> Result<usize> {
    let conn = db.conn();
    let has_dense: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'vec_dense'",
        [],
        |r| r.get(0),
    )?;
    // vec0 metadata columns are not probe-friendly; materialize the covered
    // triples once instead of scanning the virtual table per lex row.
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS covered (
             src_table TEXT NOT NULL, src_column TEXT NOT NULL, src_rowid INTEGER NOT NULL
         );
         DELETE FROM covered;",
    )?;
    if has_dense > 0 {
        conn.execute(
            "INSERT INTO covered SELECT src_table, src_column, src_rowid FROM vec_dense",
            [],
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS temp.covered_key ON covered(src_table, src_column, src_rowid);",
    )?;
    let queued = conn.execute(
        "INSERT INTO embed_queue (src_table, src_column, src_rowid)
         SELECT lv.src_table, lv.src_column, lv.src_rowid
         FROM lex_values lv
         WHERE lv.is_doc = 1
           AND NOT EXISTS (
               SELECT 1 FROM covered c
               WHERE c.src_table = lv.src_table
                 AND c.src_column = lv.src_column
                 AND c.src_rowid = lv.src_rowid)
         ON CONFLICT (src_table, src_column, src_rowid) DO UPDATE SET
             status = 'pending', attempts = 0, error = '',
             updated_at = datetime('now')
         WHERE embed_queue.status = 'done'",
        [],
    )?;
    conn.execute_batch("DROP TABLE covered;")?;
    Ok(queued)
}

/// Interpretation cards never exceed this many characters: enough for the
/// provenance line plus two context fragments, small enough that the card is
/// one embedding call's worth of text on any encoder.
pub const INTERP_CARD_MAX_CHARS: usize = 300;

/// Renders the interpretation card for one distinct value interpretation:
/// `"{table} · {column} · {value}"` plus up to two `col: value` context
/// fragments from the representative row's other short text columns.
/// Deterministic — fragments arrive in column-name order and are appended
/// only while the card stays within [`INTERP_CARD_MAX_CHARS`].
fn interpretation_card(
    table: &str,
    column: &str,
    value: &str,
    fragments: &[(String, String)],
) -> String {
    let mut card = format!("{table} · {column} · {value}");
    if card.chars().count() > INTERP_CARD_MAX_CHARS {
        card = card.chars().take(INTERP_CARD_MAX_CHARS).collect();
    }
    for (col, val) in fragments.iter().take(2) {
        let frag = format!(" · {col}: {val}");
        if card.chars().count() + frag.chars().count() <= INTERP_CARD_MAX_CHARS {
            card.push_str(&frag);
        }
    }
    card
}

/// Finds distinct value interpretations — one per `(src_table, src_column,
/// value_norm)` with `is_doc = 0` — that have no vector in `vec_interp`, and
/// enqueues each as pending work with its serialization card stored in
/// `embed_queue.serialized`. The queue key is the interpretation's
/// representative cell: `src_rowid = MIN(src_rowid)` over the rows sharing
/// the value, so the provenance-triple unique key stays exact and enqueue
/// stays idempotent with the same reset-on-vanished-vector semantics as the
/// document path.
///
/// This is the relational counterpart of the document queue: on value-shaped
/// corpora nothing crosses `DOC_MIN_LEN`, so a documents-only dense channel
/// is inert, and a value appearing in two columns needs column context to be
/// separable at all. The card carries that context.
pub fn enqueue_missing_interpretations(db: &StemmaDb) -> Result<usize> {
    let conn = db.conn();
    let has_interp: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'vec_interp'",
        [],
        |r| r.get(0),
    )?;
    // Same materialization trick as the document path: vec0 metadata columns
    // are not probe-friendly, so the covered keys are copied out once.
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS covered_interp (
             src_table TEXT NOT NULL, src_column TEXT NOT NULL, src_rowid INTEGER NOT NULL
         );
         DELETE FROM covered_interp;",
    )?;
    if has_interp > 0 {
        conn.execute(
            "INSERT INTO covered_interp
             SELECT src_table, src_column, src_rowid FROM vec_interp",
            [],
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS temp.covered_interp_key
             ON covered_interp(src_table, src_column, src_rowid);",
    )?;

    // One row per distinct interpretation; the bare `value` column resolves
    // to the row that achieved MIN(src_rowid) (SQLite's documented bare-
    // column-with-min semantics), which is exactly the representative cell.
    let missing: Vec<(String, String, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT t.src_table, t.src_column, t.rep, t.value FROM (
                 SELECT src_table, src_column, MIN(src_rowid) AS rep, value
                 FROM lex_values
                 WHERE is_doc = 0
                 GROUP BY src_table, src_column, value_norm
             ) t
             WHERE NOT EXISTS (
                 SELECT 1 FROM covered_interp c
                 WHERE c.src_table = t.src_table
                   AND c.src_column = t.src_column
                   AND c.src_rowid = t.rep)
             ORDER BY t.src_table, t.src_column, t.rep",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    let mut queued = 0usize;
    {
        let mut frag_stmt = conn.prepare_cached(
            "SELECT src_column, value FROM lex_values
             WHERE src_table = ?1 AND src_rowid = ?2 AND is_doc = 0
               AND src_column != ?3
             ORDER BY src_column LIMIT 2",
        )?;
        let mut insert = conn.prepare_cached(
            "INSERT INTO embed_queue (src_table, src_column, src_rowid, serialized)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (src_table, src_column, src_rowid) DO UPDATE SET
                 status = 'pending', attempts = 0, error = '', serialized = ?4,
                 updated_at = datetime('now')
             WHERE embed_queue.status = 'done'",
        )?;
        for (table, column, rep, value) in missing {
            let fragments: Vec<(String, String)> = frag_stmt
                .query_map(stemmadb::rusqlite::params![table, rep, column], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<std::result::Result<_, _>>()?;
            let card = interpretation_card(&table, &column, &value, &fragments);
            queued += insert.execute(stemmadb::rusqlite::params![table, column, rep, card])?;
        }
    }
    conn.execute_batch("DROP TABLE covered_interp;")?;
    Ok(queued)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DrainStats {
    /// Items embedded and marked done by this call.
    pub drained: usize,
    /// Items marked failed by this call (budget exhausted or unembeddable).
    pub failed: usize,
    /// Pending items left after this call.
    pub remaining: usize,
}

/// Drains one batch of pending `embed_queue` items through the embedder.
/// Document items (empty `serialized`) embed their raw stored text into
/// `vec_dense`; interpretation items embed their serialization card — also
/// raw, cards are "documents" in the asymmetric scheme — into `vec_interp`.
/// Either vec0 table is created (at the embedder's reported dimension) with
/// its own `model_registry` row on first use.
///
/// Documents and cards are embedded RAW — the asymmetric Qwen3-style
/// convention puts the instruction on the query side only
/// (`stemma_embed::format_query`); applying it here would desert the vector
/// space the queries live in.
///
/// Failure semantics: an embedding-call failure bumps `attempts` on the batch
/// and returns the error (the caller decides whether to keep going); an item
/// out of retry budget, or whose source text has vanished, is marked
/// `failed` with an error note and never blocks the rest. A registry row
/// carrying a different model identity — on either vector table — is a hard
/// refusal: every pending item is marked failed and the call errors, because
/// appending into a foreign vector space is worse than not embedding at all.
pub fn drain_embed_queue(
    db: &StemmaDb,
    embedder: &dyn Embedder,
    batch: usize,
) -> Result<DrainStats> {
    let conn = db.conn();
    let mut failed = 0usize;

    // Model identity discipline, checked before any embedding work, for both
    // vector tables the queue can feed. The `model` string is the
    // vector-space identity; `backend` records how vectors arrived ('staged'
    // loaders and a live embedder of the same model share a space).
    let offered = embedder.identity().model;
    let mut has_dense = false;
    let mut has_interp = false;
    let mut refusal: Option<Error> = None;
    for table in ["vec_dense", "vec_interp"] {
        let exists: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = ?1",
            [table],
            |r| r.get(0),
        )?;
        match table {
            "vec_dense" => has_dense = exists > 0,
            _ => has_interp = exists > 0,
        }
        let registered: Option<String> = conn
            .query_row(
                "SELECT model FROM model_registry WHERE vector_table = ?1",
                [table],
                |r| r.get(0),
            )
            .ok();
        refusal = match &registered {
            Some(m) if *m != offered => Some(Error::ModelMismatch {
                table: table.to_string(),
                registered: m.clone(),
                offered: offered.clone(),
            }),
            None if exists > 0 => Some(Error::UnregisteredVectorTable {
                table: table.to_string(),
            }),
            _ => None,
        };
        if refusal.is_some() {
            break;
        }
    }
    if let Some(err) = refusal {
        conn.execute(
            "UPDATE embed_queue SET status = 'failed', error = ?1,
                    updated_at = datetime('now')
             WHERE status = 'pending'",
            [err.to_string()],
        )?;
        return Err(err);
    }

    // Items out of retry budget fail now rather than being picked forever.
    failed += conn.execute(
        "UPDATE embed_queue SET status = 'failed',
                error = CASE WHEN error = '' THEN 'retry budget exhausted' ELSE error END,
                updated_at = datetime('now')
         WHERE status = 'pending' AND attempts >= ?1",
        [EMBED_MAX_ATTEMPTS],
    )?;

    // One batch of pending work, least-retried first. `serialized` is the
    // text to embed when set — the interpretation card, which also routes the
    // vector to vec_interp; documents leave it empty and are fetched from
    // lex_values so the store holds each document once.
    let mut items: Vec<(i64, String, String, i64, String, Option<String>)> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT q.id, q.src_table, q.src_column, q.src_rowid, q.serialized,
                    coalesce(nullif(q.serialized, ''), lv.value)
             FROM embed_queue q
             LEFT JOIN lex_values lv
               ON lv.src_table = q.src_table AND lv.src_column = q.src_column
              AND lv.src_rowid = q.src_rowid
             WHERE q.status = 'pending'
             ORDER BY q.attempts, q.id
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([batch as i64], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        for row in rows {
            items.push(row?);
        }
    }

    // Items whose source text is gone (index rebuilt out from under the
    // queue) cannot be embedded, now or later. Interpretation items carry
    // their card in the queue, so only document items can go missing.
    let mut texts = Vec::new();
    let mut work: Vec<(i64, String, String, i64, &'static str)> = Vec::new();
    for (id, table, column, rowid, serialized, text) in items {
        match text {
            Some(t) => {
                let target = if serialized.is_empty() {
                    "vec_dense"
                } else {
                    "vec_interp"
                };
                work.push((id, table, column, rowid, target));
                texts.push(t);
            }
            None => {
                conn.execute(
                    "UPDATE embed_queue SET status = 'failed',
                            error = 'source text missing from lex_values',
                            updated_at = datetime('now')
                     WHERE id = ?1",
                    [id],
                )?;
                failed += 1;
            }
        }
    }

    let mut drained = 0usize;
    if !work.is_empty() {
        let vectors = match embedder.embed(&texts) {
            Ok(v) => v,
            Err(e) => {
                // The whole batch shares one call; charge one attempt each
                // and leave the items pending for the budget to bound.
                let note = e.to_string();
                for (id, ..) in &work {
                    conn.execute(
                        "UPDATE embed_queue SET attempts = attempts + 1, error = ?2,
                                updated_at = datetime('now')
                         WHERE id = ?1",
                        stemmadb::rusqlite::params![id, note],
                    )?;
                }
                return Err(e.into());
            }
        };

        let dim = vectors.first().map(|v| v.len()).unwrap_or(0);
        let identity = embedder.identity();
        for (target, exists) in [("vec_dense", has_dense), ("vec_interp", has_interp)] {
            if !work.iter().any(|w| w.4 == target) {
                continue;
            }
            if !exists {
                conn.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS {target} USING vec0(
                         embedding float[{dim}],
                         src_table text,
                         src_column text,
                         src_rowid integer
                     );"
                ))?;
            }
            conn.execute(
                "INSERT INTO model_registry (vector_table, backend, model, dimension, quantization)
                 VALUES (?1, ?2, ?3, ?4, 'f32')
                 ON CONFLICT(vector_table) DO NOTHING",
                stemmadb::rusqlite::params![target, identity.backend, identity.model, dim as i64],
            )?;
        }

        for ((id, table, column, rowid, target), vector) in work.into_iter().zip(vectors) {
            let blob: Vec<u8> = vector.iter().flat_map(|x| x.to_le_bytes()).collect();
            let inserted = conn.execute(
                &format!(
                    "INSERT INTO {target} (embedding, src_table, src_column, src_rowid)
                     VALUES (?1, ?2, ?3, ?4)"
                ),
                stemmadb::rusqlite::params![blob, table, column, rowid],
            );
            match inserted {
                Ok(_) => {
                    conn.execute(
                        "UPDATE embed_queue SET status = 'done', error = '',
                                updated_at = datetime('now')
                         WHERE id = ?1",
                        [id],
                    )?;
                    drained += 1;
                }
                Err(e) => {
                    // Wrong dimension or other per-row rejection: this item
                    // can never succeed against this table.
                    conn.execute(
                        "UPDATE embed_queue SET status = 'failed', error = ?2,
                                updated_at = datetime('now')
                         WHERE id = ?1",
                        stemmadb::rusqlite::params![id, e.to_string()],
                    )?;
                    failed += 1;
                }
            }
        }
    }

    let remaining: i64 = conn.query_row(
        "SELECT count(*) FROM embed_queue WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?;
    Ok(DrainStats {
        drained,
        failed,
        remaining: remaining as usize,
    })
}

fn count_tables(cols: &[TextColumn]) -> usize {
    let mut t: Vec<&str> = cols.iter().map(|c| c.table.as_str()).collect();
    t.sort_unstable();
    t.dedup();
    t.len()
}

/// TEXT-typed columns of every user table.
fn text_columns(db: &StemmaDb) -> Result<Vec<TextColumn>> {
    let mut out = Vec::new();
    for table in db.src_tables()? {
        let conn = db.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT name, type FROM pragma_table_info(?1, '{SRC_SCHEMA}')"
        ))?;
        let cols = stmt.query_map([&table], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for c in cols {
            let (name, ty) = c?;
            let ty = ty.to_uppercase();
            if ty.contains("TEXT") || ty.contains("CHAR") {
                out.push(TextColumn {
                    table: table.clone(),
                    column: name,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mini_db() -> StemmaDb {
        let db = StemmaDb::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE src.offices(id INTEGER PRIMARY KEY, name TEXT, city TEXT);
                 INSERT INTO src.offices VALUES
                    (17, 'Seattle - Northgate', 'Seattle'),
                    (18, 'Portland Downtown', 'Portland');
                 CREATE TABLE src.notes(id INTEGER PRIMARY KEY, body TEXT, num INTEGER);
                 INSERT INTO src.notes VALUES (1, 'quarterly revenue back on track', 7);",
            )
            .unwrap();
        db
    }

    #[test]
    fn indexes_text_columns_only() {
        let db = mini_db();
        let stats = build_lexical_index(&db, false).unwrap();
        assert!(stats.rebuilt);
        assert_eq!(stats.tables, 2);
        assert_eq!(stats.text_columns, 3); // name, city, body — not num
        assert_eq!(stats.values, 5);
    }

    #[test]
    fn exact_and_fuzzy_lookups_work() {
        let db = mini_db();
        build_lexical_index(&db, false).unwrap();
        let conn = db.conn();
        let exact: i64 = conn
            .query_row(
                "SELECT count(*) FROM lex_values WHERE value_norm = lower('Seattle')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exact, 1);
        // Trigram finds the substring mention inside the longer stored value.
        let fuzzy: i64 = conn
            .query_row(
                "SELECT count(*) FROM lex_trigram WHERE lex_trigram MATCH '\"seattle\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fuzzy, 2); // 'Seattle' and 'Seattle - Northgate'
    }

    #[test]
    fn rebuild_is_skipped_unless_forced() {
        let db = mini_db();
        build_lexical_index(&db, false).unwrap();
        let again = build_lexical_index(&db, false).unwrap();
        assert!(!again.rebuilt);
        let forced = build_lexical_index(&db, true).unwrap();
        assert!(forced.rebuilt);
    }

    /// Deterministic embedder: each text maps to a unit vector on a small
    /// hypersphere, seeded by its bytes, so identical texts embed identically
    /// and different texts land apart.
    struct FakeEmbedder {
        dim: usize,
        fail: bool,
    }

    impl FakeEmbedder {
        fn new(dim: usize) -> Self {
            Self { dim, fail: false }
        }
        fn vector(&self, text: &str) -> Vec<f32> {
            let mut state: u64 = 0xcbf29ce484222325;
            for b in text.bytes() {
                state ^= b as u64;
                state = state.wrapping_mul(0x100000001b3);
            }
            // splitmix64 stream seeded by the text hash: the finalizer's
            // nonlinearity keeps different texts' vectors uncorrelated.
            let mut z = state;
            let mut v: Vec<f32> = (0..self.dim)
                .map(|_| {
                    z = z.wrapping_add(0x9e3779b97f4a7c15);
                    let mut x = z;
                    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                    x ^= x >> 31;
                    (x as f64 / u64::MAX as f64) as f32 - 0.5
                })
                .collect();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= norm);
            v
        }
    }

    impl stemma_embed::Embedder for FakeEmbedder {
        fn embed(&self, texts: &[String]) -> stemma_embed::Result<Vec<Vec<f32>>> {
            if self.fail {
                return Err(stemma_embed::Error::Http("fake endpoint down".into()));
            }
            Ok(texts.iter().map(|t| self.vector(t)).collect())
        }
        fn identity(&self) -> stemma_embed::ModelIdentity {
            stemma_embed::ModelIdentity {
                backend: "fake".into(),
                model: "fake-embedder".into(),
                dimension: self.dim,
            }
        }
    }

    /// A corpus with actual documents: three long bodies (>= DOC_MIN_LEN)
    /// and short values that must stay out of the queue.
    fn doc_db() -> StemmaDb {
        let db = StemmaDb::open_in_memory().unwrap();
        let body = |topic: &str| {
            format!(
                "Article concerning {topic}. This body exists to cross the document \
                 threshold, so it repeats itself with modest dignity: {topic}, again \
                 {topic}, considered from every angle a regulation writer can afford, \
                 until the two-hundred character mark is safely behind it and the \
                 classifier files it as a document rather than a value."
            )
        };
        db.conn()
            .execute_batch(&format!(
                "CREATE TABLE src.articles(id INTEGER PRIMARY KEY, title TEXT, body TEXT);
                 INSERT INTO src.articles VALUES
                    (1, 'Coastal permits', '{a}'),
                    (2, 'Insurance filings', '{b}'),
                    (3, 'Water rights', '{c}');",
                a = body("coastal development permits"),
                b = body("insurance filing deadlines"),
                c = body("appropriative water rights"),
            ))
            .unwrap();
        build_lexical_index(&db, false).unwrap();
        db
    }

    fn queue_counts(db: &StemmaDb) -> (i64, i64, i64) {
        let count = |status: &str| -> i64 {
            db.conn()
                .query_row(
                    "SELECT count(*) FROM embed_queue WHERE status = ?1",
                    [status],
                    |r| r.get(0),
                )
                .unwrap()
        };
        (count("pending"), count("done"), count("failed"))
    }

    #[test]
    fn enqueue_finds_exactly_the_missing_docs() {
        let db = doc_db();
        let queued = enqueue_missing_embeddings(&db).unwrap();
        assert_eq!(queued, 3, "three doc bodies, no vectors yet");
        assert_eq!(queue_counts(&db), (3, 0, 0));
        // Only body cells qualify; titles are values.
        let non_body: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM embed_queue WHERE src_column != 'body'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(non_body, 0);
        // Re-enqueue with items still pending is a no-op.
        assert_eq!(enqueue_missing_embeddings(&db).unwrap(), 0);
    }

    #[test]
    fn drain_populates_vec_dense_and_is_idempotent() {
        let db = doc_db();
        let embedder = FakeEmbedder::new(8);
        enqueue_missing_embeddings(&db).unwrap();
        let stats = drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!(stats.drained, 3);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.remaining, 0);
        assert_eq!(queue_counts(&db), (0, 3, 0));

        let vectors: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM vec_dense", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 3);
        let (backend, model, dim): (String, String, i64) = db
            .conn()
            .query_row(
                "SELECT backend, model, dimension FROM model_registry
                 WHERE vector_table = 'vec_dense'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (backend.as_str(), model.as_str(), dim),
            ("fake", "fake-embedder", 8)
        );

        // The full cycle again: nothing to queue, nothing to drain.
        assert_eq!(enqueue_missing_embeddings(&db).unwrap(), 0);
        let again = drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!((again.drained, again.failed, again.remaining), (0, 0, 0));
        let vectors_again: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM vec_dense", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors_again, 3, "idempotent: no duplicate vectors");
    }

    #[test]
    fn mismatched_registry_identity_refuses_and_fails_items() {
        let db = doc_db();
        db.conn()
            .execute(
                "INSERT INTO model_registry (vector_table, backend, model, dimension)
                 VALUES ('vec_dense', 'staged', 'some-other-model', 1024)",
                [],
            )
            .unwrap();
        enqueue_missing_embeddings(&db).unwrap();
        let err = drain_embed_queue(&db, &FakeEmbedder::new(8), EMBED_BATCH).unwrap_err();
        assert!(matches!(err, Error::ModelMismatch { .. }), "got {err}");
        let (pending, done, failed) = queue_counts(&db);
        assert_eq!((pending, done, failed), (0, 0, 3));
        let note: String = db
            .conn()
            .query_row("SELECT error FROM embed_queue LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert!(
            note.contains("some-other-model"),
            "error note names the model: {note}"
        );
    }

    #[test]
    fn embed_failures_respect_the_retry_budget() {
        let db = doc_db();
        enqueue_missing_embeddings(&db).unwrap();
        let broken = FakeEmbedder { dim: 8, fail: true };
        for attempt in 1..=EMBED_MAX_ATTEMPTS {
            let err = drain_embed_queue(&db, &broken, EMBED_BATCH).unwrap_err();
            assert!(matches!(err, Error::Embed(_)), "attempt {attempt}: {err}");
        }
        // Budget spent: the next drain sweeps the items into `failed` and
        // reports an empty queue instead of looping forever.
        let stats = drain_embed_queue(&db, &broken, EMBED_BATCH).unwrap();
        assert_eq!((stats.drained, stats.failed, stats.remaining), (0, 3, 0));
        assert_eq!(queue_counts(&db), (0, 0, 3));
    }

    /// A value-shaped corpus: short cells only, with a value repeated across
    /// rows ('Portland') and columns that give each interpretation context.
    fn value_db() -> StemmaDb {
        let db = StemmaDb::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE src.offices(id INTEGER PRIMARY KEY, name TEXT, city TEXT);
                 INSERT INTO src.offices VALUES
                    (17, 'Seattle - Northgate', 'Seattle'),
                    (18, 'Portland Downtown', 'Portland'),
                    (19, 'Portland Airport', 'Portland');",
            )
            .unwrap();
        build_lexical_index(&db, false).unwrap();
        db
    }

    #[test]
    fn interpretation_cards_are_deterministic_and_bounded() {
        let db = value_db();
        enqueue_missing_interpretations(&db).unwrap();
        let card: String = db
            .conn()
            .query_row(
                "SELECT serialized FROM embed_queue
                 WHERE src_table = 'offices' AND src_column = 'city' AND src_rowid = 17",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(card, "offices · city · Seattle · name: Seattle - Northgate");
        let cards: Vec<String> = db
            .conn()
            .prepare("SELECT serialized FROM embed_queue")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for card in &cards {
            assert!(!card.is_empty(), "interpretation items carry their card");
            assert!(card.chars().count() <= INTERP_CARD_MAX_CHARS);
        }
        // A near-limit value: the base line survives, fragments that would
        // overflow are dropped, and the card never crosses the cap.
        let long = "x".repeat(280);
        let card = interpretation_card(
            "t",
            "c",
            &long,
            &[
                ("other".into(), "y".repeat(50)),
                ("more".into(), "z".into()),
            ],
        );
        assert!(card.chars().count() <= INTERP_CARD_MAX_CHARS);
        assert!(card.starts_with("t · c · x"));
        assert!(!card.contains("other"), "oversized fragment is dropped");
    }

    #[test]
    fn enqueue_finds_interpretations_exactly_once() {
        let db = value_db();
        let queued = enqueue_missing_interpretations(&db).unwrap();
        // 3 distinct names + 2 distinct cities; the repeated 'Portland' is
        // one interpretation, keyed by its representative MIN rowid.
        assert_eq!(queued, 5);
        assert_eq!(queue_counts(&db), (5, 0, 0));
        let portland: Vec<i64> = db
            .conn()
            .prepare(
                "SELECT src_rowid FROM embed_queue
                 WHERE src_table = 'offices' AND src_column = 'city'
                   AND serialized LIKE '%Portland%'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(portland, vec![18], "one item at the representative rowid");
        // Re-enqueue with items still pending is a no-op.
        assert_eq!(enqueue_missing_interpretations(&db).unwrap(), 0);
    }

    #[test]
    fn drain_populates_vec_interp_and_registry() {
        let db = value_db();
        let embedder = FakeEmbedder::new(8);
        enqueue_missing_interpretations(&db).unwrap();
        let stats = drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!((stats.drained, stats.failed, stats.remaining), (5, 0, 0));

        let vectors: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM vec_interp", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 5);
        let (backend, model, dim): (String, String, i64) = db
            .conn()
            .query_row(
                "SELECT backend, model, dimension FROM model_registry
                 WHERE vector_table = 'vec_interp'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (backend.as_str(), model.as_str(), dim),
            ("fake", "fake-embedder", 8)
        );
        // No documents in this corpus, so no vec_dense and no dense registry
        // row: the tables are independent.
        let dense: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'vec_dense'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dense, 0);

        // The full cycle again: nothing to queue, nothing to drain.
        assert_eq!(enqueue_missing_interpretations(&db).unwrap(), 0);
        let again = drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!((again.drained, again.failed, again.remaining), (0, 0, 0));
        let vectors_again: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM vec_interp", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors_again, 5, "idempotent: no duplicate vectors");
    }

    #[test]
    fn mixed_queue_routes_docs_and_interps_to_their_tables() {
        // doc_db has three document bodies AND three short titles: one queue,
        // two vector tables, one drain.
        let db = doc_db();
        let embedder = FakeEmbedder::new(8);
        let docs = enqueue_missing_embeddings(&db).unwrap();
        let interps = enqueue_missing_interpretations(&db).unwrap();
        assert_eq!((docs, interps), (3, 3));
        let stats = drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!((stats.drained, stats.failed, stats.remaining), (6, 0, 0));
        let count = |table: &str| -> i64 {
            db.conn()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!((count("vec_dense"), count("vec_interp")), (3, 3));
        let registered: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM model_registry
                 WHERE vector_table IN ('vec_dense', 'vec_interp')
                   AND model = 'fake-embedder'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(registered, 2, "each vector table owns a registry row");
        // Interpretation vectors carry the card's embedding, not the value's:
        // the drain embeds exactly what the queue serialized.
        let card: String = db
            .conn()
            .query_row(
                "SELECT serialized FROM embed_queue
                 WHERE src_column = 'title' AND src_rowid = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let stored: Vec<u8> = db
            .conn()
            .query_row(
                "SELECT embedding FROM vec_interp
                 WHERE src_table = 'articles' AND src_column = 'title' AND src_rowid = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let expected: Vec<u8> = embedder
            .vector(&card)
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        assert_eq!(stored, expected);
    }

    #[test]
    fn interp_registry_mismatch_refuses_and_fails_items() {
        let db = value_db();
        db.conn()
            .execute(
                "INSERT INTO model_registry (vector_table, backend, model, dimension)
                 VALUES ('vec_interp', 'staged', 'some-other-model', 1024)",
                [],
            )
            .unwrap();
        enqueue_missing_interpretations(&db).unwrap();
        let err = drain_embed_queue(&db, &FakeEmbedder::new(8), EMBED_BATCH).unwrap_err();
        match &err {
            Error::ModelMismatch { table, .. } => assert_eq!(table, "vec_interp"),
            other => panic!("expected ModelMismatch, got {other}"),
        }
        let (pending, done, failed) = queue_counts(&db);
        assert_eq!((pending, done, failed), (0, 0, 5));
        let note: String = db
            .conn()
            .query_row("SELECT error FROM embed_queue LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert!(
            note.contains("some-other-model") && note.contains("vec_interp"),
            "error note names table and model: {note}"
        );
    }

    #[test]
    fn knn_over_drained_vectors_returns_sane_neighbors() {
        let db = doc_db();
        let embedder = FakeEmbedder::new(8);
        enqueue_missing_embeddings(&db).unwrap();
        drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();

        // Query with the exact stored document text: its own vector must be
        // the nearest neighbor at cosine ~1 (cos = 1 - d^2/2 on unit vectors,
        // as in resolve's dense channel).
        let target: String = db
            .conn()
            .query_row(
                "SELECT value FROM lex_values WHERE src_rowid = 2 AND is_doc = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v = embedder.vector(&target);
        let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        let hits: Vec<(i64, f64)> = db
            .conn()
            .prepare(
                "SELECT src_rowid, distance FROM vec_dense
                 WHERE embedding MATCH ?1 AND k = 3",
            )
            .unwrap()
            .query_map([blob], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(hits.len(), 3);
        let (top_rowid, top_dist) = hits[0];
        assert_eq!(top_rowid, 2);
        let cos = 1.0 - (top_dist * top_dist) / 2.0;
        assert!(cos > 0.999, "self-similarity should be ~1, got {cos}");
        // The other documents are real but distant neighbors.
        for (rowid, dist) in &hits[1..] {
            let cos = 1.0 - (dist * dist) / 2.0;
            assert_ne!(*rowid, 2);
            assert!(
                cos < 0.99,
                "distinct docs must not collapse: {rowid} at {cos}"
            );
        }
    }
}
