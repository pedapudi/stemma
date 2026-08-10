//! stemma-ingest: builds derived indexes in the .stemmadb store from the
//! attached (read-only) user database.
//!
//! Current scope: the lexical value index — every text value of every user
//! table, indexed three ways: normalized for exact lookup, FTS5/unicode61 for
//! BM25 token search, FTS5/trigram for fuzzy and substring matching — plus
//! the column measurement profile the purpose predicates read, and the embed
//! queue the dense channel drains.
//!
//! Two disciplines govern everything here:
//!
//! - **Configuration becomes derivation.** No guessed thresholds: document
//!   boundaries are derived per corpus from the length distribution (Otsu's
//!   natural break over column median log-lengths), and shape judgments use
//!   Jeffreys lower confidence bounds so small samples yield weak claims
//!   instead of firing gates. `lex_columns` stores measurements; predicates
//!   ([`is_document_column`], [`is_paraphrasable_column`],
//!   [`is_vocabulary_column`]) interpret them.
//! - **The refresh discipline.** Every derived artifact records a receipt in
//!   `derivations` — what inputs (fingerprint) and what algorithm
//!   (derivation version) produced it. [`build_lexical_index`] compares
//!   receipts against per-table content fingerprints and re-ingests only
//!   what changed; embed-queue items carry a content hash so a changed row
//!   re-embeds without a rebuild.

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
    #[error(
        "{table} is registered with query template {registered:?} but the \
         embedder offers {offered:?}; one convention's queries in another \
         convention's space is the same corruption as mixing models — refusing"
    )]
    QueryTemplateMismatch {
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
///
/// This constant survives the derivation pass on purpose. It is not an
/// epistemic guess about the corpus — it is a transport/UX bound on the
/// *exact channel*: what a user's mention can literally equal, what a
/// candidate can carry over the wire unabridged, what an evidence line can
/// display. Because it defines where "being a value" operationally ends, it
/// also anchors the document derivation: see [`derive_doc_boundary`].
pub const EXACT_MAX_LEN: usize = 120;

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStats {
    pub tables: usize,
    pub text_columns: usize,
    pub values: usize,
    pub rebuilt: bool,
    /// Tables whose lexical rows were (re-)ingested by this call; 0 means
    /// every fingerprint matched its receipt and only profiles were checked.
    pub reingested_tables: usize,
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
CREATE INDEX IF NOT EXISTS lex_values_col ON lex_values(src_table, src_column);

-- Column measurements: one row per indexed (table, column), derived from
-- lex_values by profile_columns(). Measurements only — no classifications.
-- Each shape ratio is paired with its Jeffreys lower confidence bound at
-- CONFIDENCE_LEVEL, so consumers can require *confident* majorities and a
-- five-row column naturally makes no strong claims. The purpose predicates
-- (is_document_column, is_paraphrasable_column, is_vocabulary_column) are
-- functions over these rows, not stored labels. Same lifecycle as the rest
-- of the lexical index — dropped and rebuilt with it, never migrated.
CREATE TABLE IF NOT EXISTS lex_columns (
    src_table      TEXT NOT NULL,
    src_column     TEXT NOT NULL,
    n_values       INTEGER NOT NULL,
    n_distinct     INTEGER NOT NULL,
    distinct_ratio REAL NOT NULL,
    distinct_lcb   REAL NOT NULL,
    alpha_ratio    REAL NOT NULL,
    alpha_lcb      REAL NOT NULL,
    numeric_ratio  REAL NOT NULL,
    numeric_lcb    REAL NOT NULL,
    temporal_ratio REAL NOT NULL,
    temporal_lcb   REAL NOT NULL,
    idlike_ratio   REAL NOT NULL,
    idlike_lcb     REAL NOT NULL,
    avg_len        REAL NOT NULL,
    median_len     REAL NOT NULL,
    PRIMARY KEY (src_table, src_column)
) STRICT;

-- Provenance receipts for derived state: which inputs (fingerprint) and
-- which algorithm (derivation_version) produced each artifact, when, and —
-- for scalar derivations like the document cut — the derived value itself.
-- Artifacts today: 'lex:{table}' (that table's lexical rows), 'profiles'
-- (lex_columns as a whole), 'doc_cut' (the document boundary, holding both
-- the freshly derived and the adopted value; see profile_columns).
-- The knowledge compiler's kg_meta table is the same discipline with the
-- version folded into the fingerprint tag; it predates this table and is
-- deliberately not migrated into it.
CREATE TABLE IF NOT EXISTS derivations (
    artifact           TEXT PRIMARY KEY,
    input_fingerprint  TEXT NOT NULL,
    derivation_version INTEGER NOT NULL,
    derived_at         TEXT NOT NULL DEFAULT (datetime('now')),
    value_json         TEXT NOT NULL DEFAULT '{}'
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

/// Version of the lexical derivation algorithms. Recorded in every receipt;
/// bumping it invalidates all lexical receipts at once, so an algorithm
/// change re-derives everything on the next registration without any
/// migration machinery. v2: measurement profiles + derived document
/// boundary replaced the fixed-threshold kind ladder.
pub const LEX_DERIVATION_VERSION: i64 = 2;

/// Builds — or incrementally refreshes — the lexical index. Each table's
/// content fingerprint is compared against its `derivations` receipt: only
/// changed (or new, or receipt-less) tables re-ingest, and profiles plus the
/// document boundary re-derive whenever any table moved. `force` treats
/// every table as changed. Unchanged corpora cost one aggregate scan per
/// table and no writes.
pub fn build_lexical_index(db: &StemmaDb, force: bool) -> Result<IndexStats> {
    let start = std::time::Instant::now();
    let conn = db.conn();

    // Shape self-healing (derived state, never migrated): stores from before
    // document classification lack lex_values.is_doc — drop the whole index;
    // stores from the kind-ladder era lack lex_columns.median_len — drop just
    // the profile table, which re-derives from lex_values without a reindex.
    let has_column = |table: &str, column: &str| -> bool {
        conn.query_row(
            "SELECT (SELECT count(*) FROM sqlite_master WHERE name = ?1)
                    AND (SELECT count(*) FROM pragma_table_info(?1) WHERE name = ?2)",
            [table, column],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    };
    let lex_exists: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'lex_values'",
        [],
        |r| r.get(0),
    )?;
    if lex_exists > 0 && !has_column("lex_values", "is_doc") {
        conn.execute_batch(
            "DROP TABLE lex_values;
             DROP TABLE IF EXISTS lex_fts;
             DROP TABLE IF EXISTS lex_trigram;
             DROP TABLE IF EXISTS lex_columns;",
        )?;
    }
    let lc_exists: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'lex_columns'",
        [],
        |r| r.get(0),
    )?;
    if lc_exists > 0 && !has_column("lex_columns", "median_len") {
        conn.execute_batch("DROP TABLE lex_columns;")?;
    }
    conn.execute_batch(LEX_SCHEMA)?;

    let columns = text_columns(db)?;
    let mut by_table: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for tc in &columns {
        by_table
            .entry(tc.table.clone())
            .or_default()
            .push(tc.column.clone());
    }

    // Change detection: per-table content fingerprint vs. stored receipt.
    let mut dirty: Vec<(String, String)> = Vec::new(); // (table, fingerprint)
    for table in by_table.keys() {
        let fp = db.src_table_fingerprint(table)?;
        let stored = read_receipt(conn, &format!("lex:{table}"))?;
        if force || stored.as_deref() != Some(fp.as_str()) {
            dirty.push((table.clone(), fp));
        }
    }
    // Dropped (or no-longer-text-bearing) tables leave stale rows; sweep
    // them. Indexed tables are known through their receipts, unioned with
    // lex_values itself so pre-receipt stores sweep correctly too.
    let known: Vec<String> = conn
        .prepare(
            "SELECT substr(artifact, 5) FROM derivations WHERE artifact LIKE 'lex:%'
             UNION SELECT DISTINCT src_table FROM lex_values",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let gone: Vec<String> = known
        .into_iter()
        .filter(|t| !by_table.contains_key(t))
        .collect();

    let tx = conn.unchecked_transaction()?;
    for table in &gone {
        remove_table_rows(conn, table)?;
        conn.execute(
            "DELETE FROM derivations WHERE artifact = ?1",
            [format!("lex:{table}")],
        )?;
    }
    for (table, fp) in &dirty {
        ingest_table(conn, table, &by_table[table])?;
        write_receipt(conn, &format!("lex:{table}"), fp, "{}")?;
    }
    tx.commit()?;

    // Profiles and the document boundary are corpus-level: they re-derive
    // whenever any table moved, or when their own receipt is stale (new
    // store, or LEX_DERIVATION_VERSION bumped).
    let corpus_fp = corpus_fingerprint(conn)?;
    if !dirty.is_empty() || !gone.is_empty() || read_receipt(conn, "profiles")? != Some(corpus_fp)
    {
        profile_columns(db)?;
    }

    let values: i64 = conn.query_row("SELECT count(*) FROM lex_values", [], |r| r.get(0))?;
    Ok(IndexStats {
        tables: by_table.len(),
        text_columns: columns.len(),
        values: values as usize,
        rebuilt: !dirty.is_empty() || !gone.is_empty(),
        reingested_tables: dirty.len(),
        elapsed_ms: start.elapsed().as_millis(),
    })
}

/// Removes one table's rows from lex_values and both FTS mirrors. The FTS
/// tables are external-content, so each row must be deleted *with its text*
/// (the special `'delete'` command) while the content row still exists.
fn remove_table_rows(conn: &stemmadb::rusqlite::Connection, table: &str) -> Result<()> {
    for fts in ["lex_fts", "lex_trigram"] {
        conn.execute(
            &format!(
                "INSERT INTO {fts}({fts}, rowid, value)
                 SELECT 'delete', id, value FROM lex_values WHERE src_table = ?1"
            ),
            [table],
        )?;
    }
    conn.execute("DELETE FROM lex_values WHERE src_table = ?1", [table])?;
    Ok(())
}

/// Re-ingests one table: reads the previous per-column `is_doc` stamps,
/// removes the table's rows, then inserts the fresh cells carrying those
/// stamps forward (new columns default to 0). `is_doc` is re-stamped per
/// column by profile_columns afterwards; preserving the stamp across the
/// re-ingest means an unchanged classification causes no churn downstream.
fn ingest_table(
    conn: &stemmadb::rusqlite::Connection,
    table: &str,
    cols: &[String],
) -> Result<usize> {
    let prior: std::collections::HashMap<String, i64> = conn
        .prepare("SELECT DISTINCT src_column, is_doc FROM lex_values WHERE src_table = ?1")?
        .query_map([table], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    remove_table_rows(conn, table)?;
    let mut values = 0usize;
    for col in cols {
        // Identifiers come from sqlite_master/table_info, quoted defensively.
        let sql = format!(
            "INSERT INTO lex_values (src_table, src_column, src_rowid, value, value_norm, is_doc)
             SELECT ?1, ?2, rowid, \"{col}\", lower(trim(\"{col}\")), ?3
             FROM {src}.\"{table}\"
             WHERE \"{col}\" IS NOT NULL AND trim(\"{col}\") != ''",
            src = SRC_SCHEMA,
        );
        values += conn.execute(
            &sql,
            stemmadb::rusqlite::params![table, col, prior.get(col).copied().unwrap_or(0)],
        )?;
    }
    for fts in ["lex_fts", "lex_trigram"] {
        conn.execute(
            &format!(
                "INSERT INTO {fts}(rowid, value)
                 SELECT id, value FROM lex_values WHERE src_table = ?1"
            ),
            [table],
        )?;
    }
    Ok(values)
}

// --- Provenance receipts ---------------------------------------------------

/// Reads the `derivations` receipt for one artifact, returning its input
/// fingerprint only when the derivation version also matches — a receipt
/// from an older algorithm is stale by definition.
fn read_receipt(
    conn: &stemmadb::rusqlite::Connection,
    artifact: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT input_fingerprint FROM derivations
             WHERE artifact = ?1 AND derivation_version = ?2",
            stemmadb::rusqlite::params![artifact, LEX_DERIVATION_VERSION],
            |r| r.get(0),
        )
        .ok())
}

fn write_receipt(
    conn: &stemmadb::rusqlite::Connection,
    artifact: &str,
    fingerprint: &str,
    value_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO derivations
             (artifact, input_fingerprint, derivation_version, derived_at, value_json)
         VALUES (?1, ?2, ?3, datetime('now'), ?4)
         ON CONFLICT(artifact) DO UPDATE SET
             input_fingerprint = ?2, derivation_version = ?3,
             derived_at = datetime('now'), value_json = ?4",
        stemmadb::rusqlite::params![artifact, fingerprint, LEX_DERIVATION_VERSION, value_json],
    )?;
    Ok(())
}

/// Fingerprint of the whole ingested corpus: the per-table receipts folded
/// through a content hash. Cheap, and exactly as fresh as the receipts.
fn corpus_fingerprint(conn: &stemmadb::rusqlite::Connection) -> Result<String> {
    let joined: Option<String> = conn.query_row(
        "SELECT group_concat(artifact || '=' || input_fingerprint, ';')
         FROM (SELECT artifact, input_fingerprint FROM derivations
               WHERE artifact LIKE 'lex:%' ORDER BY artifact)",
        [],
        |r| r.get(0),
    )?;
    Ok(content_hash(joined.as_deref().unwrap_or("")))
}

// --- Column measurements and purpose predicates ----------------------------
//
// The lexical index records what values exist; `lex_columns` records
// per-column MEASUREMENTS — counts, shape ratios with confidence bounds,
// length statistics. What used to be a stored six-kind classification is now
// three exported predicates, each a documented function over the
// measurements, evaluated where it is consumed. The structural shape tests
// (parses as a number, uuid/hex, epoch ranges, ISO dates) are definitions,
// not tunables, and remain fixed; every *threshold* has been replaced by a
// derivation (the document boundary) or by the symmetric point of a
// confident majority (LCB > 1/2).

/// Numeric values inside either range read as epoch timestamps:
/// seconds (1e8..4e9 ≈ 1973..2096) and milliseconds (1e11..4e12).
pub const TEMPORAL_EPOCH_RANGES: [(f64, f64); 2] = [(1e8, 4e9), (1e11, 4e12)];

/// The one conventional confidence level for every uncertainty-aware ratio
/// test: shape claims about a column are made at a Jeffreys lower confidence
/// bound of this level. 95% is the statistical convention, not a tuned
/// value; it is the only probability constant in the crate.
pub const CONFIDENCE_LEVEL: f64 = 0.95;

/// FNV-1a hash of a text, hex-encoded — the content hash carried by
/// embed-queue items and folded into corpus fingerprints. Not
/// cryptographic; it only needs to make "did this text change?" cheap.
pub fn content_hash(text: &str) -> String {
    let mut state: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        state ^= *b as u64;
        state = state.wrapping_mul(0x100000001b3);
    }
    format!("{state:016x}")
}

/// Jeffreys lower confidence bound at [`CONFIDENCE_LEVEL`] for observing
/// `successes` in `n` trials: the lower tail quantile of
/// Beta(successes + 1/2, n − successes + 1/2). This is what makes small
/// samples honest without a minimum-N gate: 5/5 bounds a proportion at only
/// ~0.69, while 80/80 bounds it above 0.97 — certainty has to be earned
/// with data, not asserted past a cutoff.
pub fn jeffreys_lcb(successes: i64, n: i64) -> f64 {
    if n <= 0 {
        return 0.0;
    }
    let (a, b) = (successes as f64 + 0.5, (n - successes) as f64 + 0.5);
    beta_quantile(a, b, 1.0 - CONFIDENCE_LEVEL)
}

/// Quantile of the Beta(a, b) distribution by bisection on the regularized
/// incomplete beta function. The CDF is monotone, so 200 halvings pin the
/// quantile far below any precision this crate consumes.
fn beta_quantile(a: f64, b: f64, p: f64) -> f64 {
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if reg_inc_beta(a, b, mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Regularized incomplete beta I_x(a, b) via the standard continued-fraction
/// expansion (modified Lentz), using the symmetry I_x(a,b) = 1 − I_{1−x}(b,a)
/// to keep the fraction in its fast-converging region.
fn reg_inc_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_cf(a, b, x) / a
    } else {
        1.0 - front * beta_cf(b, a, 1.0 - x) / b
    }
}

fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-30;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..200 {
        let m = m as f64;
        let m2 = 2.0 * m;
        // even step
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        // odd step
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-12 {
            break;
        }
    }
    h
}

