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

/// One fk/inferred_fk hop on a schema path between tables. The fk's own
/// orientation is `src_table.src_column → dst_table.dst_column`; `forward`
/// records whether the path traverses it in that direction, so a consumer
/// can rebuild the join either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathHop {
    pub src_table: String,
    pub src_column: String,
    pub dst_table: String,
    pub dst_column: String,
    /// True for `inferred_fk` edges (discovered, not declared).
    pub inferred: bool,
    /// True when the path walks the fk referencing → referenced.
    pub forward: bool,
}

/// The backend-agnostic knowledge store. Remaining query-side methods
/// (neighbors, subgraph extraction) join as the instance layer lands; keep
/// additions here, not on concrete backends.
pub trait KnowledgeStore {
    fn upsert_node(&self, node: &Node) -> Result<()>;
    fn upsert_edge(&self, edge: &Edge) -> Result<()>;
    /// Removes every node whose key starts with any of the prefixes, plus
    /// edges touching them. The unit of incremental recompilation.
    fn remove_by_key_prefixes(&self, prefixes: &[String]) -> Result<()>;
    fn stats(&self) -> Result<KgStats>;
    /// Simple paths between two tables over fk/inferred_fk edges, at most
    /// `max_hops` edges each, shortest first, at most `limit` paths. Edges
    /// are traversable in both directions; each hop keeps the fk's own
    /// orientation.
    fn table_paths(
        &self,
        from: &str,
        to: &str,
        max_hops: usize,
        limit: usize,
    ) -> Result<Vec<Vec<PathHop>>>;
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

