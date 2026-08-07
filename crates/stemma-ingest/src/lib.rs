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

-- Column typology: one profile row per indexed (table, column), derived from
-- lex_values by profile_columns(). Same lifecycle as the rest of the lexical
-- index — dropped and rebuilt with it, never migrated.
CREATE TABLE IF NOT EXISTS lex_columns (
    src_table      TEXT NOT NULL,
    src_column     TEXT NOT NULL,
    n_values       INTEGER NOT NULL,
    n_distinct     INTEGER NOT NULL,
    distinct_ratio REAL NOT NULL,
    alpha_ratio    REAL NOT NULL,
    numeric_ratio  REAL NOT NULL,
    temporal_ratio REAL NOT NULL,
    idlike_ratio   REAL NOT NULL,
    doc_ratio      REAL NOT NULL,
    avg_len        REAL NOT NULL,
    kind           TEXT NOT NULL,
    PRIMARY KEY (src_table, src_column)
) STRICT;

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
        // Stores built before column typology existed have rows but no
        // profiles; derive them now — same data, no reindex needed.
        let profiled: i64 =
            conn.query_row("SELECT count(*) FROM lex_columns", [], |r| r.get(0))?;
        if profiled == 0 {
            profile_columns(db)?;
        }
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
    profile_columns(db)?;

    Ok(IndexStats {
        tables: count_tables(&columns),
        text_columns: columns.len(),
        values,
        rebuilt: true,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

// --- Column typology -------------------------------------------------------
//
// The lexical index records what values exist; `lex_columns` records what
// KIND of thing each column holds. Downstream passes consume the kind rather
// than re-deriving per-value shape heuristics: interpretation candidacy
// admits only `text` columns, and the KG's term→column affinity only points
// at `text` columns. The profile is derived state with the same lifecycle as
// lex_values — rebuilt whenever the index rebuilds, never migrated.

/// A column is `document` when more than this fraction of its cells crossed
/// [`DOC_MIN_LEN`].
pub const KIND_DOC_MIN: f64 = 0.5;
/// A column is `temporal` when more than this fraction of its values are
/// epoch numbers ([`TEMPORAL_EPOCH_RANGES`]) or ISO-date shaped.
pub const KIND_TEMPORAL_MIN: f64 = 0.8;
/// A column is `numeric` when more than this fraction of its values consist
/// only of digits and the characters `. e + -`.
pub const KIND_NUMERIC_MIN: f64 = 0.8;
/// A column is `identifier` when more than this fraction of its values are
/// id-shaped: uuid, 16+ chars of pure hex/digits, or 6+ pure digits.
pub const KIND_IDLIKE_MIN: f64 = 0.8;
/// Cardinality gate for the `identifier`-by-distinctness and `code` rules: a
/// column whose values are (almost) all distinct behaves like a key or a
/// code list, not vocabulary.
pub const KIND_DISTINCT_MIN: f64 = 0.95;
/// `identifier`-by-distinctness also requires that fewer than this fraction
/// of values contain any letter — near-unique *prose* (names, titles) stays
/// `text`.
pub const KIND_ALPHA_MAX: f64 = 0.5;
/// `code` (SKU-shaped): near-all-distinct and more than this fraction of
/// values are space-free tokens.
pub const KIND_CODE_NOSPACE_MIN: f64 = 0.5;
/// Cardinality-based rules (`identifier` by distinctness, `code`) need a
/// sample: below this many values every column is trivially near-distinct,
/// so only the shape-based rules apply and small columns default to `text`.
pub const KIND_CARDINALITY_MIN_VALUES: i64 = 20;
/// Numeric values inside either range read as epoch timestamps:
/// seconds (1e8..4e9 ≈ 1973..2096) and milliseconds (1e11..4e12).
pub const TEMPORAL_EPOCH_RANGES: [(f64, f64); 2] = [(1e8, 4e9), (1e11, 4e12)];

/// Classification priority: first predicate that fires names the column.
/// Order matters — a copied epoch column is numeric too, but `temporal` is
/// the more specific truth; near-distinct digit keys are numeric before
/// they are anything else.
fn classify_kind(
    n_values: i64,
    distinct_ratio: f64,
    alpha_ratio: f64,
    numeric_ratio: f64,
    temporal_ratio: f64,
    idlike_ratio: f64,
    doc_ratio: f64,
    nospace_ratio: f64,
) -> &'static str {
    let big = n_values >= KIND_CARDINALITY_MIN_VALUES;
    if doc_ratio > KIND_DOC_MIN {
        "document"
    } else if temporal_ratio > KIND_TEMPORAL_MIN {
        "temporal"
    } else if numeric_ratio > KIND_NUMERIC_MIN {
        "numeric"
    } else if idlike_ratio > KIND_IDLIKE_MIN
        || (big && distinct_ratio > KIND_DISTINCT_MIN && alpha_ratio < KIND_ALPHA_MAX)
    {
        "identifier"
    } else if big && distinct_ratio > KIND_DISTINCT_MIN && nospace_ratio > KIND_CODE_NOSPACE_MIN {
        "code"
    } else {
        "text"
    }
}

/// Rebuilds `lex_columns` from `lex_values`: one grouped pass computing the
/// per-column value-shape ratios, classified into a `kind` by
/// [`classify_kind`]. Called at the end of every index (re)build; returns the
/// number of columns profiled.
pub fn profile_columns(db: &StemmaDb) -> Result<usize> {
    let conn = db.conn();
    // GLOB shapes over value_norm (already lower(trim())). In a GLOB set,
    // `^` first negates and `-` last is literal.
    let numeric = "value_norm GLOB '*[0-9]*' AND value_norm NOT GLOB '*[^0-9.e+-]*'";
    let h = "[0-9a-f]";
    let uuid = format!(
        "{a}-{b}-{b}-{b}-{c}",
        a = h.repeat(8),
        b = h.repeat(4),
        c = h.repeat(12)
    );
    let epoch = TEMPORAL_EPOCH_RANGES
        .iter()
        .map(|(lo, hi)| format!("CAST(value_norm AS REAL) BETWEEN {lo:e} AND {hi:e}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!(
        "SELECT src_table, src_column, count(*), count(DISTINCT value_norm),
                avg(value_norm GLOB '*[a-z]*'),
                avg({numeric}),
                avg(({numeric} AND ({epoch}))
                    OR value_norm GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'),
                avg(value_norm GLOB '{uuid}'
                    OR (length(value_norm) >= 16 AND value_norm NOT GLOB '*[^0-9a-f]*')
                    OR (length(value_norm) >= 6 AND value_norm NOT GLOB '*[^0-9]*')),
                avg(is_doc),
                avg(length(value)),
                avg(value_norm NOT GLOB '* *')
         FROM lex_values
         GROUP BY src_table, src_column"
    );
    type Row = (String, String, i64, i64, f64, f64, f64, f64, f64, f64, f64);
    let profiles: Vec<Row> = conn
        .prepare(&sql)?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;

    conn.execute("DELETE FROM lex_columns", [])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO lex_columns (src_table, src_column, n_values, n_distinct,
             distinct_ratio, alpha_ratio, numeric_ratio, temporal_ratio,
             idlike_ratio, doc_ratio, avg_len, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let n = profiles.len();
    for (table, column, n_values, n_distinct, alpha, numeric, temporal, idlike, doc, avg_len, nospace) in
        profiles
    {
        let distinct_ratio = n_distinct as f64 / n_values.max(1) as f64;
        let kind = classify_kind(
            n_values,
            distinct_ratio,
            alpha,
            numeric,
            temporal,
            idlike,
            doc,
            nospace,
        );
        insert.execute(stemmadb::rusqlite::params![
            table,
            column,
            n_values,
            n_distinct,
            distinct_ratio,
            alpha,
            numeric,
            temporal,
            idlike,
            doc,
            avg_len,
            kind
        ])?;
    }
    Ok(n)
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

/// The drain's batch selection. Deliberately join-free: it reads only
/// embed_queue, and the `(status, attempts, id)` index satisfies both the
/// predicate and the ORDER BY, so SQLite walks the index in output order and
/// stops at the LIMIT — no temp B-tree, no per-pending-row work (issue #4
/// measured the joined-and-sorted version at 71% of drain wall-clock on a
/// 844K-item queue). A test asserts the plan stays index-only.
const DRAIN_BATCH_SQL: &str = "SELECT id, src_table, src_column, src_rowid, serialized
     FROM embed_queue
     WHERE status = 'pending'
     ORDER BY attempts, id
     LIMIT ?1";

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

/// Names the card serialization in force. Recorded in the `vec_interp`
/// `model_registry` row at drain time; [`enqueue_missing_interpretations`]
/// treats a store whose recorded format differs as holding stale vectors —
/// they embed card TEXT, so a text-format change strands them silently — and
/// drops + requeues, loudly. Bump this string whenever card text changes.
pub const INTERP_CARD_FORMAT: &str = "set-majority-v1";

/// Renders the interpretation card for one distinct value interpretation:
/// `"{table} · {column} · {value}"` plus up to two `col: value` context
/// fragments. Fragments are SET-LEVEL statistics of the rows sharing the
/// interpretation, never a sampled row's cells: issue #6 measured cards
/// perturbed by their `MIN(src_rowid)` representative drifting 5× further
/// (within-value spread 0.29) than the gap between different values (0.06),
/// and bare fragment-free cards BEATING fragmented ones on between-value
/// separation — a card that describes one arbitrary member is worse than a
/// card that describes nothing. Deterministic — fragments arrive in
/// column-name order and are appended only while the card stays within
/// [`INTERP_CARD_MAX_CHARS`].
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
/// document path. The representative is a CITATION KEY only — it never
/// influences card text, which is a function of the whole interpretation
/// (see [`interpretation_card`]).
///
/// This is the relational counterpart of the document queue: on value-shaped
/// corpora nothing crosses `DOC_MIN_LEN`, so a documents-only dense channel
/// is inert, and a value appearing in two columns needs column context to be
/// separable at all. The card carries that context.
///
/// Candidacy is restricted to columns `lex_columns` classified as `text`:
/// temporal, numeric, identifier and code columns recur across tables by
/// construction (denormalization copies them), not because their values are
/// ambiguous vocabulary, and no paraphrase query can ever retrieve them.
pub fn enqueue_missing_interpretations(db: &StemmaDb) -> Result<usize> {
    let conn = db.conn();
    let mut has_interp: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'vec_interp'",
        [],
        |r| r.get(0),
    )?;

    // Card format discipline: vec_interp holds embeddings of card TEXT, so a
    // serialization change strands every existing vector in a space no new
    // card will be written in. If the registry's recorded format differs
    // from [`INTERP_CARD_FORMAT`] (including the '' a pre-format store
    // reports), the honest move is wholesale: drop the vector table, retire
    // its registry row, and delete the queued interpretation items so this
    // pass rebuilds every card in the current format. Loud by design.
    if has_interp > 0 {
        let recorded: String = conn
            .query_row(
                "SELECT card_format FROM model_registry WHERE vector_table = 'vec_interp'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_default();
        if recorded != INTERP_CARD_FORMAT {
            tracing::warn!(
                recorded = %recorded,
                current = INTERP_CARD_FORMAT,
                "interpretation card format changed: dropping vec_interp and requeueing all cards"
            );
            conn.execute_batch(
                "DROP TABLE vec_interp;
                 DELETE FROM model_registry WHERE vector_table = 'vec_interp';
                 DELETE FROM embed_queue WHERE serialized != '';",
            )?;
            has_interp = 0;
        }
    }
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

    // One row per distinct interpretation. The displayed value is the MODAL
    // raw spelling over the rows sharing the value_norm (ties broken
    // lexicographically) — a property of the set, deterministic under any
    // insertion order — never the representative row's cell, which is kept
    // only as the citation key.
    //
    // Candidacy is column-typed. A card is consulted solely when the same
    // value ties across DISTINCT (table, column) readings — but on
    // denormalized schemas cross-column recurrence is dominated by *copied
    // data* (timestamps, join keys, SKUs repeated into fact tables), which is
    // structurally guaranteed to recur and can never be reached by a
    // natural-language paraphrase. So both the outer selection and the
    // recurrence subquery are restricted to columns lex_columns classified as
    // `text`, with a per-value letter guard for non-linguistic strays inside
    // otherwise-texty columns. Expectation: the queue holds the shared
    // *vocabulary* of the corpus — typically orders of magnitude below the
    // untyped predicate on warehouse shapes (issue #2: 846,989 cards, 90.4%
    // epoch floats, against 98 useful documents).
    let missing: Vec<(String, String, i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT t.src_table, t.src_column, t.rep, t.value_norm,
                    (SELECT v.value FROM lex_values v
                     WHERE v.src_table = t.src_table AND v.src_column = t.src_column
                       AND v.value_norm = t.value_norm AND v.is_doc = 0
                     GROUP BY v.value ORDER BY count(*) DESC, v.value LIMIT 1)
             FROM (
                 SELECT lv.src_table, lv.src_column, MIN(lv.src_rowid) AS rep, lv.value_norm
                 FROM lex_values lv
                 JOIN lex_columns lc ON lc.src_table = lv.src_table
                                    AND lc.src_column = lv.src_column
                 WHERE lv.is_doc = 0
                   AND lc.kind = 'text'
                   AND lv.value_norm GLOB '*[a-z]*'
                   AND lv.value_norm IN (
                       SELECT lv2.value_norm FROM lex_values lv2
                       JOIN lex_columns lc2 ON lc2.src_table = lv2.src_table
                                           AND lc2.src_column = lv2.src_column
                       WHERE lv2.is_doc = 0
                         AND lc2.kind = 'text'
                         AND lv2.value_norm GLOB '*[a-z]*'
                       GROUP BY lv2.value_norm
                       HAVING count(DISTINCT lv2.src_table || '·' || lv2.src_column) >= 2
                   )
                 GROUP BY lv.src_table, lv.src_column, lv.value_norm
             ) t
             WHERE NOT EXISTS (
                 SELECT 1 FROM covered_interp c
                 WHERE c.src_table = t.src_table
                   AND c.src_column = t.src_column
                   AND c.src_rowid = t.rep)
             ORDER BY t.src_table, t.src_column, t.rep",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    let mut queued = 0usize;
    {
        // Set-level fragments: for each OTHER paraphrasable (`text`-kind,
        // non-doc) column of the same table, the modal value across ALL rows
        // sharing the interpretation — included only when that mode is a
        // strict majority (> 50% of the column's non-null values in the set;
        // a definition, not a tunable — and one that makes the winner unique,
        // so no tie-break is ever exercised). A column with no majority
        // contributes nothing, and an interpretation with no majorities gets
        // a bare `table · column · value` card: issue #6 measured bare cards
        // beating single-row-fragment cards on between-value separation, so
        // omission is strictly better than sampled noise. The whole
        // computation is a GROUP BY — deterministic, insertion-order-free.
        let mut frag_stmt = conn.prepare_cached(
            "SELECT src_column, value FROM (
                 SELECT o.src_column AS src_column, o.value AS value,
                        count(*) AS n,
                        sum(count(*)) OVER (PARTITION BY o.src_column) AS total
                 FROM lex_values me
                 JOIN lex_values o
                   ON o.src_table = me.src_table AND o.src_rowid = me.src_rowid
                 JOIN lex_columns oc
                   ON oc.src_table = o.src_table AND oc.src_column = o.src_column
                 WHERE me.src_table = ?1 AND me.src_column = ?2
                   AND me.value_norm = ?3 AND me.is_doc = 0
                   AND o.src_column != ?2 AND o.is_doc = 0
                   AND oc.kind = 'text'
                 GROUP BY o.src_column, o.value)
             WHERE 2 * n > total
             ORDER BY src_column",
        )?;
        let mut insert = conn.prepare_cached(
            "INSERT INTO embed_queue (src_table, src_column, src_rowid, serialized)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (src_table, src_column, src_rowid) DO UPDATE SET
                 status = 'pending', attempts = 0, error = '', serialized = ?4,
                 updated_at = datetime('now')
             WHERE embed_queue.status = 'done'",
        )?;
        for (table, column, rep, value_norm, value) in missing {
            let fragments: Vec<(String, String)> = frag_stmt
                .query_map(
                    stemmadb::rusqlite::params![table, column, value_norm],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
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
/// Documents and cards are embedded RAW — the asymmetric convention of
/// instruction-tuned retrieval encoders puts the template on the query side
/// only ([`Embedder::format_query`]); applying it here would desert the
/// vector space the queries live in.
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

    // One batch of pending work, least-retried first. Selection touches
    // embed_queue alone: the composite (status, attempts, id) index yields
    // the ORDER BY directly, so the scan stops at `batch` rows instead of
    // sorting — and joining — every pending row per batch (issue #4). The
    // payload lookup joins AFTER selection, for just the chosen rows.
    let mut items: Vec<(i64, String, String, i64, String)> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(DRAIN_BATCH_SQL)?;
        let rows = stmt.query_map([batch as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        for row in rows {
            items.push(row?);
        }
    }

    // `serialized` is the text to embed when set — the interpretation card,
    // which also routes the vector to vec_interp; documents leave it empty
    // and their text is fetched from lex_values here (per selected item, not
    // per pending item) so the store holds each document once. Items whose
    // source text is gone (index rebuilt out from under the queue) cannot be
    // embedded, now or later; interpretation items carry their card in the
    // queue, so only document items can go missing.
    let mut texts = Vec::new();
    let mut work: Vec<(i64, String, String, i64, &'static str)> = Vec::new();
    let mut doc_text = conn.prepare_cached(
        "SELECT value FROM lex_values
         WHERE src_table = ?1 AND src_column = ?2 AND src_rowid = ?3",
    )?;
    for (id, table, column, rowid, serialized) in items {
        let text = if serialized.is_empty() {
            doc_text
                .query_row(stemmadb::rusqlite::params![table, column, rowid], |r| {
                    r.get::<_, String>(0)
                })
                .ok()
        } else {
            Some(serialized.clone())
        };
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
            // The registry row records the whole identity of the space:
            // model, dimension, the query-side template the embedder pairs
            // with its vectors, and — for vec_interp — the card format its
            // vectors were serialized under (enqueue invalidates on
            // mismatch).
            conn.execute(
                "INSERT INTO model_registry (vector_table, backend, model, dimension,
                                             quantization, query_template, card_format)
                 VALUES (?1, ?2, ?3, ?4, 'f32', ?5, ?6)
                 ON CONFLICT(vector_table) DO NOTHING",
                stemmadb::rusqlite::params![
                    target,
                    identity.backend,
                    identity.model,
                    dim as i64,
                    identity.query_template,
                    if target == "vec_interp" {
                        INTERP_CARD_FORMAT
                    } else {
                        ""
                    },
                ],
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

    /// One column of each kind, 24 rows per table so the cardinality-gated
    /// rules (identifier-by-distinctness, code) are live. All columns are
    /// TEXT-typed — the CSV-import shape where epoch floats, dates and keys
    /// arrive as strings, which is exactly when typology matters.
    fn typology_db() -> StemmaDb {
        let db = StemmaDb::open_in_memory().unwrap();
        let conn = db.conn();
        conn.execute_batch(
            "CREATE TABLE src.events(id INTEGER PRIMARY KEY,
                 occurred_at TEXT, day TEXT, amount TEXT);
             CREATE TABLE src.assets(id INTEGER PRIMARY KEY,
                 uid TEXT, sku TEXT, name TEXT, body TEXT);
             CREATE TABLE src.parents(id INTEGER PRIMARY KEY, pid TEXT);
             CREATE TABLE src.children(id INTEGER PRIMARY KEY, parent_id TEXT);",
        )
        .unwrap();
        let names = ["Harbor Crane", "North Pier", "Dock Office"];
        let amounts = ["19.99", "24.50", "5", "120"];
        for i in 0i64..24 {
            conn.execute(
                "INSERT INTO src.events (occurred_at, day, amount) VALUES (?1, ?2, ?3)",
                stemmadb::rusqlite::params![
                    format!("{}.{:03}", 1_700_000_000 + i * 9973, i),
                    format!("2024-{:02}-{:02}", i % 12 + 1, i % 28 + 1),
                    amounts[i as usize % amounts.len()],
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO src.assets (uid, sku, name, body) VALUES (?1, ?2, ?3, ?4)",
                stemmadb::rusqlite::params![
                    format!("{i:08x}-{i:04x}-{i:04x}-{i:04x}-{i:012x}"),
                    format!("SKU-{i:04}-Q{}", i % 7),
                    names[i as usize % names.len()],
                    format!(
                        "Operating notes for asset {i}: the body repeats procedural \
                         language until it comfortably crosses the two-hundred \
                         character document threshold, so the classifier files it \
                         as a document rather than a value, as any manual page \
                         or long free-text field would be."
                    ),
                ],
            )
            .unwrap();
            // The same digit key stored in two tables — the FK shape.
            let key = (1000 + i).to_string();
            conn.execute("INSERT INTO src.parents (pid) VALUES (?1)", [&key])
                .unwrap();
            conn.execute("INSERT INTO src.children (parent_id) VALUES (?1)", [&key])
                .unwrap();
        }
        build_lexical_index(&db, false).unwrap();
        db
    }

    #[test]
    fn column_kinds_classify_the_typology_fixture() {
        let db = typology_db();
        let kind = |t: &str, c: &str| -> String {
            db.conn()
                .query_row(
                    "SELECT kind FROM lex_columns WHERE src_table = ?1 AND src_column = ?2",
                    [t, c],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(kind("events", "occurred_at"), "temporal", "epoch floats");
        assert_eq!(kind("events", "day"), "temporal", "ISO dates");
        assert_eq!(kind("events", "amount"), "numeric", "plain decimals");
        assert_eq!(kind("assets", "uid"), "identifier", "uuids");
        assert_eq!(kind("assets", "sku"), "code", "SKU-shaped tokens");
        assert_eq!(kind("assets", "name"), "text", "recurring names");
        assert_eq!(kind("assets", "body"), "document", "long bodies");
        // Digit keys are numeric-shaped before anything else; both sides of
        // the copied-key pair classify away from 'text' identically.
        assert_eq!(kind("parents", "pid"), "numeric", "digit keys");
        assert_eq!(kind("children", "parent_id"), "numeric", "copied digit keys");

        // Ratio spot checks: the profile stores what the kinds derive from.
        let (n, distinct, temporal): (i64, f64, f64) = db
            .conn()
            .query_row(
                "SELECT n_values, distinct_ratio, temporal_ratio FROM lex_columns
                 WHERE src_table = 'events' AND src_column = 'occurred_at'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(n, 24);
        assert_eq!(distinct, 1.0);
        assert_eq!(temporal, 1.0);
    }

    /// Issue #2 in miniature: a denormalized warehouse whose fact table
    /// copies timestamps, keys and SKUs from its dimension tables. Under the
    /// untyped recurrence predicate every copied column qualified as
    /// "ambiguous vocabulary" (846,989 cards on the reported dataset, 90.4%
    /// epoch floats); column-typed candidacy must admit only the text
    /// columns' shared vocabulary.
    fn warehouse_db() -> StemmaDb {
        let db = StemmaDb::open_in_memory().unwrap();
        let conn = db.conn();
        conn.execute_batch(
            "CREATE TABLE src.products(id TEXT, name TEXT, brand TEXT,
                 category TEXT, sku TEXT);
             CREATE TABLE src.orders(id TEXT, user_id TEXT,
                 created_at TEXT, shipped_at TEXT);
             CREATE TABLE src.order_items(id TEXT, order_id TEXT, product_id TEXT,
                 product_name TEXT, brand TEXT, category TEXT, sku TEXT,
                 created_at TEXT, shipped_at TEXT);",
        )
        .unwrap();
        let brands = ["allegra k", "calvin klein", "levi's", "carhartt", "nike"];
        let cats = ["Outerwear & Coats", "Dresses", "Suits & Sport Coats", "Jeans"];
        for i in 0i64..40 {
            conn.execute(
                "INSERT INTO src.products VALUES (?1, ?2, ?3, ?4, ?5)",
                stemmadb::rusqlite::params![
                    (i + 1).to_string(),
                    format!("product {i}"),
                    brands[i as usize % brands.len()],
                    cats[i as usize % cats.len()],
                    format!("{:032x}", 0x9e3779b97f4a7c15u64 ^ (i as u64) << 17),
                ],
            )
            .unwrap();
        }
        for i in 0i64..80 {
            let created = format!("{}.{:07}", 1_700_000_000 + i * 9973, i * 13);
            let shipped = format!("{}.{:07}", 1_700_086_400 + i * 9973, i * 13);
            conn.execute(
                "INSERT INTO src.orders VALUES (?1, ?2, ?3, ?4)",
                stemmadb::rusqlite::params![
                    (i + 1).to_string(),
                    (i % 30).to_string(),
                    created,
                    shipped
                ],
            )
            .unwrap();
            let p = i % 40;
            // Denormalized: the same name/brand/category/sku/instants, copied.
            conn.execute(
                "INSERT INTO src.order_items
                 SELECT ?1, ?1, p.id, p.name, p.brand, p.category, p.sku, ?2, ?3
                 FROM src.products p WHERE p.id = ?4",
                stemmadb::rusqlite::params![
                    (i + 1).to_string(),
                    created,
                    shipped,
                    (p + 1).to_string()
                ],
            )
            .unwrap();
        }
        build_lexical_index(&db, false).unwrap();
        db
    }

    #[test]
    fn warehouse_candidacy_excludes_copied_timestamps_keys_and_skus() {
        let db = warehouse_db();

        // The untyped predicate from before issue #2, for contrast: copied
        // epochs, keys and SKUs all recur across (table, column) pairs by
        // construction, so it floods.
        let untyped: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM (
                     SELECT src_table, src_column, value_norm FROM lex_values
                     WHERE is_doc = 0
                       AND value_norm IN (
                           SELECT value_norm FROM lex_values WHERE is_doc = 0
                           GROUP BY value_norm
                           HAVING count(DISTINCT src_table || '·' || src_column) >= 2)
                     GROUP BY src_table, src_column, value_norm)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(untyped > 800, "the untyped predicate floods: {untyped}");

        let queued = enqueue_missing_interpretations(&db).unwrap();
        println!("warehouse candidacy: untyped predicate {untyped} cards, typed {queued}");
        // Exactly the shared text vocabulary: 40 names × 2 columns + 5
        // brands × 2 + 4 categories × 2.
        assert_eq!(queued, 98, "untyped predicate would enqueue {untyped}");
        let non_text: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM embed_queue q
                 WHERE (q.src_table, q.src_column) NOT IN (VALUES
                     ('products', 'name'), ('products', 'brand'),
                     ('products', 'category'), ('order_items', 'product_name'),
                     ('order_items', 'brand'), ('order_items', 'category'))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            non_text, 0,
            "no cards for timestamps, keys, SKUs or any other copied column"
        );
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
        template: String,
    }

    impl FakeEmbedder {
        fn new(dim: usize) -> Self {
            Self {
                dim,
                fail: false,
                template: String::new(),
            }
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
                query_template: self.template.clone(),
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
                 CREATE TABLE src.tags(id INTEGER PRIMARY KEY, label TEXT);
                 INSERT INTO src.tags VALUES
                    (1, 'Coastal permits'), (2, 'Insurance filings'), (3, 'Water rights');
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
        let broken = FakeEmbedder {
            dim: 8,
            fail: true,
            template: String::new(),
        };
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
                    (19, 'Portland Airport', 'Portland');
                 -- landmarks shares values with offices so the fixture has an
                 -- ambiguous vocabulary: only cross-column values get cards
                 CREATE TABLE src.landmarks(id INTEGER PRIMARY KEY, title TEXT);
                 INSERT INTO src.landmarks VALUES
                    (1, 'Seattle'), (2, 'Portland'), (3, 'Portland Downtown');",
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

    /// Issue #4's regression guard: the drain's batch selection must be
    /// answered by walking the (status, attempts, id) index in output order —
    /// any temp B-tree in the plan means SQLite is re-sorting the whole
    /// pending queue per batch.
    #[test]
    fn drain_batch_selection_plan_has_no_temp_btree() {
        let db = value_db();
        enqueue_missing_interpretations(&db).unwrap();
        let plan: Vec<String> = db
            .conn()
            .prepare(&format!("EXPLAIN QUERY PLAN {DRAIN_BATCH_SQL}"))
            .unwrap()
            .query_map([EMBED_BATCH as i64], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        let plan = plan.join("\n");
        assert!(
            !plan.to_uppercase().contains("TEMP B-TREE"),
            "batch selection must not materialize a sort:\n{plan}"
        );
        assert!(
            plan.contains("embed_queue_status_attempts_id"),
            "batch selection walks the composite index:\n{plan}"
        );
    }

    /// A corpus for the set-level card semantics: two tables sharing a
    /// category vocabulary, a brand column with one clear majority
    /// ('Carhartt' on 3 of 4 Outerwear rows) and one exact 50/50 split (the
    /// Dresses rows), plus a constant epoch column that no card may quote.
    /// `rows` controls both insertion order and which row wins MIN(rowid).
    fn card_fixture(rows: &[(i64, &str, &str)]) -> StemmaDb {
        let db = StemmaDb::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE src.items(id INTEGER PRIMARY KEY,
                     category TEXT, brand TEXT, created_at TEXT);
                 CREATE TABLE src.tags(id INTEGER PRIMARY KEY, label TEXT);
                 INSERT INTO src.tags VALUES (1, 'Outerwear & Coats'), (2, 'Dresses');",
            )
            .unwrap();
        for (id, category, brand) in rows {
            db.conn()
                .execute(
                    "INSERT INTO src.items VALUES (?1, ?2, ?3, '1700000000.0')",
                    stemmadb::rusqlite::params![id, category, brand],
                )
                .unwrap();
        }
        build_lexical_index(&db, false).unwrap();
        db
    }

    fn cards_of(db: &StemmaDb) -> Vec<(String, String, String)> {
        enqueue_missing_interpretations(db).unwrap();
        db.conn()
            .prepare(
                "SELECT src_table, src_column, serialized FROM embed_queue
                 WHERE serialized != ''
                 ORDER BY src_table, src_column, serialized",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    /// Issue #6's core complaint: card text depended on which row happened to
    /// win MIN(src_rowid). The same logical corpus, built in two insertion
    /// orders with the primary keys permuted so a DIFFERENT row is the
    /// representative each time, must produce byte-identical cards.
    #[test]
    fn cards_are_identical_across_insertion_orders() {
        // Forward: the Carhartt rows lead; MIN-rowid Outerwear row is Carhartt,
        // MIN-rowid Dresses row is Levi's.
        let forward = card_fixture(&[
            (1, "Outerwear & Coats", "Carhartt"),
            (2, "Outerwear & Coats", "Carhartt"),
            (3, "Outerwear & Coats", "Carhartt"),
            (4, "Outerwear & Coats", "Columbia"),
            (5, "Dresses", "Levi's"),
            (6, "Dresses", "Oakley"),
        ]);
        // Permuted: inserted in reverse, ids reassigned so the representative
        // Outerwear row is now Columbia and the representative Dresses row is
        // Oakley — the exact perturbation that used to rewrite the card.
        let permuted = card_fixture(&[
            (6, "Dresses", "Levi's"),
            (5, "Dresses", "Oakley"),
            (4, "Outerwear & Coats", "Carhartt"),
            (3, "Outerwear & Coats", "Carhartt"),
            (2, "Outerwear & Coats", "Carhartt"),
            (1, "Outerwear & Coats", "Columbia"),
        ]);
        let (a, b) = (cards_of(&forward), cards_of(&permuted));
        assert_eq!(a, b, "cards are a function of the set, not of row order");

        let card = |table: &str, column: &str, needle: &str| -> String {
            a.iter()
                .map(|(t, c, s)| (t.as_str(), c.as_str(), s.as_str()))
                .find(|(t, c, s)| *t == table && *c == column && s.contains(needle))
                .map(|(_, _, s)| s.to_string())
                .unwrap_or_else(|| panic!("no {table}.{column} card containing {needle:?}"))
        };
        // Strict majority (3/4): the modal brand is included.
        assert_eq!(
            card("items", "category", "Outerwear"),
            "items · category · Outerwear & Coats · brand: Carhartt"
        );
        // Exact half is not a majority: the fragment is omitted and the card
        // falls back to bare — which issue #6 measured beating a sampled
        // fragment on between-value separation.
        assert_eq!(
            card("items", "category", "Dresses"),
            "items · category · Dresses"
        );
        // The constant epoch column is temporal-kind, not paraphrasable: even
        // a 100% mode never becomes a fragment.
        for (_, _, s) in &a {
            assert!(!s.contains("created_at"), "no temporal fragments: {s}");
        }
        // Single-column tables have nothing set-level to say: bare cards.
        assert_eq!(card("tags", "label", "Dresses"), "tags · label · Dresses");
    }

    /// Cards embed card TEXT, so changing the serialization strands existing
    /// vec_interp vectors. Enqueue must notice a foreign recorded format,
    /// drop the table, retire its registry row and requeue every card.
    #[test]
    fn card_format_change_invalidates_vec_interp_and_requeues() {
        let db = value_db();
        let embedder = FakeEmbedder::new(8);
        enqueue_missing_interpretations(&db).unwrap();
        drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        let fmt: String = db
            .conn()
            .query_row(
                "SELECT card_format FROM model_registry WHERE vector_table = 'vec_interp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fmt, INTERP_CARD_FORMAT,
            "drain records the format it embedded"
        );

        // Simulate a store written under an older card serialization.
        db.conn()
            .execute(
                "UPDATE model_registry SET card_format = 'row-sample-v0'
                 WHERE vector_table = 'vec_interp'",
                [],
            )
            .unwrap();
        let requeued = enqueue_missing_interpretations(&db).unwrap();
        assert_eq!(requeued, 6, "every interpretation is requeued");
        assert_eq!(queue_counts(&db), (6, 0, 0));
        let gone: i64 = db
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM sqlite_master WHERE name = 'vec_interp')
                        + (SELECT count(*) FROM model_registry WHERE vector_table = 'vec_interp')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gone, 0, "stale vectors and their registry row are gone");

        // The next drain rebuilds the table under the current format.
        let stats = drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!((stats.drained, stats.failed, stats.remaining), (6, 0, 0));
        let (n, fmt): (i64, String) = db
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM vec_interp), card_format
                 FROM model_registry WHERE vector_table = 'vec_interp'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((n, fmt.as_str()), (6, INTERP_CARD_FORMAT));
    }

    /// Issue #5: the query template travels with the model identity — the
    /// backend renders queries through it, and the drain records it in the
    /// registry beside the model it must agree with.
    #[test]
    fn drain_records_the_query_template_in_the_registry() {
        let db = value_db();
        let embedder = FakeEmbedder {
            dim: 8,
            fail: false,
            template: "Find rows for: {query}".into(),
        };
        assert_eq!(embedder.format_query("socks"), "Find rows for: socks");
        assert_eq!(FakeEmbedder::new(8).format_query("socks"), "socks");
        enqueue_missing_interpretations(&db).unwrap();
        drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        let template: String = db
            .conn()
            .query_row(
                "SELECT query_template FROM model_registry WHERE vector_table = 'vec_interp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(template, "Find rows for: {query}");
    }

    #[test]
    fn enqueue_finds_interpretations_exactly_once() {
        let db = value_db();
        let queued = enqueue_missing_interpretations(&db).unwrap();
        // Only the ambiguous vocabulary is card-worthy: 'seattle',
        // 'portland' and 'portland downtown' each live in two columns
        // (offices + landmarks), so 3 norms x 2 readings = 6 items; the
        // single-column names ('Seattle - Northgate', 'Portland Airport')
        // are excluded. Repeated 'Portland' cities stay one interpretation,
        // keyed by the representative MIN rowid.
        assert_eq!(queued, 6);
        assert_eq!(queue_counts(&db), (6, 0, 0));
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
        assert_eq!((stats.drained, stats.failed, stats.remaining), (6, 0, 0));

        let vectors: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM vec_interp", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 6);
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
        assert_eq!(vectors_again, 6, "idempotent: no duplicate vectors");
    }

    #[test]
    fn mixed_queue_routes_docs_and_interps_to_their_tables() {
        // doc_db has three document bodies AND three short titles: one queue,
        // two vector tables, one drain.
        let db = doc_db();
        let embedder = FakeEmbedder::new(8);
        let docs = enqueue_missing_embeddings(&db).unwrap();
        let interps = enqueue_missing_interpretations(&db).unwrap();
        // titles are shared with src.tags, so each title yields two readings
        assert_eq!((docs, interps), (3, 6));
        let stats = drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!((stats.drained, stats.failed, stats.remaining), (9, 0, 0));
        let count = |table: &str| -> i64 {
            db.conn()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!((count("vec_dense"), count("vec_interp")), (3, 6));
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
        assert_eq!((pending, done, failed), (0, 0, 6));
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
