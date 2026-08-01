//! stemma-kg: the knowledge store.
//!
//! The knowledge graph is layered, and its construction follows the practices
//! that survive in current literature and production systems:
//!
//! - **Schema layer** — tables, columns, declared foreign keys (the join
//!   graph that schema-linking work like SchemaGraphSQL exploits).
//! - **Discovered relations** — undeclared join paths found by
//!   inclusion-dependency mining (containment of one column's values in
//!   another's key column). Real datasets rarely declare their FKs; discovered
//!   edges carry a confidence and are marked `method:"inferred"` so consumers
//!   can weight them below declared ones.
//! - **Profile layer** — frequent values of value-like columns and
//!   characteristic terms of document corpora, plus **term co-occurrence**
//!   edges (the GraphRAG-lite move: give a document corpus real graph
//!   structure without an LLM pass).
//! - **Instance layer** (rest of milestone 4) — per-record entities, aliases,
//!   embedding-assisted entity resolution across rows.
//!
//! **Maintenance is incremental.** Every compiled table records a content
//! fingerprint; recompiles touch only tables whose fingerprint changed (the
//! queue/dirty-tracking pattern that replaced batch KG rebuilds in production
//! RAG systems). Every edge carries provenance (`method`) and `confidence` in
//! its props — an edge you cannot explain is an edge you cannot trust.
//!
//! Everything goes through the [`KnowledgeStore`] trait: consumers program
//! against it, never against a concrete backend. The first backend is SQLite
//! tables inside the .stemmadb store; graph SQL never leaves that backend.