    fn table_paths(
        &self,
        from: &str,
        to: &str,
        max_hops: usize,
        limit: usize,
    ) -> Result<Vec<Vec<PathHop>>> {
        if from == to || max_hops == 0 || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.db.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT ns.label, nd.label, e.label, e.kind FROM kg_edges e
             JOIN kg_nodes ns ON ns.id = e.src
             JOIN kg_nodes nd ON nd.id = e.dst
             WHERE e.kind IN ('fk', 'inferred_fk')
               AND ns.kind = 'table' AND nd.kind = 'table'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut hops: Vec<PathHop> = Vec::new();
        for row in rows {
            let (src_table, dst_table, label, kind) = row?;
            // Edge labels encode the column pair: "office_id → id" for
            // declared fks, "dept →? id" for inferred ones.
            let Some((from_col, to_col)) = label.split_once('→') else {
                continue;
            };
            hops.push(PathHop {
                src_table,
                src_column: from_col.trim().to_string(),
                dst_table,
                dst_column: to_col.trim_start_matches('?').trim().to_string(),
                inferred: kind == "inferred_fk",
                forward: true,
            });
        }

        // Depth-first enumeration of simple paths. Table graphs are tens of
        // nodes; `max_hops` and `limit` bound the walk, no index needed.
        fn walk(
            hops: &[PathHop],
            here: &str,
            to: &str,
            max_hops: usize,
            visited: &mut Vec<String>,
            path: &mut Vec<PathHop>,
            found: &mut Vec<Vec<PathHop>>,
        ) {
            if path.len() >= max_hops {
                return;
            }
            for h in hops {
                let (next, forward) = if h.src_table == here {
                    (h.dst_table.as_str(), true)
                } else if h.dst_table == here {
                    (h.src_table.as_str(), false)
                } else {
                    continue;
                };
                if visited.iter().any(|v| v == next) {
                    continue;
                }
                let mut hop = h.clone();
                hop.forward = forward;
                path.push(hop);
                if next == to {
                    found.push(path.clone());
                } else {
                    visited.push(next.to_string());
                    walk(hops, next, to, max_hops, visited, path, found);
                    visited.pop();
                }
                path.pop();
            }
        }
        let mut found = Vec::new();
        walk(
            &hops,
            from,
            to,
            max_hops,
            &mut vec![from.to_string()],
            &mut Vec::new(),
            &mut found,
        );
        found.sort_by_key(|p| p.len());
        found.truncate(limit);
        Ok(found)
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
/// A term must appear in at least this many documents to matter…
const MIN_TERM_DOCS: i64 = 5;
/// …and in at most this fraction of them: beyond that it is a corpus
/// stopword ("within", "shall") no matter how common. High DF is the
/// *least* distinctive signal in a single-domain corpus.
const MAX_TERM_DF_RATIO: f64 = 0.25;
/// Documents sampled for capitalized-phrase mining.
const PHRASE_SAMPLE_DOCS: usize = 1500;
/// A phrase must recur at least this often across the sample.
const MIN_PHRASE_COUNT: usize = 5;
/// Phrase-entity nodes kept per table.
const TOP_PHRASES_PER_TABLE: usize = 20;
/// Candidate pool fed into TextRank (top of the burstiness shortlist).
const PAGERANK_CANDIDATES: usize = 200;
/// Containment ratio at which an undeclared join is proposed.
const INFERRED_FK_MIN_CONTAINMENT: f64 = 0.95;
/// Skip inclusion mining on columns with more distinct values than this.
const INFERRED_FK_MAX_DISTINCT: i64 = 500_000;
/// Term pairs kept per table, ranked by co-occurrence strength.
const TOP_COOCCUR_PAIRS: usize = 40;
/// Minimum conditional co-occurrence (pair docs / min(term docs)).
const MIN_COOCCUR_RATIO: f64 = 0.25;
/// Term→column affinity: a mined term keeps edges to at most this many
/// (table, column)s whose *value* content it recurs in — the columns a
/// query using that term is probably talking about.
const TOP_AFFINITY_COLUMNS: usize = 4;
/// A term must appear in at least this many value cells of a column to earn
/// an affinity edge — the same recurrence-not-heuristics floor as
/// MIN_VALUE_COUNT: one co-occurrence is coincidence, two is a pattern.
const MIN_AFFINITY_MATCHES: i64 = 2;

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
    // The leading tag versions the COMPILER, not the data: bumping it
    // invalidates every stored fingerprint so algorithm upgrades recompile.
    // kg3: added the term→column affinity pass.
    Ok(format!("kg3:{n}:{mx}:{sum}"))
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
    // containment is a global property anyway. Term→column affinity is
    // cross-table in the same way — a clean table's terms keep edges into a
    // recompiled table's column nodes.
    compile_declared_fks(db, &store, &tables)?;
    compile_inferred_joins(db, &store, &tables)?;
    compile_term_column_affinity(db, &store, &tables)?;

    for t in &dirty {
        let fp = fingerprint(db, t)?;
        conn.execute(
            "INSERT INTO kg_meta (src_table, fingerprint, compiled_at)
             VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(src_table) DO UPDATE SET fingerprint = ?2, compiled_at = datetime('now')",
            stemmadb::rusqlite::params![t, fp],
        )?;
    }

    compute_centrality(db)?;

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
        // Candidate terms from corpus-wide statistics (fts5vocab cannot
        // split by source table; single-doc-table stores — the common case —
        // are exact). Two filters kill function words without any list:
        // a DF *ceiling* — beyond MAX_TERM_DF_RATIO of docs a term is a
        // corpus stopword ("within", "shall") no matter how common — and a
        // burstiness prior (occurrences-per-containing-doc × log coverage)
        // to shortlist candidates worth graphing.
        let df_ceiling = ((doc_values as f64) * MAX_TERM_DF_RATIO).ceil() as i64;
        let mut stmt = conn.prepare(
            "SELECT term, doc, cnt FROM lex_vocab
             WHERE length(term) >= ?1 AND doc >= ?2 AND doc <= ?3
               AND term NOT GLOB '*[0-9]*'
             ORDER BY doc DESC LIMIT 4000",
        )?;
        let mut candidates: Vec<(String, i64, i64)> = stmt
            .query_map(
                stemmadb::rusqlite::params![MIN_TERM_LEN as i64, MIN_TERM_DOCS, df_ceiling],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect::<std::result::Result<_, _>>()?;
        let burstiness = |doc: i64, cnt: i64| (cnt as f64 / doc as f64) * (1.0 + (doc as f64).ln());
        candidates.sort_by(|a, b| burstiness(b.1, b.2).total_cmp(&burstiness(a.1, a.2)));
        candidates.truncate(PAGERANK_CANDIDATES);

        // One pass over a document sample powers everything downstream:
        // the term co-occurrence graph, TextRank, and phrase mining.
        let mut stmt = conn.prepare(
            "SELECT value FROM lex_values WHERE is_doc = 1 AND src_table = ?1
             ORDER BY id LIMIT ?2",
        )?;
        let docs: Vec<String> = stmt
            .query_map(
                stemmadb::rusqlite::params![t, PHRASE_SAMPLE_DOCS as i64],
                |r| r.get(0),
            )?
            .collect::<std::result::Result<_, _>>()?;

        // Per-document presence of each candidate term.
        use std::collections::{HashMap, HashSet};
        let index_of: HashMap<&str, usize> = candidates
            .iter()
            .enumerate()
            .map(|(i, (term, _, _))| (term.as_str(), i))
            .collect();
        let n = candidates.len();
        let mut sample_df = vec![0u32; n];
        let mut cooccur: HashMap<(usize, usize), u32> = HashMap::new();
        for doc in &docs {
            let mut present: HashSet<usize> = HashSet::new();
            for w in doc.split(|c: char| !c.is_alphanumeric()) {
                if w.len() >= MIN_TERM_LEN {
                    if let Some(&i) = index_of.get(w.to_lowercase().as_str()) {
                        present.insert(i);
                    }
                }
            }
            let mut ids: Vec<usize> = present.into_iter().collect();
            ids.sort_unstable();
            for (a, &i) in ids.iter().enumerate() {
                sample_df[i] += 1;
                for &j in &ids[a + 1..] {
                    *cooccur.entry((i, j)).or_default() += 1;
                }
            }
        }

        // TextRank: weighted PageRank over the co-occurrence graph. A term
        // matters when it co-occurs with other terms that matter — the
        // centrality signal frequency ranking cannot see.
        let rank = pagerank(n, &cooccur);
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| rank[b].total_cmp(&rank[a]));
        let kept: Vec<usize> = order
            .into_iter()
            .filter(|&i| sample_df[i] > 0)
            .take(TOP_TERMS_PER_TABLE)
            .collect();

        for &i in &kept {
            let (term, doc, _) = &candidates[i];
            let key = format!("term:{t}:{term}");
            store.upsert_node(&Node {
                key: key.clone(),
                kind: KIND_TERM.into(),
                label: term.clone(),
                props: format!("{{\"docs\":{doc},\"textrank\":{:.4}}}", rank[i]),
            })?;
            store.upsert_edge(&Edge {
                src_key: format!("table:{t}"),
                dst_key: key,
                kind: "term".into(),
                label: format!("{doc} docs"),
                props: format!("{{\"method\":\"textrank\",\"docs\":{doc}}}"),
            })?;
        }

        // Co-occurrence edges among the kept terms, from the same sample.
        let kept_set: HashSet<usize> = kept.iter().copied().collect();
        let mut pairs: Vec<(usize, usize, u32)> = cooccur
            .iter()
            .filter(|((a, b), _)| kept_set.contains(a) && kept_set.contains(b))
            .map(|(&(a, b), &c)| (a, b, c))
            .collect();
        pairs.sort_by(|x, y| y.2.cmp(&x.2));
        let mut kept_pairs = 0usize;
        for (a, b, both) in pairs {
            let ratio = both as f64 / sample_df[a].min(sample_df[b]).max(1) as f64;
            if ratio < MIN_COOCCUR_RATIO {
                continue;
            }
            store.upsert_edge(&Edge {
                src_key: format!("term:{t}:{}", candidates[a].0),
                dst_key: format!("term:{t}:{}", candidates[b].0),
                kind: "cooccurs".into(),
                label: format!("{both} docs"),
                props: format!(
                    "{{\"method\":\"profiled\",\"confidence\":{ratio:.2},\"docs\":{both}}}"
                ),
            })?;
            kept_pairs += 1;
            if kept_pairs >= TOP_COOCCUR_PAIRS {
                break;
            }
        }

        compile_phrase_entities(store, t, &docs)?;
    }
    Ok(())
}