/// Lanczos approximation of ln Γ(x), g = 7, n = 9 — accurate to well beyond
/// the demands of a confidence bound.
fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.99999999999980993,
        676.5203681218851,
        -1259.1392167224028,
        771.32342877765313,
        -176.61502916214059,
        12.507343278686905,
        -0.13857109526572012,
        9.9843695780195716e-6,
        1.5056327351493116e-7,
    ];
    if x < 0.5 {
        // reflection
        return std::f64::consts::PI.ln()
            - (std::f64::consts::PI * x).sin().ln()
            - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut acc = G[0];
    for (i, g) in G.iter().enumerate().skip(1) {
        acc += g / (x + i as f64);
    }
    let t = x + 7.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + acc.ln()
}

/// Otsu's 2-class natural break over a small sample: the threshold that
/// maximizes between-class variance, returned as the midpoint of the gap it
/// splits. `None` when fewer than two distinct values exist — there is no
/// break in a point mass.
fn otsu_cut(values: &[f64]) -> Option<f64> {
    let mut xs = values.to_vec();
    xs.sort_by(f64::total_cmp);
    let n = xs.len();
    if n < 2 || xs[0] == xs[n - 1] {
        return None;
    }
    let total: f64 = xs.iter().sum();
    let mut best = (f64::MIN, 0usize);
    let mut lower_sum = 0.0;
    for k in 1..n {
        lower_sum += xs[k - 1];
        if xs[k - 1] == xs[k] {
            continue; // not a real boundary
        }
        let (w0, w1) = (k as f64, (n - k) as f64);
        let (mu0, mu1) = (lower_sum / w0, (total - lower_sum) / w1);
        let between = w0 * w1 * (mu0 - mu1) * (mu0 - mu1);
        if between > best.0 {
            best = (between, k);
        }
    }
    Some(0.5 * (xs[best.1 - 1] + xs[best.1]))
}

/// The corpus's document boundary, derived by [`derive_doc_boundary`] and
/// applied by [`is_document_column`]. `Cut` carries a threshold over
/// `ln(1 + median_len)`.
#[derive(Debug, Clone, PartialEq)]
pub enum DocBoundary {
    /// Every column's typical value is beyond value scale: a pure document
    /// corpus, however its lengths cluster.
    AllDocs,
    /// Columns whose median log-length exceeds the cut are document columns.
    Cut(f64),
}

/// Derives the document boundary from the per-column median value lengths
/// (raw characters). Parameter-free: the corpus's own length distribution
/// decides, anchored only by [`EXACT_MAX_LEN`] — the operational definition
/// of where "being a value" ends, since a value the exact channel cannot
/// match is not a value in any sense the pipeline can use.
///
/// - Medians uniformly beyond value scale → [`DocBoundary::AllDocs`]: the
///   corpus is prose everywhere (single-column article stores land here).
/// - Otherwise the boundary is Otsu's natural break over the median
///   log-lengths, never lowered below value scale. On value-shaped corpora
///   (BIRD and its kin) the break sits among short medians, the value-scale
///   floor prevails, and no column is a document — the degenerate unimodal
///   case falls out of the same expression.
///
/// The break being *corpus-relative* is the point: a 150-char-median column
/// clusters with values in a corpus whose documents run 3,000 chars, and the
/// derived cut says so, where any fixed length would have guessed.
pub fn derive_doc_boundary(median_lens: &[f64]) -> DocBoundary {
    let prose = ((EXACT_MAX_LEN + 1) as f64).ln();
    if !median_lens.is_empty() && median_lens.iter().all(|m| (1.0 + m).ln() > prose) {
        return DocBoundary::AllDocs;
    }
    let logs: Vec<f64> = median_lens.iter().map(|m| (1.0 + m).ln()).collect();
    DocBoundary::Cut(otsu_cut(&logs).map_or(prose, |c| c.max(prose)))
}

/// One `lex_columns` row: the measurements every purpose predicate reads.
#[derive(Debug, Clone)]
pub struct ColumnProfile {
    pub src_table: String,
    pub src_column: String,
    pub n_values: i64,
    pub n_distinct: i64,
    pub distinct_ratio: f64,
    pub distinct_lcb: f64,
    pub alpha_ratio: f64,
    pub alpha_lcb: f64,
    pub numeric_ratio: f64,
    pub numeric_lcb: f64,
    pub temporal_ratio: f64,
    pub temporal_lcb: f64,
    pub idlike_ratio: f64,
    pub idlike_lcb: f64,
    pub avg_len: f64,
    pub median_len: f64,
}

/// Reads the measurement profiles out of `lex_columns`.
pub fn column_profiles(db: &StemmaDb) -> Result<Vec<ColumnProfile>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT src_table, src_column, n_values, n_distinct, distinct_ratio,
                distinct_lcb, alpha_ratio, alpha_lcb, numeric_ratio, numeric_lcb,
                temporal_ratio, temporal_lcb, idlike_ratio, idlike_lcb,
                avg_len, median_len
         FROM lex_columns ORDER BY src_table, src_column",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ColumnProfile {
            src_table: r.get(0)?,
            src_column: r.get(1)?,
            n_values: r.get(2)?,
            n_distinct: r.get(3)?,
            distinct_ratio: r.get(4)?,
            distinct_lcb: r.get(5)?,
            alpha_ratio: r.get(6)?,
            alpha_lcb: r.get(7)?,
            numeric_ratio: r.get(8)?,
            numeric_lcb: r.get(9)?,
            temporal_ratio: r.get(10)?,
            temporal_lcb: r.get(11)?,
            idlike_ratio: r.get(12)?,
            idlike_lcb: r.get(13)?,
            avg_len: r.get(14)?,
            median_len: r.get(15)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Is this column a document column under the given boundary? Document
/// columns hold text that mentions resolve *into* (BM25/snippet semantics)
/// rather than *equal*; the property is a fact about the column's place in
/// the corpus's length distribution, so it is decided per column, not per
/// cell. The stamped `lex_values.is_doc` bit is this predicate applied under
/// the *adopted* boundary (see [`profile_columns`] for the hysteresis).
pub fn is_document_column(profile: &ColumnProfile, boundary: &DocBoundary) -> bool {
    match boundary {
        DocBoundary::AllDocs => true,
        DocBoundary::Cut(cut) => (1.0 + profile.median_len).ln() > *cut,
    }
}