use stemmadb::StemmaDb;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] stemmadb::rusqlite::Error),
    #[error(transparent)]
    Db(#[from] stemmadb::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub const KIND_TABLE: &str = "table";
pub const KIND_COLUMN: &str = "column";
pub const KIND_VALUE: &str = "value";
pub const KIND_TERM: &str = "term";

#[derive(Debug, Clone)]
pub struct Node {
    /// Stable unique key, e.g. "table:offices", "column:offices.city",
    /// "value:offices.city:seattle", "term:regulations:commissioner".
    pub key: String,
    pub kind: String,
    pub label: String,
    /// JSON bag for cold properties (counts, types).
    pub props: String,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub src_key: String,
    pub dst_key: String,
    /// "has_column" | "fk" | "inferred_fk" | "frequent_value" | "term" | "cooccurs"
    pub kind: String,
    pub label: String,
    /// JSON provenance: {"method":"declared|inferred|profiled","confidence":0.97,...}
    pub props: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KgStats {
    pub nodes: usize,
    pub edges: usize,
    /// Tables recompiled in the last `compile` call (0 = everything fresh).
    pub recompiled_tables: usize,
}

/// The backend-agnostic knowledge store. Query-side methods (neighbors,
/// bounded path search, subgraph extraction) join as collective
/// disambiguation lands; keep additions here, not on concrete backends.
pub trait KnowledgeStore {
    fn upsert_node(&self, node: &Node) -> Result<()>;
    fn upsert_edge(&self, edge: &Edge) -> Result<()>;
    /// Removes every node whose key starts with any of the prefixes, plus
    /// edges touching them. The unit of incremental recompilation.
    fn remove_by_key_prefixes(&self, prefixes: &[String]) -> Result<()>;
    fn stats(&self) -> Result<KgStats>;
}

/// SQLite backend: kg_nodes/kg_edges/kg_meta inside the .stemmadb store.
pub struct SqliteKnowledgeStore<'a> {
    db: &'a StemmaDb,
}

const KG_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kg_nodes (
    id    INTEGER PRIMARY KEY,
    key   TEXT NOT NULL UNIQUE,
    kind  TEXT NOT NULL,
    label TEXT NOT NULL,
    props TEXT NOT NULL DEFAULT '{}'
) STRICT;
CREATE INDEX IF NOT EXISTS kg_nodes_kind ON kg_nodes(kind);
CREATE TABLE IF NOT EXISTS kg_edges (
    src   INTEGER NOT NULL REFERENCES kg_nodes(id),
    dst   INTEGER NOT NULL REFERENCES kg_nodes(id),
    kind  TEXT NOT NULL,
    label TEXT NOT NULL DEFAULT '',
    props TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (src, dst, kind)
) WITHOUT ROWID;
-- Incremental-maintenance bookkeeping: one fingerprint per compiled table.
CREATE TABLE IF NOT EXISTS kg_meta (
    src_table   TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    compiled_at TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;
"#;

impl<'a> SqliteKnowledgeStore<'a> {
    pub fn new(db: &'a StemmaDb) -> Result<Self> {
        let conn = db.conn();
        // kg_edges gained a props column; the KG is derived state, so a shape
        // mismatch is handled by dropping and recompiling.
        let has_props: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('kg_edges') WHERE name = 'props'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let exists: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'kg_edges'",
            [],
            |r| r.get(0),
        )?;
        if exists > 0 && has_props == 0 {
            conn.execute_batch(
                "DROP TABLE kg_edges; DROP TABLE IF EXISTS kg_nodes; DROP TABLE IF EXISTS kg_meta;",
            )?;
        }
        conn.execute_batch(KG_SCHEMA)?;
        Ok(Self { db })
    }

    fn node_id(&self, key: &str) -> Result<i64> {
        Ok(self
            .db
            .conn()
            .query_row("SELECT id FROM kg_nodes WHERE key = ?1", [key], |r| {
                r.get(0)
            })?)
    }
}

impl KnowledgeStore for SqliteKnowledgeStore<'_> {
    fn upsert_node(&self, node: &Node) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO kg_nodes (key, kind, label, props) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET kind = ?2, label = ?3, props = ?4",
            stemmadb::rusqlite::params![node.key, node.kind, node.label, node.props],
        )?;
        Ok(())
    }

    fn upsert_edge(&self, edge: &Edge) -> Result<()> {
        let src = self.node_id(&edge.src_key)?;
        let dst = self.node_id(&edge.dst_key)?;
        self.db.conn().execute(
            "INSERT INTO kg_edges (src, dst, kind, label, props) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(src, dst, kind) DO UPDATE SET label = ?4, props = ?5",
            stemmadb::rusqlite::params![src, dst, edge.kind, edge.label, edge.props],
        )?;
        Ok(())
    }

    fn remove_by_key_prefixes(&self, prefixes: &[String]) -> Result<()> {
        let conn = self.db.conn();
        for p in prefixes {
            let like = format!("{}%", p.replace('%', ""));
            conn.execute(
                "DELETE FROM kg_edges WHERE src IN (SELECT id FROM kg_nodes WHERE key LIKE ?1)
                 OR dst IN (SELECT id FROM kg_nodes WHERE key LIKE ?1)",
                [&like],
            )?;
            conn.execute("DELETE FROM kg_nodes WHERE key LIKE ?1", [&like])?;
        }
        Ok(())
    }

    fn stats(&self) -> Result<KgStats> {
        let conn = self.db.conn();
        let nodes: i64 = conn.query_row("SELECT count(*) FROM kg_nodes", [], |r| r.get(0))?;
        let edges: i64 = conn.query_row("SELECT count(*) FROM kg_edges", [], |r| r.get(0))?;
        Ok(KgStats {
            nodes: nodes as usize,
            edges: edges as usize,
            recompiled_tables: 0,
        })
    }
}

// ---- compilation parameters ----

/// Frequent-value nodes per column, at most this many.
const TOP_VALUES_PER_COLUMN: usize = 10;
/// A value earns a node when it recurs at least this often.
const MIN_VALUE_COUNT: i64 = 2;
/// Characteristic-term nodes per table (from document columns).
const TOP_TERMS_PER_TABLE: usize = 24;
/// Terms shorter than this are noise.
const MIN_TERM_LEN: usize = 4;
/// Containment ratio at which an undeclared join is proposed.
const INFERRED_FK_MIN_CONTAINMENT: f64 = 0.95;
/// Skip inclusion mining on columns with more distinct values than this.
const INFERRED_FK_MAX_DISTINCT: i64 = 500_000;
/// Term pairs kept per table, ranked by co-occurrence strength.
const TOP_COOCCUR_PAIRS: usize = 40;
/// Minimum conditional co-occurrence (pair docs / min(term docs)).
const MIN_COOCCUR_RATIO: f64 = 0.25;

