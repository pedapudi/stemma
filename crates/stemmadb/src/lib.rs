//! stemmadb: the storage layer of the stemma ecosystem.
//!
//! Owns all SQLite access. The user's database stays stock and is attached
//! read-only; everything stemma derives — index tables, the compiled knowledge
//! store, the embed queue, the model registry — lives in a sidecar `.stemmadb`
//! file (itself a SQLite database) opened as the main database of the
//! connection. SQLite itself is unmodified: capability comes from core modules
//! (FTS5 with the trigram tokenizer, JSON1, R-Tree) plus the statically
//! registered sqlite-vec extension.

use std::path::Path;
use std::sync::Once;

use rusqlite::Connection;

pub use rusqlite;

/// Schema version of the .stemmadb store, kept in `PRAGMA user_version`.
pub const STORE_SCHEMA_VERSION: i32 = 1;

/// Name under which the user database is attached.
pub const SRC_SCHEMA: &str = "src";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        ".stemmadb store has schema version {found}, this build supports {supported}; \
         re-ingest the database"
    )]
    StoreVersionMismatch { found: i32, supported: i32 },
}

pub type Result<T> = std::result::Result<T, Error>;

unsafe extern "C" {
    fn sqlite3_vec_init(
        db: *mut rusqlite::ffi::sqlite3,
        pz_err_msg: *mut *mut std::os::raw::c_char,
        p_api: *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;
}

/// Registers bundled extensions (sqlite-vec) for every future connection in
/// this process. Idempotent.
pub fn register_extensions() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let rc = rusqlite::ffi::sqlite3_auto_extension(Some(sqlite3_vec_init));
        assert_eq!(rc, rusqlite::ffi::SQLITE_OK, "registering sqlite-vec failed");
    });
}

/// A stemma store bound to one user database.
pub struct StemmaDb {
    conn: Connection,
}

impl StemmaDb {
    /// Opens (creating if needed) the `.stemmadb` store at `store_path` and
    /// attaches the user database at `user_db_path` read-only as [`SRC_SCHEMA`].
    pub fn open(store_path: &Path, user_db_path: &Path) -> Result<Self> {
        register_extensions();
        let conn = Connection::open(store_path)?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "foreign_keys", "on")?;
        let uri = format!(
            "file:{}?mode=ro",
            user_db_path.to_string_lossy().replace('?', "%3f")
        );
        conn.execute(
            &format!("ATTACH DATABASE ?1 AS {SRC_SCHEMA}"),
            rusqlite::params![uri],
        )?;
        let db = Self { conn };
        db.init_store_schema()?;
        Ok(db)
    }

    /// Opens an in-memory store attached to an in-memory user DB — test use.
    pub fn open_in_memory() -> Result<Self> {
        register_extensions();
        let conn = Connection::open_in_memory()?;
        conn.execute(&format!("ATTACH DATABASE ':memory:' AS {SRC_SCHEMA}"), [])?;
        let db = Self { conn };
        db.init_store_schema()?;
        Ok(db)
    }

    fn init_store_schema(&self) -> Result<()> {
        let found: i32 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found == 0 {
            self.conn.execute_batch(SCHEMA_SQL)?;
            self.conn
                .pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
        } else if found != STORE_SCHEMA_VERSION {
            return Err(Error::StoreVersionMismatch {
                found,
                supported: STORE_SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    /// The underlying connection. The `main` schema is the .stemmadb store;
    /// the user database is the attached [`SRC_SCHEMA`].
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Version string of the statically linked sqlite-vec extension.
    pub fn vec_version(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT vec_version()", [], |r| r.get(0))?)
    }

    /// True if the FTS5 module (with which all lexical indexes are built) is
    /// present.
    pub fn has_fts5(&self) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM pragma_module_list WHERE name = 'fts5'",
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Tables of the attached user database (excluding SQLite internals).
    pub fn src_tables(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT name FROM {SRC_SCHEMA}.sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ))?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(names)
    }
}

/// Tables owned by the store. Index tables (FTS5/vec0) are created during
/// ingest per source table; only fixed bookkeeping tables live here.
const SCHEMA_SQL: &str = r#"
-- Which embedding model produced the vectors of each vector table. A model
-- change never mutates in place: a new vector table is backfilled and swapped
-- in (blue-green), so vector spaces are never silently mixed.
CREATE TABLE IF NOT EXISTS model_registry (
    vector_table TEXT PRIMARY KEY,
    backend      TEXT NOT NULL,
    model        TEXT NOT NULL,
    revision     TEXT NOT NULL DEFAULT '',
    dimension    INTEGER NOT NULL,
    quantization TEXT NOT NULL DEFAULT 'f32',
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;

-- Rows awaiting (re-)embedding. Writes never wait on the embedder: ingest
-- enqueues here and an async worker drains through the Embedder backend.
CREATE TABLE IF NOT EXISTS embed_queue (
    id           INTEGER PRIMARY KEY,
    src_table    TEXT NOT NULL,
    src_rowid    INTEGER NOT NULL,
    serialized   TEXT NOT NULL,
    enqueued_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (src_table, src_rowid)
) STRICT;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_registers_extensions() {
        let db = StemmaDb::open_in_memory().expect("open");
        let v = db.vec_version().expect("vec_version");
        assert!(v.starts_with('v'), "unexpected vec_version: {v}");
        assert!(db.has_fts5().unwrap());
    }

    #[test]
    fn store_schema_is_versioned() {
        let db = StemmaDb::open_in_memory().unwrap();
        let v: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, STORE_SCHEMA_VERSION);
        for t in ["model_registry", "embed_queue"] {
            let n: i64 = db
                .conn()
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing table {t}");
        }
    }

    #[test]
    fn attaches_user_db_read_only() {
        let dir = std::env::temp_dir().join(format!("stemmadb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let store = dir.join("user.stemmadb");
        {
            let c = Connection::open(&user).unwrap();
            c.execute_batch(
                "CREATE TABLE offices(id INTEGER PRIMARY KEY, name TEXT, city TEXT);
                 INSERT INTO offices VALUES (17, 'Seattle - Northgate', 'Seattle');",
            )
            .unwrap();
        }
        let db = StemmaDb::open(&store, &user).unwrap();
        assert_eq!(db.src_tables().unwrap(), vec!["offices".to_string()]);
        let err = db
            .conn()
            .execute("INSERT INTO src.offices VALUES (1, 'x', 'y')", []);
        assert!(err.is_err(), "user DB must be read-only");
        std::fs::remove_dir_all(&dir).ok();
    }
}