/// Can a natural-language paraphrase plausibly reach this column's values?
/// Letter-bearing by simple majority, and not *confidently* shape-structural:
/// a column is disqualified only when the Jeffreys lower bound of its
/// numeric, temporal or id-like fraction clears 1/2 — evidence that most of
/// the column is structural, not a hunch from three rows. Admission is the
/// default and denial carries the burden of proof, which is exactly how the
/// old minimum-sample gate behaved without needing one: a small column
/// cannot be confidently condemned, so it stays eligible.
///
/// Deliberately absent: any cardinality test. Near-unique prose (names,
/// titles) is legitimate vocabulary, and key-like columns are already caught
/// by their shape. A letter-bearing code scheme (`SKU-0001-Q3`) is admitted
/// — the recurrence requirement downstream keeps it harmless, and denying it
/// would take a distinctness threshold this crate no longer owns.
pub fn is_paraphrasable_column(profile: &ColumnProfile) -> bool {
    profile.alpha_ratio > 0.5
        && profile.numeric_lcb <= 0.5
        && profile.temporal_lcb <= 0.5
        && profile.idlike_lcb <= 0.5
}

/// Which columns may hold knowledge-graph term affinity. Today this IS
/// [`is_paraphrasable_column`] — a term can recur meaningfully exactly in
/// the columns a paraphrase can reach — kept as its own name so the two
/// consumers state their purpose and can diverge without a hunt.
pub fn is_vocabulary_column(profile: &ColumnProfile) -> bool {
    is_paraphrasable_column(profile)
}

/// The `(table, column)` pairs eligible for vocabulary purposes — the
/// vocabulary predicate minus document columns (their cells are prose to
/// resolve into, not values to disambiguate). Consumed by interpretation
/// candidacy here and the knowledge compiler's affinity pass.
pub fn vocabulary_columns(db: &StemmaDb) -> Result<Vec<(String, String)>> {
    let boundary = read_adopted_boundary(db.conn())?;
    Ok(column_profiles(db)?
        .into_iter()
        .filter(|p| {
            is_vocabulary_column(p)
                && !boundary
                    .as_ref()
                    .is_some_and(|b| is_document_column(p, b))
        })
        .map(|p| (p.src_table, p.src_column))
        .collect())
}

fn boundary_json(b: &DocBoundary) -> String {
    match b {
        DocBoundary::AllDocs => "{\"kind\":\"all_docs\"}".into(),
        DocBoundary::Cut(c) => format!("{{\"kind\":\"cut\",\"cut\":{c}}}"),
    }
}

/// The adopted document boundary from the `doc_cut` receipt, if one exists.
fn read_adopted_boundary(
    conn: &stemmadb::rusqlite::Connection,
) -> Result<Option<DocBoundary>> {
    let row: Option<(String, Option<f64>)> = conn
        .query_row(
            "SELECT json_extract(value_json, '$.adopted.kind'),
                    json_extract(value_json, '$.adopted.cut')
             FROM derivations WHERE artifact = 'doc_cut' AND derivation_version = ?1",
            [LEX_DERIVATION_VERSION],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    Ok(match row {
        Some((kind, _)) if kind == "all_docs" => Some(DocBoundary::AllDocs),
        Some((kind, Some(cut))) if kind == "cut" => Some(DocBoundary::Cut(cut)),
        _ => None,
    })
}

/// Rebuilds `lex_columns` from `lex_values` — one grouped pass for counts and
/// shape sums, one windowed pass for medians — then re-derives the document
/// boundary and stamps `lex_values.is_doc` per column.
///
/// **Hysteresis.** The boundary *re-derives* on every call (recorded as
/// `current` in the `doc_cut` receipt) but is *re-adopted* only when adopting
/// it would change at least one column's document-ness; otherwise the
/// previously adopted value stands. A cut that drifts with every appended
/// row must not churn derived state that only cares which side of it each
/// column is on. When adoption does flip columns, their embed-queue items
/// and vectors are invalid in a way no content hash can see (the *channel*
/// changed, not the text), so they are dropped for re-enqueue and the blast
/// radius is logged.
pub fn profile_columns(db: &StemmaDb) -> Result<usize> {
    let conn = db.conn();
    // GLOB shapes over value_norm (already lower(trim())). In a GLOB set,
    // `^` first negates and `-` last is literal. These are structural
    // definitions (what it means to parse as a number, look like a uuid,
    // land in an epoch range), not tunables.
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
    // Counts, not ratios: the Jeffreys bound needs (successes, n).
    let sql = format!(
        "SELECT src_table, src_column, count(*), count(DISTINCT value_norm),
                sum(value_norm GLOB '*[a-z]*'),
                sum({numeric}),
                sum(({numeric} AND ({epoch}))
                    OR value_norm GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'),
                sum(value_norm GLOB '{uuid}'
                    OR (length(value_norm) >= 16 AND value_norm NOT GLOB '*[^0-9a-f]*')
                    OR (length(value_norm) >= 6 AND value_norm NOT GLOB '*[^0-9]*')),
                avg(length(value))
         FROM lex_values
         GROUP BY src_table, src_column"
    );
    type Row = (String, String, i64, i64, i64, i64, i64, i64, f64);
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
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;

    // True per-column medians of value length (averaging the middle pair on
    // even counts), via one windowed pass.
    let medians: std::collections::HashMap<(String, String), f64> = conn
        .prepare(
            "WITH ranked AS (
                 SELECT src_table AS t, src_column AS c, length(value) AS len,
                        row_number() OVER (PARTITION BY src_table, src_column
                                           ORDER BY length(value)) AS rn,
                        count(*) OVER (PARTITION BY src_table, src_column) AS n
                 FROM lex_values)
             SELECT t, c, avg(len) FROM ranked
             WHERE rn IN ((n + 1) / 2, (n + 2) / 2)
             GROUP BY t, c",
        )?
        .query_map([], |r| {
            Ok(((r.get::<_, String>(0)?, r.get::<_, String>(1)?), r.get(2)?))
        })?
        .collect::<std::result::Result<_, _>>()?;

    let tx = conn.unchecked_transaction()?;
    conn.execute("DELETE FROM lex_columns", [])?;
    {
        let mut insert = conn.prepare_cached(
            "INSERT INTO lex_columns (src_table, src_column, n_values, n_distinct,
                 distinct_ratio, distinct_lcb, alpha_ratio, alpha_lcb,
                 numeric_ratio, numeric_lcb, temporal_ratio, temporal_lcb,
                 idlike_ratio, idlike_lcb, avg_len, median_len)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )?;
        for (table, column, n, n_distinct, alpha, numeric, temporal, idlike, avg_len) in &profiles
        {
            let ratio = |k: i64| k as f64 / (*n).max(1) as f64;
            let median = medians
                .get(&(table.clone(), column.clone()))
                .copied()
                .unwrap_or(0.0);
            insert.execute(stemmadb::rusqlite::params![
                table,
                column,
                n,
                n_distinct,
                ratio(*n_distinct),
                jeffreys_lcb(*n_distinct, *n),
                ratio(*alpha),
                jeffreys_lcb(*alpha, *n),
                ratio(*numeric),
                jeffreys_lcb(*numeric, *n),
                ratio(*temporal),
                jeffreys_lcb(*temporal, *n),
                ratio(*idlike),
                jeffreys_lcb(*idlike, *n),
                avg_len,
                median
            ])?;
        }
    }

    // --- Document boundary: derive, maybe adopt, stamp. ---
    let all = column_profiles(db)?;
    let med: Vec<f64> = all.iter().map(|p| p.median_len).collect();
    let current = derive_doc_boundary(&med);
    let previous = read_adopted_boundary(conn)?;
    let adopted = match &previous {
        None => current.clone(),
        Some(prev) => {
            let doc_set = |b: &DocBoundary| -> Vec<bool> {
                all.iter().map(|p| is_document_column(p, b)).collect()
            };
            if doc_set(prev) == doc_set(&current) {
                prev.clone()
            } else {
                current.clone()
            }
        }
    };

    let mut reset_items = 0usize;
    let mut flipped = 0usize;
    let vec_tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE name IN ('vec_dense', 'vec_interp')")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for p in &all {
        let target = is_document_column(p, &adopted) as i64;
        let changed = conn.execute(
            "UPDATE lex_values SET is_doc = ?3
             WHERE src_table = ?1 AND src_column = ?2 AND is_doc != ?3",
            stemmadb::rusqlite::params![p.src_table, p.src_column, target],
        )?;
        if changed > 0 {
            flipped += 1;
            // The column changed channels (document ↔ value): queue items and
            // vectors keyed under the old reading are invalid wholesale.
            // Drop them; the next enqueue pass recreates the right ones.
            reset_items += conn.execute(
                "DELETE FROM embed_queue WHERE src_table = ?1 AND src_column = ?2",
                stemmadb::rusqlite::params![p.src_table, p.src_column],
            )?;
            for vt in &vec_tables {
                conn.execute(
                    &format!("DELETE FROM {vt} WHERE src_table = ?1 AND src_column = ?2"),
                    stemmadb::rusqlite::params![p.src_table, p.src_column],
                )?;
            }
        }
    }
    if reset_items > 0 {
        tracing::info!(
            columns = flipped,
            items_reset = reset_items,
            "document boundary adopted: embed items reset for re-enqueue"
        );
    }

    let corpus_fp = corpus_fingerprint(conn)?;
    write_receipt(
        conn,
        "doc_cut",
        &corpus_fp,
        &format!(
            "{{\"current\":{},\"adopted\":{}}}",
            boundary_json(&current),
            boundary_json(&adopted)
        ),
    )?;
    write_receipt(conn, "profiles", &corpus_fp, "{}")?;
    tx.commit()?;
    Ok(profiles.len())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DenseStats {
    pub vectors: usize,
    pub dimension: usize,
    pub model: String,
    /// Query-side convention recorded for the space (`''` = the registry
    /// row predates the column and recorded nothing; `"{query}"` = stated
    /// bare).
    pub query_template: String,
    pub promoted: bool,
}

/// Promotes externally staged vectors into the vec0 dense index.
///
/// Loaders (e.g. eval/legal/load_vectors.py) write rows into `vec_staging`
/// — a plain table, writable without the sqlite-vec extension — and this
/// pass, running inside the extension-bearing process, creates the `vec0`
/// virtual table, moves the vectors in, records the model identity in
/// `model_registry` — model, dimension AND the query template the staged
/// vectors were produced to be queried under, so the space's convention is
/// stored fact rather than a name-based guess at query time — and drops the
/// staging table. One model per dense table, always: mixed identities in
/// staging are a hard error.
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
        let existing: Option<(String, i64, String)> = conn
            .query_row(
                "SELECT model, dimension, query_template FROM model_registry
                 WHERE vector_table = 'vec_dense'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        if existing.is_some() {
            let _ = derive_dense_geometry(db);
        }
        return Ok(existing.map(|(model, dim, template)| {
            let vectors: i64 = conn
                .query_row("SELECT count(*) FROM vec_dense", [], |r| r.get(0))
                .unwrap_or(0);
            DenseStats {
                vectors: vectors as usize,
                dimension: dim as usize,
                model,
                query_template: template,
                promoted: false,
            }
        }));
    }

    // The staged query template is part of the model identity (endpoint,
    // model and template must agree), so it rides the same DISTINCT that
    // enforces one-model-per-table. Staging written by loaders that predate
    // the column carries no template: '' lands in the registry, and query
    // time falls back to the model-family guess — the state this column
    // exists to retire, so new loaders always stage one.
    let staging_has_template: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('vec_staging') WHERE name = 'query_template'",
        [],
        |r| r.get(0),
    )?;
    let identities: Vec<(String, i64, String)> = if staging_has_template > 0 {
        conn.prepare("SELECT DISTINCT model, dim, query_template FROM vec_staging")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?
    } else {
        conn.prepare("SELECT DISTINCT model, dim FROM vec_staging")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, String::new())))?
            .collect::<std::result::Result<_, _>>()?
    };
    let (model, dim, template) = match identities.as_slice() {
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
        "INSERT INTO model_registry (vector_table, backend, model, dimension, quantization,
                                     query_template)
         VALUES ('vec_dense', 'staged', ?1, ?2, 'f32', ?3)
         ON CONFLICT(vector_table) DO UPDATE
             SET model = ?1, dimension = ?2, query_template = ?3",
        stemmadb::rusqlite::params![model, dim, template],
    )?;
    conn.execute_batch("DROP TABLE vec_staging;")?;
    let _ = derive_dense_geometry(db);

    Ok(Some(DenseStats {
        vectors: moved,
        dimension: dim as usize,
        model,
        query_template: template,
        promoted: true,
    }))
}