/// Content fingerprint of one user table: cheap to compute, catches inserts,
/// deletes, and rowid churn. In-place updates that preserve count, max rowid
/// and rowid sum are missed — acceptable for derived state that `force`
/// rebuilds. O(n) per table, no text hashing.
fn fingerprint(db: &StemmaDb, table: &str) -> Result<String> {
    let (n, mx, sum): (i64, i64, i64) = db.conn().query_row(
        &format!(
            "SELECT count(*), coalesce(max(rowid),0), coalesce(sum(rowid),0) FROM {}.\"{table}\"",
            stemmadb::SRC_SCHEMA
        ),
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(format!("{n}:{mx}:{sum}"))
}

/// Compiles (or incrementally refreshes) the schema, discovered-relation and
/// profile layers. Only tables whose fingerprint changed are recompiled;
/// `force` rebuilds everything.
pub fn compile(db: &StemmaDb, force: bool) -> Result<KgStats> {
    let store = SqliteKnowledgeStore::new(db)?;
    let conn = db.conn();
    let tables = db.src_tables()?;

    // ---- incremental maintenance: which tables changed? ----
    let mut dirty: Vec<String> = Vec::new();
    for t in &tables {
        let fp = fingerprint(db, t)?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT fingerprint FROM kg_meta WHERE src_table = ?1",
                [t],
                |r| r.get(0),
            )
            .ok();
        if force || stored.as_deref() != Some(fp.as_str()) {
            dirty.push(t.clone());
        }
    }
    // Dropped tables leave stale nodes; sweep them.
    let known: Vec<String> = conn
        .prepare("SELECT src_table FROM kg_meta")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for gone in known.iter().filter(|k| !tables.contains(k)) {
        store.remove_by_key_prefixes(&[
            format!("table:{gone}"),
            format!("column:{gone}."),
            format!("value:{gone}."),
            format!("term:{gone}:"),
        ])?;
        conn.execute("DELETE FROM kg_meta WHERE src_table = ?1", [gone])?;
    }
    if dirty.is_empty() {
        let mut s = store.stats()?;
        s.recompiled_tables = 0;
        return Ok(s);
    }
    tracing::info!(dirty = dirty.len(), total = tables.len(), "kg recompile");

    // Scope the rebuild to dirty tables only.
    for t in &dirty {
        store.remove_by_key_prefixes(&[
            format!("table:{t}"),
            format!("column:{t}."),
            format!("value:{t}."),
            format!("term:{t}:"),
        ])?;
    }

    compile_schema_layer(db, &store, &dirty)?;
    compile_value_profile(db, &store, &dirty)?;
    compile_term_profile(db, &store, &dirty)?;
    // Cross-table passes re-run globally whenever anything changed: removing
    // a dirty table's nodes also removed clean tables' edges into it, and
    // containment is a global property anyway.
    compile_declared_fks(db, &store, &tables)?;
    compile_inferred_joins(db, &store, &tables)?;

    for t in &dirty {
        let fp = fingerprint(db, t)?;
        conn.execute(
            "INSERT INTO kg_meta (src_table, fingerprint, compiled_at)
             VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(src_table) DO UPDATE SET fingerprint = ?2, compiled_at = datetime('now')",
            stemmadb::rusqlite::params![t, fp],
        )?;
    }

    let mut s = store.stats()?;
    s.recompiled_tables = dirty.len();
    Ok(s)
}

