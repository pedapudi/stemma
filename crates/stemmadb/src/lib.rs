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
pub const STORE_SCHEMA_VERSION: i32 = 5;

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
        assert_eq!(
            rc,
            rusqlite::ffi::SQLITE_OK,
            "registering sqlite-vec failed"
        );
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
        if found > STORE_SCHEMA_VERSION {
            return Err(Error::StoreVersionMismatch {
                found,
                supported: STORE_SCHEMA_VERSION,
            });
        }
        // Additive migrations: every DDL block is idempotent, so upgrading
        // is applying the full schema and stamping the new version. Only a
        // store from the FUTURE is an error.
        if found < STORE_SCHEMA_VERSION {
            // v4 reshaped embed_queue (per-column work items, status/attempt
            // tracking); the old shape had a narrower unique key, which no
            // ALTER can widen. The queue is transient work state — pre-v4
            // nothing ever drained or enqueued — so the migration drops the
            // old shape and lets SCHEMA_SQL recreate it. Guarded by shape,
            // not version, so it runs exactly when the old table exists.
            let old_queue: i64 = self
                .conn
                .query_row(
                    "SELECT (SELECT count(*) FROM sqlite_master WHERE name = 'embed_queue')
                            AND NOT (SELECT count(*) FROM pragma_table_info('embed_queue')
                                     WHERE name = 'status')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if old_queue != 0 {
                self.conn.execute_batch("DROP TABLE embed_queue;")?;
            }
            self.conn.execute_batch(SCHEMA_SQL)?;
            // v3: history attribution. ALTERs are not idempotent, so they are
            // guarded per-version rather than living in SCHEMA_SQL.
            if found == 2 {
                self.conn.execute_batch(
                    "ALTER TABLE query_log ADD COLUMN source TEXT NOT NULL DEFAULT '';
                     ALTER TABLE query_log ADD COLUMN session TEXT NOT NULL DEFAULT '';",
                )?;
            }
            // v5: per-item content hashes so a changed source row re-embeds
            // without a rebuild. Additive, so it follows the guarded-ALTER
            // pattern: SCHEMA_SQL creates the column on fresh stores, the
            // ALTER runs exactly when a v4-shaped queue exists without it.
            let missing_hash: i64 = self
                .conn
                .query_row(
                    "SELECT (SELECT count(*) FROM sqlite_master WHERE name = 'embed_queue')
                            AND NOT (SELECT count(*) FROM pragma_table_info('embed_queue')
                                     WHERE name = 'content_hash')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if missing_hash != 0 {
                self.conn.execute_batch(
                    "ALTER TABLE embed_queue ADD COLUMN content_hash TEXT NOT NULL DEFAULT '';",
                )?;
            }
            self.conn
                .pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
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

    /// Content fingerprint of one user table: `"{count}:{max_rowid}:{sum_rowid}"`.
    /// Cheap to compute (no text hashing, one aggregate scan), catches
    /// inserts, deletes and rowid churn; in-place updates that preserve all
    /// three are missed — acceptable for derived state a forced rebuild can
    /// always repair. Every deriver of per-table state (lexical index,
    /// knowledge compiler) compares against this same triple, prefixed with
    /// its own algorithm-version tag so an algorithm upgrade invalidates its
    /// receipts without touching anyone else's.
    pub fn src_table_fingerprint(&self, table: &str) -> Result<String> {
        let (n, mx, sum): (i64, i64, i64) = self.conn.query_row(
            &format!(
                "SELECT count(*), coalesce(max(rowid),0), coalesce(sum(rowid),0) \
                 FROM {SRC_SCHEMA}.\"{table}\""
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(format!("{n}:{mx}:{sum}"))
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

-- Cells awaiting (re-)embedding. Writes never wait on the embedder: ingest
-- enqueues here and an async worker drains through the Embedder backend.
-- `serialized` is the text the embedder will see; empty means "fetch the
-- stored value from lex_values at drain time" (documents are large and
-- already stored once). Status is pending → done | failed, with `attempts`
-- bounding retries and `error` recording why a failed item failed; counts
-- per status are one GROUP BY away.
-- `content_hash` (v5) fingerprints the exact text the item was enqueued to
-- embed; the enqueue passes compare it to detect a changed source row and
-- reset the item to pending, which is how re-embedding on data change works
-- without any rebuild.
CREATE TABLE IF NOT EXISTS embed_queue (
    id           INTEGER PRIMARY KEY,
    src_table    TEXT NOT NULL,
    src_column   TEXT NOT NULL,
    src_rowid    INTEGER NOT NULL,
    serialized   TEXT NOT NULL DEFAULT '',
    content_hash TEXT NOT NULL DEFAULT '',
    status       TEXT NOT NULL DEFAULT 'pending',
    attempts     INTEGER NOT NULL DEFAULT 0,
    error        TEXT NOT NULL DEFAULT '',
    enqueued_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (src_table, src_column, src_rowid)
) STRICT;
CREATE INDEX IF NOT EXISTS embed_queue_status ON embed_queue(status);

-- v2: operational history. Query history is written by the resolution
-- server; chat history by the console/agents. Both are per-database working
-- memory — queryable like everything else in the store.
CREATE TABLE IF NOT EXISTS query_log (
    id         INTEGER PRIMARY KEY,
    query      TEXT NOT NULL,
    mentions   INTEGER NOT NULL,
    elapsed_ms REAL NOT NULL,
    asked_at   TEXT NOT NULL DEFAULT (datetime('now')),
    source     TEXT NOT NULL DEFAULT '',
    session    TEXT NOT NULL DEFAULT ''
) STRICT;
CREATE INDEX IF NOT EXISTS query_log_at ON query_log(asked_at);
CREATE TABLE IF NOT EXISTS chat_log (
    id           INTEGER PRIMARY KEY,
    conversation TEXT NOT NULL DEFAULT 'default',
    role         TEXT NOT NULL,
    content      TEXT NOT NULL,
    trail        TEXT NOT NULL DEFAULT '[]',
    said_at      TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;
CREATE INDEX IF NOT EXISTS chat_log_conv ON chat_log(conversation, id);
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
    fn migrates_old_embed_queue_shape() {
        let dir = std::env::temp_dir().join(format!("stemmadb-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let store = dir.join("user.stemmadb");
        Connection::open(&user).unwrap();
        {
            // A pre-v4 store: embed_queue in its original shape, no status.
            let c = Connection::open(&store).unwrap();
            c.execute_batch(
                "CREATE TABLE embed_queue (
                     id INTEGER PRIMARY KEY,
                     src_table TEXT NOT NULL,
                     src_rowid INTEGER NOT NULL,
                     serialized TEXT NOT NULL,
                     enqueued_at TEXT NOT NULL DEFAULT (datetime('now')),
                     UNIQUE (src_table, src_rowid)
                 ) STRICT;
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        let db = StemmaDb::open(&store, &user).unwrap();
        let v: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, STORE_SCHEMA_VERSION);
        for col in ["src_column", "status", "attempts", "error", "content_hash"] {
            let n: i64 = db
                .conn()
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('embed_queue') WHERE name = ?1",
                    [col],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing embed_queue column {col}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrates_v4_queue_by_adding_content_hash() {
        let dir = std::env::temp_dir().join(format!("stemmadb-migrate5-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let store = dir.join("user.stemmadb");
        let _ = std::fs::remove_file(&user);
        let _ = std::fs::remove_file(&store);
        Connection::open(&user).unwrap();
        {
            // A v4 store: the queue has status tracking but no content_hash,
            // and holds a done item whose row must survive the upgrade.
            let c = Connection::open(&store).unwrap();
            c.execute_batch(
                "CREATE TABLE embed_queue (
                     id INTEGER PRIMARY KEY,
                     src_table TEXT NOT NULL,
                     src_column TEXT NOT NULL,
                     src_rowid INTEGER NOT NULL,
                     serialized TEXT NOT NULL DEFAULT '',
                     status TEXT NOT NULL DEFAULT 'pending',
                     attempts INTEGER NOT NULL DEFAULT 0,
                     error TEXT NOT NULL DEFAULT '',
                     enqueued_at TEXT NOT NULL DEFAULT (datetime('now')),
                     updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                     UNIQUE (src_table, src_column, src_rowid)
                 ) STRICT;
                 INSERT INTO embed_queue (src_table, src_column, src_rowid, status)
                 VALUES ('articles', 'body', 1, 'done');
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        }
        let db = StemmaDb::open(&store, &user).unwrap();
        let (status, hash): (String, String) = db
            .conn()
            .query_row(
                "SELECT status, content_hash FROM embed_queue WHERE src_rowid = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "done", "additive migration keeps queue state");
        assert_eq!(hash, "", "pre-v5 items carry the unknown-content default");
        let v: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, STORE_SCHEMA_VERSION);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn src_table_fingerprint_tracks_content_changes() {
        let db = StemmaDb::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE src.t(id INTEGER PRIMARY KEY, v TEXT);
                 INSERT INTO src.t VALUES (1, 'a'), (2, 'b');",
            )
            .unwrap();
        let a = db.src_table_fingerprint("t").unwrap();
        assert_eq!(a, "2:2:3");
        db.conn()
            .execute("INSERT INTO src.t VALUES (3, 'c')", [])
            .unwrap();
        let b = db.src_table_fingerprint("t").unwrap();
        assert_ne!(a, b, "inserts move the fingerprint");
        db.conn()
            .execute("DELETE FROM src.t WHERE id = 3", [])
            .unwrap();
        assert_eq!(db.src_table_fingerprint("t").unwrap(), a);
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