/// Vectors sampled for the corpus's dense geometry; enough that the two
/// means are stable, few enough that the derivation is sub-second.
const GEOMETRY_SAMPLE: usize = 128;

/// The corpus's dense-space geometry, derived from `vec_dense` itself and
/// receipted like every other derived quantity: `null_mean` — the mean
/// cosine of random pairs of this corpus's vectors under this model (its
/// anisotropy floor: what "unrelated" scores HERE, not the 0.0 of an ideal
/// space), and `nn_mean` — the mean cosine of a vector to its nearest
/// distinct neighbor (what a genuine near-match scores). Resolution
/// calibrates raw cosines between the two instead of against constants:
/// on the legal corpus random pairs score +0.21, and treating cosine as
/// absolute evidence let dense-only hits displace correct lexical
/// candidates (eval: L1 recall@5 0.68 → 0.48 with dense enabled).
///
/// Returns `None` — and removes any stale receipt — when the index is
/// absent, too small to measure, or degenerate (nn indistinguishable from
/// null); the resolver then lets dense participate by rank alone.
pub fn derive_dense_geometry(db: &StemmaDb) -> Result<Option<(f64, f64)>> {
    let conn = db.conn();
    let has: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'vec_dense'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let absent = |conn: &stemmadb::rusqlite::Connection| -> Result<Option<(f64, f64)>> {
        conn.execute(
            "DELETE FROM derivations WHERE artifact = 'dense:geometry'",
            [],
        )?;
        Ok(None)
    };
    if has == 0 {
        return absent(&conn);
    }
    let total: i64 = conn.query_row("SELECT count(*) FROM vec_dense", [], |r| r.get(0))?;
    if (total as usize) < GEOMETRY_SAMPLE {
        return absent(&conn);
    }
    let model: String = conn
        .query_row(
            "SELECT model FROM model_registry WHERE vector_table = 'vec_dense'",
            [],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let fingerprint = format!("{model}:{total}");
    if read_receipt(&conn, "dense:geometry")?.as_deref() == Some(fingerprint.as_str()) {
        let cached: Option<String> = conn
            .query_row(
                "SELECT value_json FROM derivations WHERE artifact = 'dense:geometry'",
                [],
                |r| r.get(0),
            )
            .ok();
        if let Some(json) = cached {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                if let (Some(nm), Some(nn)) = (
                    v.get("null_mean").and_then(|x| x.as_f64()),
                    v.get("nn_mean").and_then(|x| x.as_f64()),
                ) {
                    return Ok(Some((nm, nn)));
                }
            }
        }
    }

    // Evenly-strided rowid probes: vec0 assigns rowids sequentially, so a
    // stride over [min, max] samples the corpus without scanning it. Gaps
    // (deleted rows) just shrink the sample.
    let (lo, hi): (i64, i64) = conn.query_row(
        "SELECT min(rowid), max(rowid) FROM vec_dense",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let stride = ((hi - lo) / GEOMETRY_SAMPLE as i64).max(1);
    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(GEOMETRY_SAMPLE);
    let mut probe = conn.prepare("SELECT embedding FROM vec_dense WHERE rowid = ?1")?;
    let mut r = lo;
    while r <= hi && vectors.len() < GEOMETRY_SAMPLE {
        if let Ok(blob) = probe.query_row([r], |row| row.get::<_, Vec<u8>>(0)) {
            vectors.push(
                blob.chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect(),
            );
        }
        r += stride;
    }
    if vectors.len() < GEOMETRY_SAMPLE / 2 {
        return absent(&conn);
    }

    let cos = |a: &[f32], b: &[f32]| -> f64 {
        let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
        for (x, y) in a.iter().zip(b) {
            dot += (*x as f64) * (*y as f64);
            na += (*x as f64) * (*x as f64);
            nb += (*y as f64) * (*y as f64);
        }
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na.sqrt() * nb.sqrt())
        }
    };

    // Null: every distinct pair of the strided sample. The sample spans the
    // corpus, so the pairs are what "two arbitrary rows" score.
    let (mut sum, mut n) = (0.0f64, 0usize);
    for i in 0..vectors.len() {
        for j in (i + 1)..vectors.len() {
            sum += cos(&vectors[i], &vectors[j]);
            n += 1;
        }
    }
    let null_mean = sum / n.max(1) as f64;

    // Nearest-neighbor scale: KNN k=2 (self + nearest distinct) for a
    // quarter of the sample — 32 brute-force scans, bounded and fast.
    let mut knn = conn.prepare(
        "SELECT distance FROM vec_dense WHERE embedding MATCH ?1 AND k = 2",
    )?;
    let (mut nn_sum, mut nn_n) = (0.0f64, 0usize);
    for v in vectors.iter().take(GEOMETRY_SAMPLE / 4) {
        let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        let dists: Vec<f64> = knn
            .query_map([blob], |row| row.get::<_, f64>(0))?
            .filter_map(|d| d.ok())
            .collect();
        // First hit is (numerically) self; the second is the neighbor.
        if let Some(d) = dists.get(1) {
            nn_sum += 1.0 - (d * d) / 2.0;
            nn_n += 1;
        }
    }
    if nn_n == 0 {
        return absent(&conn);
    }
    let nn_mean = nn_sum / nn_n as f64;
    if nn_mean <= null_mean + 1e-6 {
        // Degenerate geometry: neighbors indistinguishable from noise.
        return absent(&conn);
    }

    write_receipt(
        &conn,
        "dense:geometry",
        &fingerprint,
        &format!(
            "{{\"null_mean\":{null_mean:.6},\"nn_mean\":{nn_mean:.6},\"sampled\":{}}}",
            vectors.len()
        ),
    )?;
    Ok(Some((null_mean, nn_mean)))
}

/// Items per drain cycle: claimed together, embedded as [`EMBED_CONCURRENCY`]
/// concurrent requests, written back in one transaction. Sized so each
/// request carries EMBED_BATCH / EMBED_CONCURRENCY documents — wide enough
/// to fill a pooling endpoint's forward passes, small enough that one
/// request stays comfortably inside the client's 60s read timeout.
pub const EMBED_BATCH: usize = 768;

/// Concurrent embedding requests in flight per drain cycle. One request at
/// a time leaves the endpoint idle while the client claims, fetches and
/// writes, and leaves all but one of the endpoint's API processes idle
/// during tokenization — measured against a 3-replica data-parallel
/// endpoint, per-item wall time was identical at batch 32 and batch 256
/// because that serial work, not GPU compute, set the pace. A few requests
/// in flight overlap those stages; more multiplies peak payload without
/// adding overlap.
pub const EMBED_CONCURRENCY: usize = 3;

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

