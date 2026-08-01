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
    value_norm TEXT NOT NULL
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
            "INSERT INTO lex_values (src_table, src_column, src_rowid, value, value_norm)
             SELECT ?1, ?2, rowid, \"{col}\", lower(trim(\"{col}\"))
             FROM {src}.\"{tbl}\"
             WHERE \"{col}\" IS NOT NULL AND trim(\"{col}\") != ''",
            col = tc.column,
            tbl = tc.table,
            src = SRC_SCHEMA,
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