/// Weighted PageRank by power iteration; treats edges as undirected
/// (co-occurrence is symmetric). Damping 0.85, fixed iteration budget —
/// convergence tolerance is irrelevant at these sizes.
fn pagerank(n: usize, edges: &std::collections::HashMap<(usize, usize), u32>) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    const DAMPING: f64 = 0.85;
    const ITERS: usize = 40;
    let mut weight_sum = vec![0f64; n];
    for (&(a, b), &w) in edges {
        weight_sum[a] += w as f64;
        weight_sum[b] += w as f64;
    }
    let mut rank = vec![1.0 / n as f64; n];
    let mut next = vec![0f64; n];
    for _ in 0..ITERS {
        next.fill((1.0 - DAMPING) / n as f64);
        let mut dangling = 0.0;
        for i in 0..n {
            if weight_sum[i] == 0.0 {
                dangling += rank[i];
            }
        }
        for i in 0..n {
            next[i] += DAMPING * dangling / n as f64;
        }
        for (&(a, b), &w) in edges {
            let w = w as f64;
            next[b] += DAMPING * rank[a] * w / weight_sum[a];
            next[a] += DAMPING * rank[b] * w / weight_sum[b];
        }
        std::mem::swap(&mut rank, &mut next);
    }
    rank
}

/// PageRank over the compiled graph itself, stored as `centrality` on every
/// node — the UI reads it to size marks by importance.
fn compute_centrality(db: &StemmaDb) -> Result<()> {
    let conn = db.conn();
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM kg_nodes ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    if ids.is_empty() {
        return Ok(());
    }
    let index: std::collections::HashMap<i64, usize> =
        ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let mut edges: std::collections::HashMap<(usize, usize), u32> =
        std::collections::HashMap::new();
    let rows: Vec<(i64, i64)> = conn
        .prepare("SELECT src, dst FROM kg_edges")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    for (s, d) in rows {
        if let (Some(&a), Some(&b)) = (index.get(&s), index.get(&d)) {
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let rank = pagerank(ids.len(), &edges);
    for (i, &id) in ids.iter().enumerate() {
        conn.execute(
            "UPDATE kg_nodes SET props = json_set(props, '$.centrality', ?1) WHERE id = ?2",
            stemmadb::rusqlite::params![format!("{:.5}", rank[i]).parse::<f64>().unwrap_or(0.0), id],
        )?;
    }
    Ok(())
}

/// Named entities in prose are multi-word and capitalized ("California
/// Coastal Commission", "Fish and Game Code"): mine recurring capitalized
/// phrases from a document sample. Deterministic, LLM-free; the LLM-based
/// extraction pass of the instance layer supersedes, not replaces, this.
fn compile_phrase_entities(
    store: &SqliteKnowledgeStore,
    table: &str,
    docs: &[String],
) -> Result<()> {
    use std::collections::HashMap;
    const CONNECTORS: &[&str] = &["of", "and", "the", "for"];

    let mut counts: HashMap<String, usize> = HashMap::new();
    for doc in docs {
        let words: Vec<&str> = doc
            .split(|c: char| !(c.is_alphanumeric() || c == '\''))
            .filter(|w| !w.is_empty())
            .collect();
        let is_cap = |w: &str| {
            w.chars().next().is_some_and(|c| c.is_uppercase())
                && w.len() >= 2
                && w.chars().all(|c| c.is_alphabetic() || c == '\'')
        };
        let mut i = 0;
        while i < words.len() {
            if !is_cap(words[i]) {
                i += 1;
                continue;
            }
            // Extend through capitalized words with lowercase connectors
            // allowed between them; the phrase must end capitalized.
            let mut j = i + 1;
            let mut last_cap = i;
            while j < words.len() && j - i < 6 {
                if is_cap(words[j]) {
                    last_cap = j;
                    j += 1;
                } else if CONNECTORS.contains(&words[j]) && last_cap == j - 1 {
                    j += 1;
                } else {
                    break;
                }
            }
            if last_cap > i {
                let phrase = words[i..=last_cap].join(" ");
                if phrase.len() <= 60 {
                    *counts.entry(phrase).or_default() += 1;
                }
            }
            i = last_cap.max(i) + 1;
        }
    }

    // Drop phrases that are strict prefixes of a more complete phrase with
    // comparable support ("California Coastal" vs "California Coastal
    // Commission") — keep the most informative form.
    let mut phrases: Vec<(String, usize)> = counts
        .iter()
        .filter(|(_, &n)| n >= MIN_PHRASE_COUNT)
        .map(|(p, &n)| (p.clone(), n))
        .collect();
    phrases.retain(|(p, n)| {
        !counts.iter().any(|(longer, &ln)| {
            longer != p && longer.starts_with(p.as_str()) && ln * 2 >= *n
        })
    });
    phrases.sort_by(|a, b| b.1.cmp(&a.1));

    for (phrase, n) in phrases.into_iter().take(TOP_PHRASES_PER_TABLE) {
        let key = format!("phrase:{table}:{}", phrase.to_lowercase());
        store.upsert_node(&Node {
            key: key.clone(),
            kind: KIND_TERM.into(),
            label: phrase,
            props: format!("{{\"count\":{n},\"phrase\":true,\"sampled\":{}}}", docs.len()),
        })?;
        store.upsert_edge(&Edge {
            src_key: format!("table:{table}"),
            dst_key: key,
            kind: "term".into(),
            label: format!("×{n}"),
            props: format!("{{\"method\":\"profiled\",\"count\":{n}}}"),
        })?;
    }
    Ok(())
}

/// Term→column affinity: for every mined term (TextRank words and mined
/// phrases alike — both are `kind = 'term'`), which columns' *value* content
/// the term recurs in, measured with one FTS probe per term grouped by
/// (src_table, src_column). Kept: the top TOP_AFFINITY_COLUMNS columns with
/// at least MIN_AFFINITY_MATCHES matching cells, as `col_affinity` edges
/// from the term node to the existing `column:{table}.{column}` nodes.
///
/// Only value cells (`is_doc = 0`) in columns whose `lex_columns.kind` is
/// `text` count. A term trivially "co-occurs" with the document column it was
/// mined from, and the consumer of these edges — resolution's
/// context-coherence stage — disambiguates *value* interpretations; letting
/// document columns fill the slots would spend the budget on edges nothing
/// can use. The column-typology restriction is the same argument one level
/// up: term affinity into a timestamp, key or code column is meaningless —
/// no mention of the term ever resolves to those values — so only `text`
/// columns may hold affinity.
///
/// Runs globally (all served tables) whenever any table recompiled, like the
/// other cross-table passes: recompiling table u removes u's column nodes
/// and with them every clean table's affinity edges into u.
fn compile_term_column_affinity(
    db: &StemmaDb,
    store: &SqliteKnowledgeStore,
    tables: &[String],
) -> Result<()> {
    let conn = db.conn();
    let terms: Vec<(String, String)> = conn
        .prepare("SELECT key, label FROM kg_nodes WHERE kind = 'term' ORDER BY key")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    if terms.is_empty() {
        return Ok(());
    }
    let mut probe = conn.prepare_cached(
        "SELECT v.src_table, v.src_column, count(*) AS n
         FROM lex_fts f JOIN lex_values v ON v.id = f.rowid
         JOIN lex_columns lc ON lc.src_table = v.src_table
                            AND lc.src_column = v.src_column
         WHERE lex_fts MATCH ?1 AND v.is_doc = 0 AND lc.kind = 'text'
         GROUP BY v.src_table, v.src_column
         HAVING n >= ?2
         ORDER BY n DESC, v.src_table, v.src_column
         LIMIT ?3",
    )?;
    for (key, label) in &terms {
        // Quote as an FTS5 string: phrase labels contain spaces, and no
        // label should be parsed as FTS syntax.
        let fts_query = format!("\"{}\"", label.replace('"', "\"\""));
        let cols: Vec<(String, String, i64)> = probe
            .query_map(
                stemmadb::rusqlite::params![
                    fts_query,
                    MIN_AFFINITY_MATCHES,
                    TOP_AFFINITY_COLUMNS as i64
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect::<std::result::Result<_, _>>()?;
        for (t, c, n) in cols {
            // The lexical index can name tables no longer served; skip
            // rather than dangle an edge at a missing column node.
            if !tables.contains(&t) {
                continue;
            }
            store.upsert_edge(&Edge {
                src_key: key.clone(),
                dst_key: format!("column:{t}.{c}"),
                kind: "col_affinity".into(),
                label: format!("×{n}"),
                props: format!("{{\"method\":\"profiled\",\"count\":{n}}}"),
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
    fn textrank_terms_exclude_corpus_stopwords() {
        let dir = std::env::temp_dir().join(format!("stemma-kg-{}-terms", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let storef = dir.join("user.stemmadb");
        let _ = std::fs::remove_file(&user);
        let _ = std::fs::remove_file(&storef);
        {
            let c = stemmadb::rusqlite::Connection::open(&user).unwrap();
            c.execute("CREATE TABLE docs(id INTEGER PRIMARY KEY, body TEXT)", [])
                .unwrap();
            // "whereof" rides in EVERY doc (a corpus stopword); the topical
            // vocabulary rotates per theme and co-occurs within themes.
            let themes = [
                "coastal permit commission coastal permit commission shoreline",
                "insurance filing commissioner insurance filing premium",
                "housing development council housing development zoning",
                "fisheries license quota fisheries harvest quota vessel",
                "pesticide registration applicator pesticide residue tolerance",
                "vehicle emission inspection vehicle smog certificate",
            ];
            for i in 0..30 {
                let theme = themes[i % 6];
                let body = format!(
                    "whereof the {theme} whereof provisions apply {theme} whereof. {}",
                    "Additional procedural language follows to reach document length                      comfortably past the classification threshold for documents."
                );
                c.execute("INSERT INTO docs(body) VALUES (?1)", [body]).unwrap();
            }
        }
        let db = StemmaDb::open(&storef, &user).unwrap();
        stemma_ingest::build_lexical_index(&db, false).unwrap();
        compile(&db, false).unwrap();
        let labels: Vec<String> = db
            .conn()
            .prepare("SELECT label FROM kg_nodes WHERE kind = 'term'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        assert!(
            !labels.iter().any(|l| l == "whereof"),
            "df-ceiling must kill corpus stopwords, got {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "coastal" || l == "commission"),
            "topical terms must survive, got {labels:?}"
        );
        // every term node carries its TextRank score
        let unranked: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM kg_nodes WHERE kind = 'term'                  AND props NOT LIKE '%textrank%' AND props NOT LIKE '%phrase%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unranked, 0);
        // and every node got a centrality from the graph-wide PageRank
        let uncentral: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM kg_nodes WHERE json_extract(props, '$.centrality') IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(uncentral, 0);
    }

    #[test]
    fn term_column_affinity_points_at_value_columns() {
        let dir = std::env::temp_dir().join(format!("stemma-kg-{}-affinity", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.db");
        let storef = dir.join("user.stemmadb");
        let _ = std::fs::remove_file(&user);
        let _ = std::fs::remove_file(&storef);
        {
            let c = stemmadb::rusqlite::Connection::open(&user).unwrap();
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
            c.execute_batch(
                "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
                 CREATE TABLE clients (id INTEGER PRIMARY KEY, company TEXT NOT NULL);
                 CREATE TABLE vendors (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            )
            .unwrap();
            for i in 0..30 {
                let theme = themes[i % 6];
                c.execute(
                    "INSERT INTO notes (body) VALUES (?1)",
                    [format!(
                        "whereof the {theme} whereof provisions apply {theme} whereof. {pad}"
                    )],
                )
                .unwrap();
            }
            c.execute_batch(
                "INSERT INTO clients (company) VALUES
                     ('Atlas Freight'), ('Beacon Mills'), ('Coral Imports');
                 INSERT INTO vendors (name) VALUES
                     ('Atlas Freight'), ('Cargo Line'), ('Cargo Express'), ('Delta Supply');",
            )
            .unwrap();
        }
        let db = StemmaDb::open(&storef, &user).unwrap();
        stemma_ingest::build_lexical_index(&db, false).unwrap();
        compile(&db, false).unwrap();

        let affinity = |term: &str| -> Vec<(String, String)> {
            db.conn()
                .prepare(
                    "SELECT cn.key, e.props FROM kg_edges e
                     JOIN kg_nodes tn ON tn.id = e.src
                     JOIN kg_nodes cn ON cn.id = e.dst
                     WHERE e.kind = 'col_affinity' AND tn.label = ?1",
                )
                .unwrap()
                .query_map([term], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(|x| x.unwrap())
                .collect()
        };

        // "cargo" recurs in vendors.name values (Cargo Line, Cargo Express)
        // and nowhere else that clears the floor — one affinity edge, with
        // provenance, pointing at the existing column node.
        let cargo = affinity("cargo");
        assert_eq!(
            cargo.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["column:vendors.name"],
            "got {cargo:?}"
        );
        assert!(cargo[0].1.contains("\"method\":\"profiled\""));
        assert!(cargo[0].1.contains("\"count\":2"));
        // A term with no recurring value matches earns nothing: document
        // cells are excluded, so "diesel" (docs only) has no affinity.
        assert!(affinity("diesel").is_empty());
        // The budget holds for every term.
        let max_edges: i64 = db
            .conn()
            .query_row(
                "SELECT coalesce(max(n), 0) FROM (
                    SELECT count(*) AS n FROM kg_edges
                    WHERE kind = 'col_affinity' GROUP BY src)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(max_edges as usize <= TOP_AFFINITY_COLUMNS);

        // Cross-table restoration: recompiling the column's table removes
        // its column nodes (and the affinity edges into them); the global
        // pass must put the edges back.
        db.conn()
            .execute("DELETE FROM kg_meta WHERE src_table = 'vendors'", [])
            .unwrap();
        let s = compile(&db, false).unwrap();
        assert_eq!(s.recompiled_tables, 1);
        assert_eq!(
            affinity("cargo")
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            vec!["column:vendors.name"],
            "affinity edges survive an incremental recompile of the column's table"
        );
    }

    #[test]
    fn bounded_paths_over_fk_edges() {
        let db = ingested_mini("paths");
        compile(&db, false).unwrap();
        let store = SqliteKnowledgeStore::new(&db).unwrap();
        let paths = store.table_paths("people", "teams", 2, 8).unwrap();
        // Shortest first: the direct teams.lead_id → people.id fk, traversed
        // in reverse (people is the referenced side).
        let direct = &paths[0];
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].src_table, "teams");
        assert_eq!(direct[0].src_column, "lead_id");
        assert_eq!(direct[0].dst_column, "id");
        assert!(!direct[0].forward);
        assert!(!direct[0].inferred);
        // Membership is the two-hop alternative: people ← team_members → teams.
        assert!(paths
            .iter()
            .any(|p| p.len() == 2 && p.iter().all(|h| h.src_table == "team_members")));
        // The hop bound and the trivial cases hold.
        assert!(store.table_paths("people", "teams", 0, 8).unwrap().is_empty());
        assert!(store.table_paths("people", "people", 2, 8).unwrap().is_empty());
        assert_eq!(store.table_paths("people", "teams", 2, 1).unwrap().len(), 1);
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