/// Finds document cells (`lex_values.is_doc = 1`) that need embedding work
/// and enqueues them as pending items. Documents only — short values travel
/// through the interpretation-card path instead
/// ([`enqueue_missing_interpretations`]), which serializes column context the
/// raw value does not carry.
///
/// Each item carries a [`content_hash`] of the exact text it was enqueued to
/// embed. An item is (re)queued when: it has never been enqueued; its stored
/// hash differs from the current text (the source row changed — its stale
/// vector is deleted and the item resets to pending, which is the whole of
/// incremental re-embedding); or it is `done` but its vector has vanished.
/// Items from before hashes existed (empty hash) adopt the current text as
/// their baseline without resetting — their provenance is unknowable, and
/// any change from here on is caught. Idempotent; returns the number of
/// items newly enqueued or reset.
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

    type Item = (String, String, i64, String, Option<i64>, String, String, bool);
    let items: Vec<Item> = {
        let mut stmt = conn.prepare(
            "SELECT lv.src_table, lv.src_column, lv.src_rowid, lv.value,
                    q.id, coalesce(q.status, ''), coalesce(q.content_hash, ''),
                    EXISTS (SELECT 1 FROM covered c
                            WHERE c.src_table = lv.src_table
                              AND c.src_column = lv.src_column
                              AND c.src_rowid = lv.src_rowid)
             FROM lex_values lv
             LEFT JOIN embed_queue q
               ON q.src_table = lv.src_table AND q.src_column = lv.src_column
              AND q.src_rowid = lv.src_rowid
             WHERE lv.is_doc = 1",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    let mut queued = 0usize;
    let tx = conn.unchecked_transaction()?;
    {
        let mut insert = conn.prepare_cached(
            "INSERT INTO embed_queue (src_table, src_column, src_rowid, content_hash)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut reset = conn.prepare_cached(
            "UPDATE embed_queue SET status = 'pending', attempts = 0, error = '',
                    serialized = '', content_hash = ?2, updated_at = datetime('now')
             WHERE id = ?1",
        )?;
        let mut adopt = conn.prepare_cached(
            "UPDATE embed_queue SET content_hash = ?2 WHERE id = ?1",
        )?;
        for (table, column, rowid, value, qid, status, stored_hash, covered) in items {
            let hash = content_hash(&value);
            match qid {
                None => {
                    insert.execute(stemmadb::rusqlite::params![table, column, rowid, hash])?;
                    queued += 1;
                }
                Some(id) if stored_hash.is_empty() => {
                    // Pre-v5 item: unknown provenance, adopt the baseline.
                    adopt.execute(stemmadb::rusqlite::params![id, hash])?;
                }
                Some(id) if stored_hash != hash => {
                    // The source row changed: its stored vector (either
                    // table — the column may have changed channels too) is
                    // stale, and the item re-embeds.
                    delete_vectors(conn, &table, &column, rowid)?;
                    reset.execute(stemmadb::rusqlite::params![id, hash])?;
                    queued += 1;
                }
                Some(id) if status == "done" && !covered => {
                    reset.execute(stemmadb::rusqlite::params![id, hash])?;
                    queued += 1;
                }
                Some(_) => {} // pending or failed with current content, or done and covered
            }
        }
    }
    tx.commit()?;
    conn.execute_batch("DROP TABLE covered;")?;
    Ok(queued)
}

/// Deletes any vector stored under a provenance triple, in whichever vector
/// table holds it.
fn delete_vectors(
    conn: &stemmadb::rusqlite::Connection,
    table: &str,
    column: &str,
    rowid: i64,
) -> Result<()> {
    let vec_tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE name IN ('vec_dense', 'vec_interp')")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for vt in vec_tables {
        conn.execute(
            &format!(
                "DELETE FROM {vt} WHERE src_table = ?1 AND src_column = ?2 AND src_rowid = ?3"
            ),
            stemmadb::rusqlite::params![table, column, rowid],
        )?;
    }
    Ok(())
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
/// value_norm)` with `is_doc = 0` — that need embedding work, and enqueues
/// each as pending with its serialization card stored in
/// `embed_queue.serialized`. The queue key is the interpretation's
/// representative cell: `src_rowid = MIN(src_rowid)` over the rows sharing
/// the value, so the provenance-triple unique key stays exact and enqueue
/// stays idempotent, with the same content-hash refresh semantics as the
/// document path — here the hash covers the *card*, so an item resets when
/// anything the encoder would see changed (the value, or its context
/// fragments). The representative is a CITATION KEY only — it never
/// influences card text, which is a function of the whole interpretation
/// (see [`interpretation_card`]).
///
/// This is the relational counterpart of the document queue: on value-shaped
/// corpora no column crosses the derived document boundary, so a
/// documents-only dense channel is inert, and a value appearing in two
/// columns needs column context to be separable at all. The card carries
/// that context.
///
/// Candidacy is restricted to columns [`is_vocabulary_column`] admits (via
/// [`vocabulary_columns`]): temporal, numeric and identifier-shaped columns
/// recur across tables by construction (denormalization copies them), not
/// because their values are ambiguous vocabulary, and no paraphrase query
/// can ever retrieve them.
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
    // The vocabulary predicate, materialized for the SQL below to join.
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS vocab_cols (
             src_table TEXT NOT NULL, src_column TEXT NOT NULL,
             PRIMARY KEY (src_table, src_column)
         );
         DELETE FROM vocab_cols;",
    )?;
    {
        let mut ins = conn.prepare_cached(
            "INSERT OR IGNORE INTO vocab_cols (src_table, src_column) VALUES (?1, ?2)",
        )?;
        for (t, c) in vocabulary_columns(db)? {
            ins.execute(stemmadb::rusqlite::params![t, c])?;
        }
    }
    // Join-explained recurrence is a key relationship, not vocabulary.
    // A column that is an endpoint of a discovered join (declared or
    // inferred fk in the compiled graph) recurs across columns BY
    // CONSTRUCTION — that is what a join is — so its values are keys even
    // when they are letter-bearing ("18 CCR § 240" in refs.ref and
    // citations.ref). Issue #2's lesson, applied to text keys: without this,
    // a citation-mined corpus enqueues every reference string as a card
    // (~133k on the legal corpus) and the fragment pass grinds for hours.
    // Reading kg_edges here shares the documented layering deviation
    // (04-knowledge-graph.md): the graph is in the same store.
    let has_kg: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'kg_edges'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_kg > 0 {
        let pairs: Vec<(String, String)> = conn
            .prepare(
                "SELECT replace(n1.key, 'table:', ''), e.label FROM kg_edges e
                 JOIN kg_nodes n1 ON n1.id = e.src
                 WHERE e.kind IN ('fk', 'inferred_fk')
                 UNION ALL
                 SELECT replace(n2.key, 'table:', ''), e.label FROM kg_edges e
                 JOIN kg_nodes n2 ON n2.id = e.dst
                 WHERE e.kind IN ('fk', 'inferred_fk')",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let mut del = conn.prepare_cached(
            "DELETE FROM vocab_cols WHERE src_table = ?1 AND src_column = ?2",
        )?;
        for (table, label) in pairs {
            // Labels are "src_col → dst_col" (inferred joins mark "→?");
            // both endpoint columns are key columns for their tables.
            for col in label.split('→') {
                let col = col.trim().trim_start_matches('?').trim();
                if !col.is_empty() {
                    del.execute(stemmadb::rusqlite::params![table, col])?;
                }
            }
        }
    }

    // Ambiguity is INCIDENTAL sharing; keys and denormalized copies are
    // SYSTEMATIC sharing. A value qualifies as ambiguous vocabulary only if
    // it is shared across a column pair whose overlap is a minority of both
    // columns' vocabularies (majority overlap = same domain — a text join
    // key like citations.ref↔refs.ref, or a copied column — regardless of
    // what the join miner asserted; its 0.95 threshold is for CLAIMING a
    // join, and the burden here is reversed). Majority is a definition, not
    // a tunable, matching the card-fragment rule.
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS vocab_pairs (
             t1 TEXT NOT NULL, c1 TEXT NOT NULL,
             t2 TEXT NOT NULL, c2 TEXT NOT NULL, excluded INTEGER NOT NULL
         );
         DELETE FROM vocab_pairs;
         CREATE TEMP TABLE IF NOT EXISTS vocab_values AS
             SELECT DISTINCT lv.src_table AS t, lv.src_column AS c, lv.value_norm AS vn
             FROM lex_values lv
             JOIN vocab_cols vc ON vc.src_table = lv.src_table
                               AND vc.src_column = lv.src_column
             WHERE lv.is_doc = 0 AND lv.value_norm GLOB '*[a-z]*';
         DELETE FROM vocab_values;
         INSERT INTO vocab_values
             SELECT DISTINCT lv.src_table, lv.src_column, lv.value_norm
             FROM lex_values lv
             JOIN vocab_cols vc ON vc.src_table = lv.src_table
                               AND vc.src_column = lv.src_column
             WHERE lv.is_doc = 0 AND lv.value_norm GLOB '*[a-z]*';
         CREATE INDEX IF NOT EXISTS temp.vocab_values_vn ON vocab_values(vn);
         INSERT INTO vocab_pairs
             SELECT a.t, a.c, b.t, b.c,
                    count(*) * 2 > min(
                        (SELECT count(*) FROM vocab_values x WHERE x.t = a.t AND x.c = a.c),
                        (SELECT count(*) FROM vocab_values y WHERE y.t = b.t AND y.c = b.c))
             FROM vocab_values a
             JOIN vocab_values b ON b.vn = a.vn
              AND (a.t || '·' || a.c) < (b.t || '·' || b.c)
             GROUP BY a.t, a.c, b.t, b.c;
         CREATE TEMP TABLE IF NOT EXISTS ambiguous_vocab (value_norm TEXT PRIMARY KEY);
         DELETE FROM ambiguous_vocab;
         INSERT OR IGNORE INTO ambiguous_vocab
             SELECT a.vn FROM vocab_values a
             JOIN vocab_values b ON b.vn = a.vn
              AND (a.t || '·' || a.c) < (b.t || '·' || b.c)
             JOIN vocab_pairs p ON p.t1 = a.t AND p.c1 = a.c
                               AND p.t2 = b.t AND p.c2 = b.c
             WHERE p.excluded = 0;",
    )?;

    // One row per distinct interpretation. The displayed value is the MODAL
    // raw spelling over the rows sharing the value_norm (ties broken
    // lexicographically) — a property of the set, deterministic under any
    // insertion order — never the representative row's cell, which is kept
    // only as the citation key.
    //
    // Candidacy is column-purposed. A card is consulted solely when the same
    // value ties across DISTINCT (table, column) readings — but on
    // denormalized schemas cross-column recurrence is dominated by *copied
    // data* (timestamps, join keys, SKUs repeated into fact tables), which is
    // structurally guaranteed to recur and can never be reached by a
    // natural-language paraphrase. So both the outer selection and the
    // recurrence subquery are restricted to vocabulary columns, with a
    // per-value letter guard for non-linguistic strays inside otherwise-texty
    // columns. Expectation: the queue holds the shared *vocabulary* of the
    // corpus — typically orders of magnitude below the untyped predicate on
    // warehouse shapes (issue #2: 846,989 cards, 90.4% epoch floats, against
    // 98 useful documents). The card's head value is the MODAL spelling
    // across the interpretation's rows (issue #6): deterministic, never the
    // representative row's accident.
    type Cand = (String, String, i64, String, String, Option<i64>, String, String, bool);
    let candidates: Vec<Cand> = {
        let mut stmt = conn.prepare(
            "SELECT t.src_table, t.src_column, t.rep, t.value_norm,
                    (SELECT v.value FROM lex_values v
                     WHERE v.src_table = t.src_table AND v.src_column = t.src_column
                       AND v.value_norm = t.value_norm AND v.is_doc = 0
                     GROUP BY v.value ORDER BY count(*) DESC, v.value LIMIT 1),
                    q.id, coalesce(q.status, ''), coalesce(q.content_hash, ''),
                    EXISTS (SELECT 1 FROM covered_interp c
                            WHERE c.src_table = t.src_table
                              AND c.src_column = t.src_column
                              AND c.src_rowid = t.rep)
             FROM (
                 SELECT lv.src_table, lv.src_column, MIN(lv.src_rowid) AS rep, lv.value_norm
                 FROM lex_values lv
                 JOIN vocab_cols vc ON vc.src_table = lv.src_table
                                   AND vc.src_column = lv.src_column
                 WHERE lv.is_doc = 0
                   AND lv.value_norm GLOB '*[a-z]*'
                   AND lv.value_norm IN (SELECT value_norm FROM ambiguous_vocab)
                 GROUP BY lv.src_table, lv.src_column, lv.value_norm
             ) t
             LEFT JOIN embed_queue q
               ON q.src_table = t.src_table AND q.src_column = t.src_column
              AND q.src_rowid = t.rep
             ORDER BY t.src_table, t.src_column, t.rep",
        )?;
        let rows = stmt.query_map([], |r| {
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
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    let mut queued = 0usize;
    let tx = conn.unchecked_transaction()?;
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
                 JOIN vocab_cols oc
                   ON oc.src_table = o.src_table AND oc.src_column = o.src_column
                 WHERE me.src_table = ?1 AND me.src_column = ?2
                   AND me.value_norm = ?3 AND me.is_doc = 0
                   AND o.src_column != ?2 AND o.is_doc = 0
                 GROUP BY o.src_column, o.value)
             WHERE 2 * n > total
             ORDER BY src_column",
        )?;
        let mut insert = conn.prepare_cached(
            "INSERT INTO embed_queue (src_table, src_column, src_rowid, serialized, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut reset = conn.prepare_cached(
            "UPDATE embed_queue SET status = 'pending', attempts = 0, error = '',
                    serialized = ?2, content_hash = ?3, updated_at = datetime('now')
             WHERE id = ?1",
        )?;
        let mut adopt = conn.prepare_cached(
            "UPDATE embed_queue SET serialized = ?2, content_hash = ?3 WHERE id = ?1",
        )?;
        let mut processed = 0usize;
        for (table, column, rep, value_norm, value, qid, status, stored_hash, covered) in candidates {
            // Long sweeps must not monopolize the WAL writer: commit every
            // 500 items so concurrent query_log writes proceed and progress
            // survives interruption (the sweep is idempotent).
            processed += 1;
            if processed % 500 == 0 {
                conn.execute_batch("COMMIT; BEGIN")?;
            }
            let fragments: Vec<(String, String)> = frag_stmt
                .query_map(
                    stemmadb::rusqlite::params![table, column, value_norm],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
                .collect::<std::result::Result<_, _>>()?;
            let card = interpretation_card(&table, &column, &value, &fragments);
            let hash = content_hash(&card);
            match qid {
                None => {
                    insert.execute(stemmadb::rusqlite::params![table, column, rep, card, hash])?;
                    queued += 1;
                }
                Some(id) if stored_hash.is_empty() => {
                    // Pre-v5 item: unknown provenance, adopt the baseline.
                    adopt.execute(stemmadb::rusqlite::params![id, card, hash])?;
                }
                Some(id) if stored_hash != hash => {
                    delete_vectors(conn, &table, &column, rep)?;
                    reset.execute(stemmadb::rusqlite::params![id, card, hash])?;
                    queued += 1;
                }
                Some(id) if status == "done" && !covered => {
                    reset.execute(stemmadb::rusqlite::params![id, card, hash])?;
                    queued += 1;
                }
                Some(_) => {}
            }
        }
    }
    tx.commit()?;
    conn.execute_batch("DROP TABLE covered_interp; DROP TABLE vocab_cols;")?;
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
    // loaders and a live embedder of the same model share a space). The
    // query template is the identity's third leg: a registered convention
    // the embedder does not share means its queries would land in a foreign
    // convention's space — refused like a model mismatch. An empty
    // registered template is a row that predates the column, not a stated
    // convention, so it constrains nothing.
    let identity = embedder.identity();
    let offered = identity.model;
    let offered_template = identity.query_template;
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
        let registered: Option<(String, String)> = conn
            .query_row(
                "SELECT model, query_template FROM model_registry WHERE vector_table = ?1",
                [table],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        refusal = match &registered {
            Some((m, _)) if *m != offered => Some(Error::ModelMismatch {
                table: table.to_string(),
                registered: m.clone(),
                offered: offered.clone(),
            }),
            Some((_, t))
                if !t.is_empty()
                    && !stemma_embed::query_templates_agree(t, &offered_template) =>
            {
                Some(Error::QueryTemplateMismatch {
                    table: table.to_string(),
                    registered: t.clone(),
                    offered: offered_template.clone(),
                })
            }
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
    // INDEXED BY is load-bearing: without stats the planner answers this
    // point lookup with the (src_table, src_column) index, whose prefix
    // matches every row of a document column — measured at 33ms per lookup
    // against ~0 through (src_table, src_rowid), which turned the fetch
    // phase into 3× the embedding wall time per drain cycle.
    let mut doc_text = conn.prepare_cached(
        "SELECT value FROM lex_values INDEXED BY lex_values_src
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
        // The cycle's texts go out as EMBED_CONCURRENCY concurrent chunk
        // requests: the endpoint's serial per-request work (tokenizing,
        // scheduling) then overlaps its own compute and this thread's
        // write-back, instead of the two sides taking turns. Threads only
        // embed — the connection never leaves this thread.
        let chunk = work.len().div_ceil(EMBED_CONCURRENCY);
        let outcomes: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = texts
                .chunks(chunk)
                .map(|c| s.spawn(move || embedder.embed(c)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("embed worker panicked"))
                .collect()
        });

        // A failed chunk is charged one attempt per item and left pending
        // for the retry budget to bound; successful sibling chunks are
        // still written — their GPU work is done, and discarding it buys
        // nothing. Only a cycle with no successes at all propagates the
        // error (the endpoint is down, not one request unlucky).
        let mut embed_err = None;
        let mut writes: Vec<(&(i64, String, String, i64, &'static str), Vec<f32>)> = Vec::new();
        for (wchunk, outcome) in work.chunks(chunk).zip(outcomes) {
            match outcome {
                Ok(vectors) => writes.extend(wchunk.iter().zip(vectors)),
                Err(e) => {
                    let note = e.to_string();
                    for (id, ..) in wchunk {
                        conn.execute(
                            "UPDATE embed_queue SET attempts = attempts + 1, error = ?2,
                                    updated_at = datetime('now')
                             WHERE id = ?1",
                            stemmadb::rusqlite::params![id, note],
                        )?;
                    }
                    embed_err.get_or_insert(e);
                }
            }
        }
        if writes.is_empty() {
            if let Some(e) = embed_err {
                return Err(e.into());
            }
        }

        // One transaction for the whole write-back: per-item autocommits
        // were a measurable share of the serial overhead the concurrency
        // above exists to hide.
        let tx = conn.unchecked_transaction()?;
        let dim = writes.first().map(|(_, v)| v.len()).unwrap_or(0);
        let identity = embedder.identity();
        for (target, exists) in [("vec_dense", has_dense), ("vec_interp", has_interp)] {
            if !writes.iter().any(|(w, _)| w.4 == target) {
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

        for ((id, table, column, rowid, target), vector) in writes {
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
        tx.commit()?;
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

    /// Two tight clusters of unit vectors: within a cluster, neighbors are
    /// near-identical (nn_mean ≈ 1); across clusters, orthogonal (null pulls
    /// toward 0.5 with equal membership). The derivation must find
    /// nn_mean > null_mean and receipt it; a store with too few vectors must
    /// yield None and hold no receipt.
    #[test]
    fn dense_geometry_derives_from_the_corpus_and_receipts() {
        let db = mini_db();
        build_lexical_index(&db, false).unwrap();
        let conn = db.conn();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE vec_dense USING vec0(
                 embedding float[4],
                 src_table text, src_column text, src_rowid integer);
             INSERT INTO model_registry (vector_table, backend, model, dimension, quantization)
             VALUES ('vec_dense', 'test', 'toy', 4, 'f32');",
        )
        .unwrap();
        let blob = |v: [f32; 4]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let mut insert = conn
            .prepare(
                "INSERT INTO vec_dense (embedding, src_table, src_column, src_rowid)
                 VALUES (?1, 'notes', 'body', ?2)",
            )
            .unwrap();
        for i in 0..(GEOMETRY_SAMPLE as i64 * 2) {
            // Cluster by parity, with a tiny per-row wobble so nearest
            // neighbors are distinct rows, not exact duplicates.
            let eps = (i as f32) * 1e-4;
            let v = if i % 2 == 0 {
                [1.0, eps, 0.0, 0.0]
            } else {
                [0.0, 0.0, 1.0, eps]
            };
            insert
                .execute(stemmadb::rusqlite::params![blob(v), i])
                .unwrap();
        }
        drop(insert);

        let (null_mean, nn_mean) = derive_dense_geometry(&db).unwrap().expect("geometry");
        assert!(
            nn_mean > 0.9 && null_mean < 0.7 && nn_mean > null_mean,
            "clusters should separate: null {null_mean} nn {nn_mean}"
        );
        let receipt: String = conn
            .query_row(
                "SELECT value_json FROM derivations WHERE artifact = 'dense:geometry'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(receipt.contains("null_mean"));
        // Second call is served by the receipt and agrees (to the receipt's
        // six-decimal storage precision).
        let (n2, nn2) = derive_dense_geometry(&db).unwrap().expect("cached geometry");
        assert!((n2 - null_mean).abs() < 1e-5 && (nn2 - nn_mean).abs() < 1e-5);

        // Under-populated index: no geometry, no receipt left behind.
        conn.execute("DELETE FROM vec_dense WHERE rowid > 10", [])
            .unwrap();
        assert!(derive_dense_geometry(&db).unwrap().is_none());
        let left: i64 = conn
            .query_row(
                "SELECT count(*) FROM derivations WHERE artifact = 'dense:geometry'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "stale geometry receipt must be removed");
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

    /// One column of each shape, 24 rows per table so the confidence bounds
    /// have data to be confident about. All columns are TEXT-typed — the
    /// CSV-import shape where epoch floats, dates and keys arrive as
    /// strings, which is exactly when the measurements matter.
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
    fn purpose_predicates_read_the_typology_fixture() {
        let db = typology_db();
        let profiles = column_profiles(&db).unwrap();
        let profile = |t: &str, c: &str| -> &ColumnProfile {
            profiles
                .iter()
                .find(|p| p.src_table == t && p.src_column == c)
                .unwrap()
        };
        let paraphrasable = |t: &str, c: &str| is_paraphrasable_column(profile(t, c));
        // Confidently shape-structural columns are denied vocabulary status.
        assert!(!paraphrasable("events", "occurred_at"), "epoch floats");
        assert!(!paraphrasable("events", "day"), "ISO dates");
        assert!(!paraphrasable("events", "amount"), "plain decimals");
        assert!(!paraphrasable("assets", "uid"), "uuids");
        assert!(!paraphrasable("parents", "pid"), "digit keys");
        assert!(!paraphrasable("children", "parent_id"), "copied digit keys");
        // Letter-bearing prose is admitted; so, honestly, is a letter-bearing
        // code scheme — with the kind ladder's distinctness threshold gone,
        // 'SKU-0001-Q3' is not confidently numeric/temporal/idlike and the
        // downstream recurrence requirement is what keeps it harmless.
        assert!(paraphrasable("assets", "name"), "recurring names");
        assert!(paraphrasable("assets", "sku"), "letter-bearing codes");
        // Vocabulary is the same predicate under its consumer's name.
        assert!(is_vocabulary_column(profile("assets", "name")));

        // The document boundary is a corpus derivation, not a length gate:
        // only the long-body column sits beyond the natural break.
        let boundary = read_adopted_boundary(db.conn()).unwrap().unwrap();
        for p in &profiles {
            let expect = p.src_table == "assets" && p.src_column == "body";
            assert_eq!(
                is_document_column(p, &boundary),
                expect,
                "document-ness of {}.{} (median {})",
                p.src_table,
                p.src_column,
                p.median_len
            );
        }
        // And vocabulary_columns excludes the document column.
        let vocab = vocabulary_columns(&db).unwrap();
        assert!(!vocab.contains(&("assets".into(), "body".into())));
        assert!(vocab.contains(&("assets".into(), "name".into())));

        // Measurement spot checks: LCBs live beside their point ratios and
        // are strictly tempered by sample size.
        let p = profile("events", "occurred_at");
        assert_eq!(p.n_values, 24);
        assert_eq!(p.distinct_ratio, 1.0);
        assert_eq!(p.temporal_ratio, 1.0);
        assert!(p.temporal_lcb > 0.5 && p.temporal_lcb < 1.0);
        // is_doc is stamped column-wide from the same boundary.
        let stamped: i64 = db
            .conn()
            .query_row(
                "SELECT count(DISTINCT src_column) FROM lex_values WHERE is_doc = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamped, 1, "exactly the body column is stamped is_doc");
    }

    #[test]
    fn jeffreys_lcb_makes_small_samples_humble() {
        // Certainty must be earned: a perfect ratio bounds very differently
        // at n = 5 and n = 80.
        // Reference values from the Beta(k+1/2, n-k+1/2) 5% quantile.
        let small = jeffreys_lcb(5, 5);
        let large = jeffreys_lcb(80, 80);
        assert!((small - 0.69425).abs() < 1e-4, "5/5: {small}");
        assert!((large - 0.97635).abs() < 1e-4, "80/80: {large}");
        assert!(small < 0.75, "5/5 stays weak evidence: {small}");
        assert!(large > 0.95, "80/80 is strong evidence: {large}");
        assert!(jeffreys_lcb(0, 0) == 0.0);
        assert!(jeffreys_lcb(0, 10) < 0.02);
        // Monotone in evidence.
        assert!(jeffreys_lcb(10, 10) > jeffreys_lcb(5, 5));
        assert!(jeffreys_lcb(8, 10) > jeffreys_lcb(5, 10));
        // And the bound is a lower bound.
        for (k, n) in [(3i64, 7i64), (7, 7), (1, 30), (29, 30)] {
            assert!(jeffreys_lcb(k, n) <= k as f64 / n as f64);
        }
    }

    /// The scenario KIND_CARDINALITY_MIN_VALUES existed to protect, now
    /// protected by arithmetic: five rows, all distinct, all letter-bearing.
    /// A point ratio would read distinct_ratio = 1.0 and idlike-style rules
    /// could condemn the column; the Jeffreys bounds stay too weak for any
    /// structural denial, so the column keeps its vocabulary status.
    #[test]
    fn five_distinct_rows_cannot_be_condemned() {
        let db = StemmaDb::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE src.towns(id INTEGER PRIMARY KEY, name TEXT);
                 INSERT INTO src.towns VALUES
                    (1, 'Arcata'), (2, 'Bodega'), (3, 'Cambria'),
                    (4, 'Dunsmuir'), (5, 'Eureka');",
            )
            .unwrap();
        build_lexical_index(&db, false).unwrap();
        let profiles = column_profiles(&db).unwrap();
        let p = profiles
            .iter()
            .find(|p| p.src_column == "name")
            .unwrap();
        assert_eq!(p.distinct_ratio, 1.0);
        assert!(
            p.distinct_lcb < 0.7,
            "all-distinct at n=5 is a weak claim: {}",
            p.distinct_lcb
        );
        assert!(is_paraphrasable_column(p));
        assert!(is_vocabulary_column(p));
    }

    #[test]
    fn otsu_finds_the_natural_break_and_declines_degenerate_input() {
        // Bimodal: the cut lands in the wide gap.
        let cut = otsu_cut(&[3.4, 3.5, 3.4, 8.0, 8.1]).unwrap();
        assert!((3.5..=8.0).contains(&cut), "got {cut}");
        // Point mass and singletons have no break.
        assert_eq!(otsu_cut(&[2.0, 2.0, 2.0]), None);
        assert_eq!(otsu_cut(&[2.0]), None);
        assert_eq!(otsu_cut(&[]), None);
    }

    #[test]
    fn doc_boundary_derivation_covers_the_corpus_shapes() {
        let prose = ((EXACT_MAX_LEN + 1) as f64).ln();
        // Legal-shaped: short metadata columns, kilochar bodies. The break
        // separates them and only the bodies are documents.
        let b = derive_doc_boundary(&[9.0, 36.0, 57.0, 2660.0, 16151.0]);
        let doc = |median: f64, b: &DocBoundary| {
            is_document_column(
                &ColumnProfile {
                    src_table: String::new(),
                    src_column: String::new(),
                    n_values: 0,
                    n_distinct: 0,
                    distinct_ratio: 0.0,
                    distinct_lcb: 0.0,
                    alpha_ratio: 0.0,
                    alpha_lcb: 0.0,
                    numeric_ratio: 0.0,
                    numeric_lcb: 0.0,
                    temporal_ratio: 0.0,
                    temporal_lcb: 0.0,
                    idlike_ratio: 0.0,
                    idlike_lcb: 0.0,
                    avg_len: 0.0,
                    median_len: median,
                },
                b,
            )
        };
        assert!(doc(2660.0, &b) && doc(16151.0, &b));
        assert!(!doc(9.0, &b) && !doc(36.0, &b) && !doc(57.0, &b));
        // The break is corpus-relative: 150-char medians cluster with the
        // values when the documents run to thousands, where any fixed
        // threshold near 120 would have called them documents.
        let b = derive_doc_boundary(&[30.0, 90.0, 150.0, 3000.0]);
        assert!(!doc(150.0, &b), "150 clusters with values in this corpus");
        assert!(doc(3000.0, &b));
        // BIRD-shaped: everything value-scale. The Otsu break sits among
        // short medians, the value-scale floor prevails, no documents.
        let b = derive_doc_boundary(&[3.0, 7.0, 11.0, 19.0, 36.0]);
        match &b {
            DocBoundary::Cut(c) => assert!((*c - prose).abs() < 1e-12),
            other => panic!("expected value-scale cut, got {other:?}"),
        }
        assert!(!doc(36.0, &b));
        // Uniformly prose: a pure document corpus, however it clusters.
        assert_eq!(derive_doc_boundary(&[500.0, 4000.0]), DocBoundary::AllDocs);
        assert_eq!(derive_doc_boundary(&[1000.0]), DocBoundary::AllDocs);
        // Degenerate single short column: no documents.
        let b = derive_doc_boundary(&[12.0]);
        assert!(!doc(12.0, &b));
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
        // With pair-aware recurrence the warehouse enqueues NOTHING: every
        // cross-column value here is a denormalized copy (product_name/sku
        // copied into the fact table), i.e. systematic majority overlap —
        // and a copy is one fact stored twice, not two readings of one
        // string. Issue #2's own framing, now fully honored: the 98 cards
        // the typed-but-pair-blind predicate still queued were copies too.
        assert_eq!(queued, 0, "untyped predicate would enqueue {untyped}");
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

    #[test]
    fn refresh_reingests_only_changed_tables() {
        let db = mini_db();
        let first = build_lexical_index(&db, false).unwrap();
        assert_eq!((first.rebuilt, first.reingested_tables), (true, 2));
        let receipt_fp = |artifact: &str| -> String {
            db.conn()
                .query_row(
                    "SELECT input_fingerprint FROM derivations WHERE artifact = ?1",
                    [artifact],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let offices_fp = receipt_fp("lex:offices");
        let notes_fp = receipt_fp("lex:notes");
        let profiles_fp = receipt_fp("profiles");

        // The user database grows a row (writable src: in-memory fixture).
        db.conn()
            .execute(
                "INSERT INTO src.notes VALUES (2, 'harbor throughput improving', 9)",
                [],
            )
            .unwrap();
        let second = build_lexical_index(&db, false).unwrap();
        assert!(second.rebuilt);
        assert_eq!(second.reingested_tables, 1, "only notes changed");
        assert_eq!(second.values, 6, "the new cell is indexed");
        // The FTS mirrors follow the changed table.
        let hits: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM lex_fts WHERE lex_fts MATCH 'harbor'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
        // Receipts: the changed table and the corpus-level profiles moved,
        // the untouched table's receipt did not.
        assert_eq!(receipt_fp("lex:offices"), offices_fp);
        assert_ne!(receipt_fp("lex:notes"), notes_fp);
        assert_ne!(receipt_fp("profiles"), profiles_fp);

        // Steady state: nothing to do.
        let third = build_lexical_index(&db, false).unwrap();
        assert_eq!((third.rebuilt, third.reingested_tables), (false, 0));
    }

    #[test]
    fn refresh_follows_in_place_edits_and_dropped_tables() {
        let db = mini_db();
        build_lexical_index(&db, false).unwrap();
        // An in-place edit that changes the rowid sum is caught. (An edit
        // preserving count/max/sum is the documented blind spot of the
        // fingerprint; `force` covers it.)
        db.conn()
            .execute_batch(
                "DELETE FROM src.notes WHERE id = 1;
                 INSERT INTO src.notes VALUES (3, 'margins compressed sharply', 7);",
            )
            .unwrap();
        let stats = build_lexical_index(&db, false).unwrap();
        assert_eq!(stats.reingested_tables, 1);
        let old: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM lex_values WHERE value LIKE '%back on track%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 0, "replaced rows leave the index");
        // Dropping a table sweeps its rows and receipt.
        db.conn().execute_batch("DROP TABLE src.notes;").unwrap();
        let stats = build_lexical_index(&db, false).unwrap();
        assert!(stats.rebuilt);
        let (rows, receipts): (i64, i64) = db
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM lex_values WHERE src_table = 'notes'),
                        (SELECT count(*) FROM derivations WHERE artifact = 'lex:notes')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((rows, receipts), (0, 0));
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

    /// A corpus with actual documents: three long bodies whose column sits
    /// beyond the derived boundary, and short values that must stay out of
    /// the document queue.
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
                 -- one shared title of three: incidental sharing (ambiguity)
                 -- on both sides, never a majority (a copy/key pair)
                 INSERT INTO src.tags VALUES
                    (1, 'Coastal permits'), (2, 'Archived'), (3, 'Pending review'),
                    (4, 'Superseded'), (5, 'Draft');
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

    /// Writes a vec_staging table the way a loader would. `template`:
    /// `Some` = the loader's shape (query_template column), `None` = a
    /// loader that predates the column.
    fn stage_vectors(db: &StemmaDb, model: &str, dim: usize, template: Option<&str>) {
        let conn = db.conn();
        let template_col = if template.is_some() {
            ",\n query_template TEXT NOT NULL"
        } else {
            ""
        };
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS vec_staging;
             CREATE TABLE vec_staging (
                 src_table  TEXT NOT NULL,
                 src_column TEXT NOT NULL,
                 src_rowid  INTEGER NOT NULL,
                 dim        INTEGER NOT NULL,
                 model      TEXT NOT NULL,
                 embedding  BLOB NOT NULL{template_col}
             );"
        ))
        .unwrap();
        for rowid in 1..=3i64 {
            let blob: Vec<u8> = (0..dim)
                .flat_map(|i| ((rowid as f32) + i as f32).to_le_bytes())
                .collect();
            match template {
                Some(t) => conn
                    .execute(
                        "INSERT INTO vec_staging VALUES ('articles', 'body', ?1, ?2, ?3, ?4, ?5)",
                        stemmadb::rusqlite::params![rowid, dim as i64, model, blob, t],
                    )
                    .unwrap(),
                None => conn
                    .execute(
                        "INSERT INTO vec_staging VALUES ('articles', 'body', ?1, ?2, ?3, ?4)",
                        stemmadb::rusqlite::params![rowid, dim as i64, model, blob],
                    )
                    .unwrap(),
            };
        }
    }

    #[test]
    fn staged_promotion_carries_the_query_template_into_the_registry() {
        let db = StemmaDb::open_in_memory().unwrap();
        stage_vectors(
            &db,
            "qwen3-emb-legal-v3",
            4,
            Some(stemma_embed::QWEN3_QUERY_TEMPLATE),
        );
        let stats = build_dense_index(&db).unwrap().unwrap();
        assert!(stats.promoted);
        assert_eq!(stats.query_template, stemma_embed::QWEN3_QUERY_TEMPLATE);
        // The registry row is the space's identity: model, dimension AND the
        // convention its queries must use — no name-family guess survives.
        let (model, dim, template): (String, i64, String) = db
            .conn()
            .query_row(
                "SELECT model, dimension, query_template FROM model_registry
                 WHERE vector_table = 'vec_dense'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(model, "qwen3-emb-legal-v3");
        assert_eq!(dim, 4);
        assert_eq!(template, stemma_embed::QWEN3_QUERY_TEMPLATE);
        // Re-promotion (blue-green restage) updates the stored template.
        stage_vectors(&db, "qwen3-emb-legal-v3", 4, Some("{query}"));
        let stats = build_dense_index(&db).unwrap().unwrap();
        assert_eq!(stats.query_template, "{query}");
        let template: String = db
            .conn()
            .query_row(
                "SELECT query_template FROM model_registry WHERE vector_table = 'vec_dense'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(template, "{query}");
    }

    #[test]
    fn prelegacy_staging_promotes_with_no_recorded_template() {
        let db = StemmaDb::open_in_memory().unwrap();
        stage_vectors(&db, "some-encoder", 4, None);
        let stats = build_dense_index(&db).unwrap().unwrap();
        assert!(stats.promoted);
        assert_eq!(
            stats.query_template, "",
            "a loader without the column records nothing, not a guess"
        );
        let template: String = db
            .conn()
            .query_row(
                "SELECT query_template FROM model_registry WHERE vector_table = 'vec_dense'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(template, "");
    }

    #[test]
    fn mismatched_registry_template_refuses_and_fails_items() {
        let db = doc_db();
        // Same model, but the space is registered under the instruction
        // convention while the embedder would query it bare: the third leg
        // of the identity disagrees, and appending is refused exactly like
        // a model mismatch.
        db.conn()
            .execute(
                "INSERT INTO model_registry (vector_table, backend, model, dimension,
                                             query_template)
                 VALUES ('vec_dense', 'staged', 'fake-embedder', 8, ?1)",
                [stemma_embed::QWEN3_QUERY_TEMPLATE],
            )
            .unwrap();
        enqueue_missing_embeddings(&db).unwrap();
        let err = drain_embed_queue(&db, &FakeEmbedder::new(8), EMBED_BATCH).unwrap_err();
        assert!(matches!(err, Error::QueryTemplateMismatch { .. }), "got {err}");
        assert_eq!(queue_counts(&db), (0, 0, 3));
    }

    #[test]
    fn bare_template_spellings_do_not_refuse_the_drain() {
        let db = doc_db();
        // Explicitly-bare registration ("{query}") and a bare embedder ("")
        // are one convention spelled two ways, not a mismatch. An empty
        // registered template would not constrain at all — but this row
        // states bare, and the bare embedder agrees.
        db.conn()
            .execute(
                "INSERT INTO model_registry (vector_table, backend, model, dimension,
                                             query_template)
                 VALUES ('vec_dense', 'staged', 'fake-embedder', 8, '{query}')",
                [],
            )
            .unwrap();
        db.conn()
            .execute_batch(
                "CREATE VIRTUAL TABLE vec_dense USING vec0(
                     embedding float[8],
                     src_table text,
                     src_column text,
                     src_rowid integer
                 );",
            )
            .unwrap();
        enqueue_missing_embeddings(&db).unwrap();
        let stats = drain_embed_queue(&db, &FakeEmbedder::new(8), EMBED_BATCH).unwrap();
        assert_eq!((stats.drained, stats.failed), (3, 0));
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
                    (19, 'Portland Airport', 'Portland'),
                    (20, 'Tacoma Mall', 'Tacoma'),
                    (21, 'Spokane Valley', 'Spokane'),
                    (22, 'Olympia Center', 'Olympia');
                 -- landmarks shares SOME values with offices (incidental
                 -- sharing = ambiguity); majority overlap would be a
                 -- key/copy relationship and excluded pair-wise
                 CREATE TABLE src.landmarks(id INTEGER PRIMARY KEY, title TEXT);
                 INSERT INTO src.landmarks VALUES
                    (1, 'Seattle'), (2, 'Portland'), (3, 'Portland Downtown'),
                    (4, 'Space Needle'), (5, 'Gasworks Park'), (6, 'Multnomah Falls');",
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
                 INSERT INTO src.tags VALUES
                    (1, 'Outerwear & Coats'), (2, 'Dresses'),
                    (3, 'Clearance'), (4, 'New Arrivals'), (5, 'Staff Picks');
                 -- keep category majority-unique so tags↔category sharing is
                 -- incidental (ambiguity), not systematic (a copy/key pair)
                 INSERT INTO src.items VALUES
                    (901, 'Jeans', 'Wrangler', '1700000000.0'),
                    (902, 'Suits & Sport Coats', 'Halston', '1700000000.0'),
                    (903, 'Swimwear', 'Speedo', '1700000000.0');",
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
        // exactly one title is shared with src.tags (incidental), so that
        // value yields two readings; the copies-of-everything case is the
        // warehouse test, which now expects zero
        assert_eq!((docs, interps), (3, 2));
        let stats = drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!((stats.drained, stats.failed, stats.remaining), (5, 0, 0));
        let count = |table: &str| -> i64 {
            db.conn()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!((count("vec_dense"), count("vec_interp")), (3, 2));
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
    fn changed_document_resets_only_its_own_item() {
        let db = doc_db();
        let embedder = FakeEmbedder::new(8);
        enqueue_missing_embeddings(&db).unwrap();
        drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!(queue_counts(&db), (0, 3, 0));

        // One document is edited in the user database.
        let revised = format!(
            "Amended article on insurance filings. {}",
            "The amended body still repeats itself with modest dignity until \
             it is unmistakably prose rather than a value. "
                .repeat(3)
        );
        db.conn()
            .execute(
                "UPDATE src.articles SET body = ?1 WHERE id = 2",
                [&revised],
            )
            .unwrap();
        // An in-place UPDATE preserves count:max:sum — the fingerprint's
        // documented blind spot — so this is the `force` path. The point of
        // the content hash is that even a full re-ingest re-embeds only what
        // actually changed.
        build_lexical_index(&db, true).unwrap();

        // The content hash catches exactly the changed row: its stale vector
        // is dropped and the item re-pends; the other two stay done.
        let queued = enqueue_missing_embeddings(&db).unwrap();
        assert_eq!(queued, 1, "only the edited document re-embeds");
        assert_eq!(queue_counts(&db), (1, 2, 0));
        let vectors: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM vec_dense", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 2, "the stale vector is gone");

        drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!(queue_counts(&db), (0, 3, 0));
        let stored: Vec<u8> = db
            .conn()
            .query_row(
                "SELECT embedding FROM vec_dense WHERE src_rowid = 2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let expected: Vec<u8> = embedder
            .vector(&revised)
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        assert_eq!(stored, expected, "the new text is what got embedded");
        // Steady state after the re-embed.
        assert_eq!(enqueue_missing_embeddings(&db).unwrap(), 0);
    }

    #[test]
    fn changed_context_resets_interpretation_cards() {
        let db = value_db();
        let embedder = FakeEmbedder::new(8);
        enqueue_missing_interpretations(&db).unwrap();
        drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!(queue_counts(&db), (0, 6, 0));

        // The 'Seattle' city interpretation's context fragment comes from
        // offices.name of the representative row; renaming the office
        // changes the card without changing the value itself.
        db.conn()
            .execute(
                "UPDATE src.offices SET name = 'Seattle - Ballard' WHERE id = 17",
                [],
            )
            .unwrap();
        // In-place UPDATE: outside the fingerprint's reach, so force the
        // re-ingest; the hashes keep everything else `done`.
        build_lexical_index(&db, true).unwrap();
        let queued = enqueue_missing_interpretations(&db).unwrap();
        assert_eq!(queued, 1, "only the card whose text changed resets");
        let card: String = db
            .conn()
            .query_row(
                "SELECT serialized FROM embed_queue
                 WHERE src_table = 'offices' AND src_column = 'city' AND src_rowid = 17",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(card, "offices · city · Seattle · name: Seattle - Ballard");
        let vectors: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM vec_interp", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 5, "the stale card vector is gone");
        drain_embed_queue(&db, &embedder, EMBED_BATCH).unwrap();
        assert_eq!(queue_counts(&db), (0, 6, 0));
        assert_eq!(enqueue_missing_interpretations(&db).unwrap(), 0);
    }

    /// The hysteresis discipline on the document cut: `current` re-derives
    /// on every profile pass, `adopted` moves only when adopting would
    /// change some column's document-ness, and an adoption that flips a
    /// column resets that column's embed items.
    #[test]
    fn doc_cut_hysteresis_governs_adoption() {
        let db = StemmaDb::open_in_memory().unwrap();
        let body = "Substantive regulatory prose, repeated until the column is \
                    unambiguously a document column in any derivation. "
            .repeat(6); // ~700 chars
        let summary = "A mid-length abstract of the article, long enough to sit \
                       between value scale and the document bodies, repeated once \
                       more for measure and then again for length and balance."
            .to_string(); // ~180 chars
        db.conn()
            .execute_batch(&format!(
                "CREATE TABLE src.papers(id INTEGER PRIMARY KEY, title TEXT, body TEXT);
                 CREATE TABLE src.notes(id INTEGER PRIMARY KEY, summary TEXT);
                 INSERT INTO src.papers VALUES
                    (1, 'tidal power', '{body}'),
                    (2, 'grid balancing', '{body}'),
                    (3, 'peak shaving', '{body}');
                 INSERT INTO src.notes VALUES (1, '{summary}'), (2, '{summary}');"
            ))
            .unwrap();
        build_lexical_index(&db, false).unwrap();
        let cuts = |db: &StemmaDb| -> (f64, f64) {
            db.conn()
                .query_row(
                    "SELECT json_extract(value_json, '$.current.cut'),
                            json_extract(value_json, '$.adopted.cut')
                     FROM derivations WHERE artifact = 'doc_cut'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
        };
        let (current, adopted) = cuts(&db);
        assert_eq!(current, adopted, "first derivation adopts itself");
        // In this corpus the natural break falls between titles and the
        // prose columns: summaries and bodies are both documents.
        let doc_cols: i64 = db
            .conn()
            .query_row(
                "SELECT count(DISTINCT src_table || '.' || src_column)
                 FROM lex_values WHERE is_doc = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(doc_cols, 2, "summary and body are the prose class");

        // A previously adopted cut sitting elsewhere in the same gap (no
        // column between the two cuts): re-deriving must NOT re-adopt.
        let jitter = current - 0.05;
        db.conn()
            .execute(
                "UPDATE derivations SET value_json =
                     json_set(value_json, '$.adopted.cut', ?1)
                 WHERE artifact = 'doc_cut'",
                [jitter],
            )
            .unwrap();
        profile_columns(&db).unwrap();
        let (current2, adopted2) = cuts(&db);
        assert_eq!(current2, current, "the cut re-derives freely");
        assert_eq!(adopted2, jitter, "same document-ness: adoption is skipped");

        // A previously adopted cut ABOVE the summary column's median: under
        // it summaries are values, under the fresh cut they are documents —
        // document-ness changes, so the fresh cut IS adopted, and the
        // flipped column's embed items are reset for re-enqueue.
        let summary_log = {
            let m: f64 = db
                .conn()
                .query_row(
                    "SELECT median_len FROM lex_columns WHERE src_column = 'summary'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (1.0 + m).ln()
        };
        assert!(summary_log > current, "fixture: summaries sit above the cut");
        db.conn()
            .execute(
                "UPDATE derivations SET value_json =
                     json_set(value_json, '$.adopted.cut', ?1)
                 WHERE artifact = 'doc_cut'",
                [summary_log + 0.1],
            )
            .unwrap();
        // Make the store consistent with that pretended past: summaries
        // stamped as values, with a (card) queue item and its vector state.
        db.conn()
            .execute_batch(
                "UPDATE lex_values SET is_doc = 0 WHERE src_column = 'summary';
                 INSERT INTO embed_queue (src_table, src_column, src_rowid, serialized, status)
                 VALUES ('notes', 'summary', 1, 'notes · summary · …', 'done');",
            )
            .unwrap();
        profile_columns(&db).unwrap();
        let (current3, adopted3) = cuts(&db);
        assert_eq!(current3, current);
        assert_eq!(adopted3, current, "document-ness changed: adoption happens");
        let (stamped, leftover): (i64, i64) = db
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM lex_values
                         WHERE src_column = 'summary' AND is_doc = 0),
                        (SELECT count(*) FROM embed_queue WHERE src_column = 'summary')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(stamped, 0, "summaries re-stamped as documents");
        assert_eq!(leftover, 0, "the flipped column's items were reset");
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