fn compile_schema_layer(
    db: &StemmaDb,
    store: &SqliteKnowledgeStore,
    tables: &[String],
) -> Result<()> {
    let conn = db.conn();
    for t in tables {
        let rows: i64 = conn
            .query_row(
                &format!(
                    "SELECT coalesce(max(rowid),0) FROM {}.\"{t}\"",
                    stemmadb::SRC_SCHEMA
                ),
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        store.upsert_node(&Node {
            key: format!("table:{t}"),
            kind: KIND_TABLE.into(),
            label: t.clone(),
            props: format!("{{\"rows\":{rows}}}"),
        })?;
        let mut stmt = conn.prepare(&format!(
            "SELECT name, type FROM pragma_table_info(?1, '{}')",
            stemmadb::SRC_SCHEMA
        ))?;
        let cols: Vec<(String, String)> = stmt
            .query_map([t], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (name, ty) in &cols {
            store.upsert_node(&Node {
                key: format!("column:{t}.{name}"),
                kind: KIND_COLUMN.into(),
                label: name.clone(),
                props: format!("{{\"type\":{ty:?},\"table\":{t:?}}}"),
            })?;
            store.upsert_edge(&Edge {
                src_key: format!("table:{t}"),
                dst_key: format!("column:{t}.{name}"),
                kind: "has_column".into(),
                label: String::new(),
                props: "{\"method\":\"declared\",\"confidence\":1.0}".into(),
            })?;
        }
    }
    Ok(())
}

/// Declared foreign keys, upserted for every table pair — runs globally on
/// each compile so edges into recompiled tables are restored.
fn compile_declared_fks(
    db: &StemmaDb,
    store: &SqliteKnowledgeStore,
    tables: &[String],
) -> Result<()> {
    let conn = db.conn();
    for t in tables {
        let mut stmt = conn.prepare(&format!(
            "SELECT \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1, '{}')",
            stemmadb::SRC_SCHEMA
        ))?;
        let fks: Vec<(String, String, Option<String>)> = stmt
            .query_map([t], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (to_table, from_col, to_col) in fks {
            if !tables.contains(&to_table) {
                continue;
            }
            store.upsert_edge(&Edge {
                src_key: format!("table:{t}"),
                dst_key: format!("table:{to_table}"),
                kind: "fk".into(),
                label: format!("{from_col} → {}", to_col.unwrap_or_else(|| "id".into())),
                props: "{\"method\":\"declared\",\"confidence\":1.0}".into(),
            })?;
        }
    }
    Ok(())
}

/// Frequent short values per column. Identifier-like columns (all-distinct)
/// earn nothing because nothing recurs.
fn compile_value_profile(
    db: &StemmaDb,
    store: &SqliteKnowledgeStore,
    tables: &[String],
) -> Result<()> {
    let conn = db.conn();
    let placeholders = vec!["?"; tables.len()].join(",");
    let sql = format!(
        "SELECT src_table, src_column, value, n FROM (
            SELECT src_table, src_column, value, count(*) AS n,
                   row_number() OVER (
                       PARTITION BY src_table, src_column ORDER BY count(*) DESC
                   ) AS rk
            FROM lex_values WHERE is_doc = 0 AND src_table IN ({placeholders})
            GROUP BY src_table, src_column, value_norm
         ) WHERE n >= ?{} AND rk <= ?{}",
        tables.len() + 1,
        tables.len() + 2,
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<Box<dyn stemmadb::rusqlite::types::ToSql>> = tables
        .iter()
        .map(|t| Box::new(t.clone()) as Box<dyn stemmadb::rusqlite::types::ToSql>)
        .collect();
    params.push(Box::new(MIN_VALUE_COUNT));
    params.push(Box::new(TOP_VALUES_PER_COLUMN as i64));
    let rows: Vec<(String, String, String, i64)> = stmt
        .query_map(
            stemmadb::rusqlite::params_from_iter(params.iter().map(|b| b.as_ref())),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?
        .collect::<std::result::Result<_, _>>()?;
    for (t, c, v, n) in rows {
        let key = format!("value:{t}.{c}:{}", v.to_lowercase());
        store.upsert_node(&Node {
            key: key.clone(),
            kind: KIND_VALUE.into(),
            label: v,
            props: format!("{{\"count\":{n}}}"),
        })?;
        store.upsert_edge(&Edge {
            src_key: format!("column:{t}.{c}"),
            dst_key: key,
            kind: "frequent_value".into(),
            label: format!("×{n}"),
            props: format!("{{\"method\":\"profiled\",\"count\":{n}}}"),
        })?;
    }
    Ok(())
}

/// Characteristic terms of document corpora plus term co-occurrence edges.
fn compile_term_profile(
    db: &StemmaDb,
    store: &SqliteKnowledgeStore,
    tables: &[String],
) -> Result<()> {
    let conn = db.conn();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS lex_vocab USING fts5vocab('lex_fts', 'row');",
    )?;
    for t in tables {
        let doc_values: i64 = conn.query_row(
            "SELECT count(*) FROM lex_values WHERE is_doc = 1 AND src_table = ?1",
            [t],
            |r| r.get(0),
        )?;
        if doc_values == 0 {
            continue;
        }
        // Corpus-wide document frequency (fts5vocab cannot split by source
        // table; single-doc-table stores — the common case — are exact).
        let mut stmt = conn.prepare(
            "SELECT term, doc FROM lex_vocab
             WHERE length(term) >= ?1 AND term NOT IN (
                'that','this','with','from','have','been','were','will','which',
                'shall','must','such','other','than','their','there','these',
                'those','when','where','under','upon','into','each','also',
                'more','less','only','some','same','then','they','them')
             ORDER BY doc DESC LIMIT ?2",
        )?;
        let terms: Vec<(String, i64)> = stmt
            .query_map(
                stemmadb::rusqlite::params![MIN_TERM_LEN as i64, TOP_TERMS_PER_TABLE as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .collect::<std::result::Result<_, _>>()?;
        for (term, doc) in &terms {
            let key = format!("term:{t}:{term}");
            store.upsert_node(&Node {
                key: key.clone(),
                kind: KIND_TERM.into(),
                label: term.clone(),
                props: format!("{{\"docs\":{doc}}}"),
            })?;
            store.upsert_edge(&Edge {
                src_key: format!("table:{t}"),
                dst_key: key,
                kind: "term".into(),
                label: format!("{doc} docs"),
                props: format!("{{\"method\":\"profiled\",\"docs\":{doc}}}"),
            })?;
        }
        // Co-occurrence: how often two characteristic terms share a document.
        let mut pairs: Vec<(String, String, i64, f64)> = Vec::new();
        for i in 0..terms.len() {
            for j in (i + 1)..terms.len() {
                let (a, da) = &terms[i];
                let (b, db_) = &terms[j];
                let both: i64 = conn.query_row(
                    "SELECT count(*) FROM lex_fts WHERE lex_fts MATCH ?1",
                    [format!("\"{a}\" AND \"{b}\"")],
                    |r| r.get(0),
                )?;
                let ratio = both as f64 / (*da.min(db_)) as f64;
                if ratio >= MIN_COOCCUR_RATIO && both > 0 {
                    pairs.push((a.clone(), b.clone(), both, ratio));
                }
            }
        }
        pairs.sort_by(|x, y| y.2.cmp(&x.2));
        for (a, b, both, ratio) in pairs.into_iter().take(TOP_COOCCUR_PAIRS) {
            store.upsert_edge(&Edge {
                src_key: format!("term:{t}:{a}"),
                dst_key: format!("term:{t}:{b}"),
                kind: "cooccurs".into(),
                label: format!("{both} docs"),
                props: format!(
                    "{{\"method\":\"profiled\",\"confidence\":{ratio:.2},\"docs\":{both}}}"
                ),
            })?;
        }
    }
    Ok(())
}

/// Inclusion-dependency mining: propose undeclared joins where an integer
/// column's distinct values are (almost) contained in another table's
/// single-column integer primary key.
fn compile_inferred_joins(
    db: &StemmaDb,
    store: &SqliteKnowledgeStore,
    tables: &[String],
) -> Result<()> {
    let conn = db.conn();

    // Candidate key columns: single-column INTEGER PKs.
    let mut keys: Vec<(String, String)> = Vec::new(); // (table, pk column)
    // Candidate referencing columns: INTEGER, not the table's own pk.
    let mut refs: Vec<(String, String)> = Vec::new();
    // Declared FK pairs to skip (already in the schema layer).
    let mut declared: Vec<(String, String)> = Vec::new(); // (from table.col, to table)
    for t in tables {
        let mut stmt = conn.prepare(&format!(
            "SELECT name, type, pk FROM pragma_table_info(?1, '{}')",
            stemmadb::SRC_SCHEMA
        ))?;
        let cols: Vec<(String, String, i64)> = stmt
            .query_map([t], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let pk_cols: Vec<_> = cols.iter().filter(|c| c.2 > 0).collect();
        for (name, ty, pk) in &cols {
            let is_int = ty.to_uppercase().contains("INT");
            if *pk == 1 && pk_cols.len() == 1 && is_int {
                keys.push((t.clone(), name.clone()));
            } else if *pk == 0 && is_int {
                refs.push((t.clone(), name.clone()));
            }
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT \"table\", \"from\" FROM pragma_foreign_key_list(?1, '{}')",
            stemmadb::SRC_SCHEMA
        ))?;
        let fks: Vec<(String, String)> = stmt
            .query_map([t], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (to_table, from_col) in fks {
            declared.push((format!("{t}.{from_col}"), to_table));
        }
    }

    for (rt, rc) in &refs {
        let distinct: i64 = conn.query_row(
            &format!(
                "SELECT count(DISTINCT \"{rc}\") FROM {}.\"{rt}\" WHERE \"{rc}\" IS NOT NULL",
                stemmadb::SRC_SCHEMA
            ),
            [],
            |r| r.get(0),
        )?;
        if distinct == 0 || distinct > INFERRED_FK_MAX_DISTINCT {
            continue;
        }
        for (kt, kc) in &keys {
            if kt == rt {
                continue;
            }
            if declared
                .iter()
                .any(|(f, to)| f == &format!("{rt}.{rc}") && to == kt)
            {
                continue;
            }
            let missing: i64 = conn.query_row(
                &format!(
                    "SELECT count(*) FROM (
                        SELECT DISTINCT \"{rc}\" AS v FROM {s}.\"{rt}\" WHERE \"{rc}\" IS NOT NULL
                        EXCEPT SELECT \"{kc}\" FROM {s}.\"{kt}\"
                     )",
                    s = stemmadb::SRC_SCHEMA
                ),
                [],
                |r| r.get(0),
            )?;
            let containment = 1.0 - missing as f64 / distinct as f64;
            if containment >= INFERRED_FK_MIN_CONTAINMENT {
                store.upsert_edge(&Edge {
                    src_key: format!("table:{rt}"),
                    dst_key: format!("table:{kt}"),
                    kind: "inferred_fk".into(),
                    label: format!("{rc} →? {kc}"),
                    props: format!(
                        "{{\"method\":\"inferred\",\"confidence\":{containment:.3},\
                          \"distinct\":{distinct}}}"
                    ),
                })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ingested_mini(tag: &str) -> StemmaDb {
        let dir = std::env::temp_dir().join(format!("stemma-kg-{}-{tag}", std::process::id()));
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
    fn compiles_schema_and_profile_layers() {
        let db = ingested_mini("layers");
        let stats = compile(&db, false).unwrap();
        assert!(stats.nodes > 6, "tables + columns + values, got {stats:?}");
        assert_eq!(stats.recompiled_tables, 6);
        let fk: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM kg_edges WHERE kind = 'fk'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fk, 6, "mini corpus declares six foreign keys");
        // Repeated values earn nodes: the quarter '2025Q3' appears twice.
        let q3: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM kg_nodes WHERE kind = 'value' AND label = '2025Q3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(q3, 1);
        // Every edge carries provenance.
        let bare: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM kg_edges WHERE props NOT LIKE '%method%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bare, 0, "edges without provenance");
    }

    #[test]
    fn incremental_recompile_skips_clean_tables() {
        let db = ingested_mini("incr");
        let a = compile(&db, false).unwrap();
        assert_eq!(a.recompiled_tables, 6);
        // Nothing changed: no work.
        let b = compile(&db, false).unwrap();
        assert_eq!(b.recompiled_tables, 0);
        assert_eq!(a.nodes, b.nodes);
        // Note: writes to the user DB can't happen through stemmadb (read-only
        // attach); simulate change by clearing one fingerprint.
        db.conn()
            .execute("DELETE FROM kg_meta WHERE src_table = 'offices'", [])
            .unwrap();
        let c = compile(&db, false).unwrap();
        assert_eq!(c.recompiled_tables, 1, "only the dirty table recompiles");
        assert_eq!(a.nodes, c.nodes, "recompile converges to the same graph");
        assert_eq!(a.edges, c.edges);
    }

    #[test]
    fn discovers_undeclared_joins() {
        // A schema with a real relationship but no declared FK.
        let dir = std::env::temp_dir().join(format!("stemma-kg-{}-infer", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let storef = dir.join("user.stemmadb");
        let _ = std::fs::remove_file(&user);
        let _ = std::fs::remove_file(&storef);
        {
            let c = stemmadb::rusqlite::Connection::open(&user).unwrap();
            c.execute_batch(
                "CREATE TABLE depts(id INTEGER PRIMARY KEY, name TEXT);
                 CREATE TABLE staff(id INTEGER PRIMARY KEY, name TEXT, dept INTEGER);
                 INSERT INTO depts VALUES (1,'legal'),(2,'ops');
                 INSERT INTO staff VALUES (1,'a',1),(2,'b',1),(3,'c',2);",
            )
            .unwrap();
        }
        let db = StemmaDb::open(&storef, &user).unwrap();
        stemma_ingest::build_lexical_index(&db, false).unwrap();
        compile(&db, false).unwrap();
        let (label, props): (String, String) = db
            .conn()
            .query_row(
                "SELECT label, props FROM kg_edges WHERE kind = 'inferred_fk'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("an inferred join edge");
        assert_eq!(label, "dept →? id");
        assert!(props.contains("\"method\":\"inferred\""));
        assert!(props.contains("\"confidence\":1.000"));
    }
}
