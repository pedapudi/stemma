//! stemma-ingest: builds derived indexes in the .stemmadb store from the
//! attached (read-only) user database.
//!
//! Current scope: the lexical value index — every text value of every user
//! table, indexed three ways: normalized for exact lookup, FTS5/unicode61 for
//! BM25 token search, FTS5/trigram for fuzzy and substring matching. This is
//! the milestone-2 candidate-generation substrate; the dense (vec0) and
//! knowledge-store compilation passes join it in later milestones.

use stemmadb::{StemmaDb, SRC_SCHEMA};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] stemmadb::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] stemmadb::rusqlite::Error),
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
}
