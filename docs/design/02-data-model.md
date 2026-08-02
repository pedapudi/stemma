# Data model

Every byte stemma persists lives in SQLite, in two files with a hard
ownership boundary. This document gives the full physical schema of both
sides of that boundary, the migration discipline that keeps the store
upgradeable, the shape of the lexical index, the evidence and trace model
carried over the wire, and the history tables.

Sources: [`crates/stemmadb/src/lib.rs`](../../crates/stemmadb/src/lib.rs),
[`crates/stemma-ingest/src/lib.rs`](../../crates/stemma-ingest/src/lib.rs),
[`crates/stemma-kg/src/lib.rs`](../../crates/stemma-kg/src/lib.rs),
[`proto/stemma/v1/resolve.proto`](../../proto/stemma/v1/resolve.proto).

## The two files

| | user database | `.stemmadb` store |
|---|---|---|
| example path | `eval/legal/data/legal.db` | `eval/legal/data/legal.stemmadb` |
| SQLite schema name | `src` (attached) | `main` (opened) |
| owner | the user | stemmadb |
| access | `file:…?mode=ro` — read-only, always | read-write |
| contents | arbitrary user tables | every derived artifact |
| disposable | never | always |

`StemmaDb::open(store_path, user_db_path)` opens the store as the main
database of the connection and then attaches the user database:

```rust
let conn = Connection::open(store_path)?;
conn.pragma_update(None, "journal_mode", "wal")?;
conn.pragma_update(None, "foreign_keys", "on")?;
let uri = format!("file:{}?mode=ro", user_db_path…);
conn.execute("ATTACH DATABASE ?1 AS src", params![uri])?;
```

Three consequences follow from this ordering and are relied on throughout:

1. **Cross-file SQL is ordinary SQL.** `INSERT INTO lex_values … SELECT …
   FROM src."regulations"` is a single statement in one connection — no
   serialization, no second process, no copy. The ingest pass, the knowledge
   compiler and every verification probe are written as plain joins across
   the boundary.
2. **The read-only attach is enforced by SQLite**, not by convention. The
   `mode=ro` URI makes writes to `src` fail at the VFS layer; a bug in
   stemma cannot corrupt user data. `stemmadb::tests::attaches_user_db_read_only`
   asserts the failure.
3. **WAL on the store** lets the resolution server hold the store read-write
   while the console, the Python `StoreBrowser`, and `sqlite3` open it
   `mode=ro` concurrently.

`StemmaDb::open_in_memory()` gives a throwaway pair (`:memory:` store with a
`:memory:` `src`) for tests; it is the only way to get a *writable* `src`,
and exists so tests can construct fixtures.

### Extension loading

`register_extensions()` calls `sqlite3_auto_extension(sqlite3_vec_init)`
once per process behind a `std::sync::Once`, so every connection opened
afterwards — including ones stemma did not open — has `vec0` and the
`vec_*()` scalar functions. sqlite-vec is compiled from
[`third_party/sqlite_vec/sqlite-vec.c`](../../third_party/sqlite_vec) and
statically linked; there is no runtime `.so` loading and no patched SQLite.
`StemmaDb::vec_version()` (`SELECT vec_version()`) is the liveness check,
and `has_fts5()` queries `pragma_module_list` for the FTS5 module.

## Store schema

The store's fixed bookkeeping tables are created by one idempotent DDL
batch, `SCHEMA_SQL` in `crates/stemmadb/src/lib.rs`. Index tables (FTS5,
`vec0`) and knowledge tables are created on demand by the subsystem that
owns them, so they are documented in their own sections below.

All four fixed tables are `STRICT`: SQLite enforces the declared column
types rather than applying its usual affinity coercions.

### `model_registry`

```sql
CREATE TABLE IF NOT EXISTS model_registry (
    vector_table TEXT PRIMARY KEY,
    backend      TEXT NOT NULL,
    model        TEXT NOT NULL,
    revision     TEXT NOT NULL DEFAULT '',
    dimension    INTEGER NOT NULL,
    quantization TEXT NOT NULL DEFAULT 'f32',
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;
```

One row per vector table, recording exactly which model produced the vectors
inside it. The primary key is the vector table name, which makes the central
invariant structural: *a vector table has exactly one model identity, for
its whole life.* Embeddings from two models are not comparable — cosine
similarity between them is meaningless, not merely noisy — so a model change
never mutates a table in place. It creates a new one, backfills it, and
swaps (blue-green). See
[05-encoders-decoders.md](05-encoders-decoders.md#blue-green-re-embedding).

`(backend, model, revision)` is the identity tuple that the Embedder
service returns from `ModelInfo`
([`embedder.proto`](../../proto/stemma/v1/embedder.proto)), so a stored
vector table can always be traced back to a running service. `dimension` and
`quantization` are recorded rather than inferred because `vec0` needs both
at DDL time and a mismatch is otherwise a silent correctness bug.

The table is written by `stemma_ingest::build_dense_index`, which inserts or
updates exactly one row, keyed `vector_table = 'vec_dense'`, with
`backend = 'staged'` when the vectors arrived from an external loader rather
than from a live embedder. `revision` is left at its default and
`quantization` at `'f32'`. `StoreBrowser.store_meta()` surfaces the whole
table to the console.

### `vec_staging` and `vec_dense`

Vectors reach the store in two steps, because the loader and the consumer
have different capabilities.

**`vec_staging`** is a plain table, created and filled by an external loader
— [`eval/legal/load_vectors.py`](../../eval/legal/load_vectors.py) is the
reference one:

```sql
CREATE TABLE vec_staging (
    src_table  TEXT NOT NULL,
    src_column TEXT NOT NULL,
    src_rowid  INTEGER NOT NULL,
    dim        INTEGER NOT NULL,
    model      TEXT NOT NULL,
    embedding  BLOB NOT NULL          -- little-endian f32, `dim` floats
);
```

It is a plain table specifically so that **a loader does not need the
sqlite-vec extension**. Python's stock `sqlite3` cannot create a `vec0`
virtual table; it can insert blobs into an ordinary one. The model identity
travels *with the rows* (`model` and `dim` on every row) rather than in a
side channel, which is what lets the promotion step reject a mixed batch.

**`vec_dense`** is the `vec0` virtual table, created by
`build_dense_index(db)` inside the extension-bearing server process at
startup:

```sql
CREATE VIRTUAL TABLE vec_dense USING vec0(
    embedding  float[{dim}],
    src_table  text,
    src_column text,
    src_rowid  integer
);
```

Promotion reads the distinct `(model, dim)` pairs from staging — **more than
one is a hard failure, not a merge** — drops and recreates `vec_dense` at
that dimension, `INSERT … SELECT`s the vectors across, upserts the
`model_registry` row, then `DROP TABLE vec_staging`. Staging is consumed, so
promotion is not accidentally repeatable. With no staging table present,
`build_dense_index` reports the existing index (`promoted: false`) if one is
registered and returns `None` if not, so a restart is a no-op.

Note the vector table stores the *provenance triple*, not the value: dense
hits are joined back to `lex_values` for their `value` and `is_doc`. That
keeps one copy of the text and makes the dense channel's candidates
structurally identical to the lexical channels'.

Four honest observations about this shape:

- **The name is fixed.** One `vec_dense` per store means one vector
  generation, for one `(table, column)` pair, at a time. The registry's
  table-keyed design supports many; the promotion code creates one.
- **`DROP TABLE IF EXISTS vec_dense` precedes the insert**, so promotion is
  destructive rather than blue-green: there is a window during startup with
  no dense index. Acceptable because it happens before the server serves, but
  it is not the online swap described in
  [05-encoders-decoders.md](05-encoders-decoders.md#blue-green-re-embedding).
- **A mixed-identity staging table panics.** The invariant is right — never
  mix vector spaces — but a `panic!` in a library function called during
  server startup is a blunt way to enforce it.
- **Partial coverage is silent.** The loader reports uuids it could not map
  to a rowid and continues, so a partial vector set stages partially.
  Retrieval stays correct (unembedded rows simply never appear as dense
  hits), but dense recall is capped with nothing recording that it was.

### `embed_queue`

```sql
CREATE TABLE IF NOT EXISTS embed_queue (
    id           INTEGER PRIMARY KEY,
    src_table    TEXT NOT NULL,
    src_rowid    INTEGER NOT NULL,
    serialized   TEXT NOT NULL,
    enqueued_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (src_table, src_rowid)
) STRICT;
```

The write path never waits on a model. Ingest enqueues `(src_table,
src_rowid, serialized)` — `serialized` is the row rendered to the text the
embedder will see — and an async worker drains the queue through the
`Embedder` backend. The `UNIQUE (src_table, src_rowid)` constraint makes
enqueue idempotent: re-ingesting a row that is already queued is a no-op
upsert rather than a duplicate embedding job.

The failure semantics matter more than the schema: if the embedder is down,
the queue grows and retrieval degrades to lexical-only. It does not fail.

*Today: created, drained by nothing.* The dense channel that exists is fed by
external staging (`vec_staging`, above), not by this queue — so the
index-time embedding path is still unbuilt even though the query-time one is
not.

### `query_log`

```sql
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
```

Written by `stemma-server` on every non-empty query, in `Resolver::trace_for`
— so both `Resolve` and `Explain` are recorded, and a query that came in
through the MCP tool is recorded exactly once even though the MCP layer calls
`Explain` and then derives its digest locally.

`source` and `session` come straight from `ResolveOptions` and are the v3
addition. `source` is a free-text provenance tag the caller sets
(`"console"`, `"agent"`, `"mcp"`); `session` is a conversation or agent
session id. They exist so that history is attributable: "what did this agent
session actually ask" is a question you answer with a `WHERE session = ?`,
not by correlating timestamps. Empty strings are the honest default for
callers that pass no options — the columns are `NOT NULL DEFAULT ''` rather
than nullable so that grouping never has to special-case `NULL`.

The write is deliberately best-effort:

```rust
// Query history is store working memory; a failed write must never
// fail the resolution.
let _ = db.conn().execute("INSERT INTO query_log …", …);
```

A live store from the legal corpus shows the shape:

| source | rows |
|---|---|
| `console` | 9 |
| `""` (grpcurl / the example binary) | 7 |

### `chat_log`

```sql
CREATE TABLE IF NOT EXISTS chat_log (
    id           INTEGER PRIMARY KEY,
    conversation TEXT NOT NULL DEFAULT 'default',
    role         TEXT NOT NULL,
    content      TEXT NOT NULL,
    trail        TEXT NOT NULL DEFAULT '[]',
    said_at      TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;
CREATE INDEX IF NOT EXISTS chat_log_conv ON chat_log(conversation, id);
```

Conversational history, one row per turn. `trail` is a JSON array of the
tool calls the assistant made for that turn — `{tool, args, result}`, with
the full resolve trajectory attached as `trace` when the tool was `resolve`.
Storing it as JSON text rather than a normalized table is a deliberate
choice: the trail is an opaque rendering artifact for the console, it is
never joined against, and its shape follows the trace proto, which changes
faster than a schema should.

`chat_log` is the one store table written from outside the Rust core:
[`ui/agent_backend.py`](../../ui/agent_backend.py) opens the store read-write
to append turns. That is sanctioned — operational memory owned by the
console — but it is the only such write, and it is worth knowing about when
reasoning about who holds the store's write lock.

The `(conversation, id)` index serves both access patterns: replaying one
conversation in order, and the conversation list (`GROUP BY conversation`
with a correlated subquery for the opening user line).

### Why history lives in the store at all

Because it is queryable like everything else. The store is a SQLite file the
user can open; putting history in it means "which queries produce zero
mentions" is a `SELECT`, not a log-scraping exercise. It is also per-database
by construction — history follows the corpus it is about, and deleting the
store deletes the history with the rest of the derived state.

## Migration discipline

The store schema version lives in `PRAGMA user_version`, exposed as
`stemmadb::STORE_SCHEMA_VERSION` (currently **3**). `init_store_schema()`
implements the whole policy in fifteen lines:

```rust
let found: i32 = pragma_query_value("user_version");
if found > STORE_SCHEMA_VERSION {
    return Err(Error::StoreVersionMismatch { found, supported });
}
if found < STORE_SCHEMA_VERSION {
    conn.execute_batch(SCHEMA_SQL)?;        // idempotent, whole schema
    if found == 2 { /* guarded ALTERs */ }
    conn.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
}
```

The rules:

1. **Only the future is an error.** A store written by a newer build is
   rejected with a message that names both versions and says what to do
   (re-ingest). A store from the past is upgraded silently.
2. **Migration is re-application.** Because every statement in `SCHEMA_SQL`
   is `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`, applying
   the entire current schema to a store at any older version is correct and
   cheap. There is no per-version migration script to get wrong, and no
   ordering to maintain.
3. **`ALTER` is the exception, and is guarded by version.** `ALTER TABLE …
   ADD COLUMN` is not idempotent, so it cannot live in `SCHEMA_SQL`. v3's
   addition of `query_log.source` and `query_log.session` is guarded by
   `found == 2` — precisely the case where `query_log` exists without them.
   A v0 or v1 store never had `query_log` at all, so `SCHEMA_SQL` creates it
   already carrying the new columns and the `ALTER` must not run. The guard
   is exact, not approximate.
4. **Additive only.** No column is ever dropped or retyped, and no table is
   renamed. This is affordable precisely because the store is derived: if a
   change ever genuinely needs to be destructive, the answer is to bump the
   version and let the user re-ingest.

### Shape-change self-healing below the version

Two subsystems own tables whose *shape* can change without a store-version
bump, because they are recomputable in seconds-to-minutes and a version bump
would force an unnecessary full re-ingest of everything else. Both detect
their own staleness and repair by dropping and rebuilding:

- **Lexical index** (`build_lexical_index`): if `lex_values` exists but
  `pragma_table_info('lex_values')` has no `is_doc` column, drop
  `lex_values`, `lex_fts`, `lex_trigram` and force a rebuild.
- **Knowledge store** (`SqliteKnowledgeStore::new`): if `kg_edges` exists
  without a `props` column, drop `kg_edges`, `kg_nodes`, `kg_meta` and let
  the next `compile` rebuild.

The discipline is: *store-version migrations for bookkeeping tables the user
might care about; drop-and-rebuild for pure derived indexes.*

### Compiler-versioned fingerprints

The knowledge compiler carries a third kind of version, orthogonal to both.
`kg_meta.fingerprint` stores a per-table content fingerprint prefixed with a
compiler tag:

```rust
Ok(format!("kg2:{n}:{mx}:{sum}"))   // count, max(rowid), sum(rowid)
```

The `kg2:` prefix versions the *algorithm*, not the data. Bumping it
invalidates every stored fingerprint at once, so an improvement to term
selection or join mining recompiles every table on the next run without any
migration machinery and without touching `PRAGMA user_version`. This is the
mechanism that makes the knowledge graph safe to keep improving. See
[04-knowledge-graph.md](04-knowledge-graph.md#incremental-maintenance).

## The lexical index

Built by [`stemma-ingest`](../../crates/stemma-ingest/src/lib.rs) from the
attached user database. One physical table plus two FTS5 virtual tables over
it.

```sql
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
CREATE INDEX IF NOT EXISTS lex_values_src  ON lex_values(src_table, src_rowid);

CREATE VIRTUAL TABLE IF NOT EXISTS lex_fts USING fts5(
    value, content='lex_values', content_rowid='id', tokenize='unicode61'
);
CREATE VIRTUAL TABLE IF NOT EXISTS lex_trigram USING fts5(
    value, content='lex_values', content_rowid='id',
    tokenize='trigram case_sensitive 0'
);
```

**The unit of indexing is the cell, not the row.** One `lex_values` row per
non-empty text cell of the user database, carrying its full provenance
`(src_table, src_column, src_rowid)`. This is what lets a resolution cite
`regulations.text #28209` rather than a vague "row 28209 matched", and it is
what the `Candidate` message carries over the wire. It also means one user
row that matches in two columns produces two candidates, which the fusion
stage does not merge — see
[03-resolution.md](03-resolution.md#known-limitations).

**Three views of the same cell.** `value_norm` is `lower(trim(value))` and
is B-tree indexed, giving O(log n) exact lookup that is case- and
edge-whitespace-insensitive. `lex_fts` is a `unicode61`-tokenized FTS5 index
supporting BM25-ranked word search [Robertson 2009]. `lex_trigram`
is an FTS5 index with the `trigram` tokenizer, which indexes overlapping
3-character sequences and therefore matches substrings and near-misses that
word tokenization cannot — `Northgate` inside `Seattle - Northgate`. Both
FTS5 tables are **external-content** (`content='lex_values'`), so the text
is stored once and the FTS tables hold only their inverted indexes.

**`is_doc` is a classification, computed at index time:**

```sql
length("col") >= 200            -- stemma_ingest::DOC_MIN_LEN
```

A value at or beyond `DOC_MIN_LEN` is a *document*: mentions resolve **into**
it (BM25/snippet semantics) rather than **equal** it. This single bit changes
the scoring branch a candidate takes, and getting it wrong in either
direction breaks retrieval on one corpus class or the other. The complementary
constant `EXACT_MAX_LEN = 120` bounds the exact-match channel from the other
side: a 3,000-character regulation body is never a value a mention equals, so
the exact channel simply excludes anything longer than 120 characters. The
gap between 120 and 200 is intentional slack — values in it are neither
exact-matchable nor length-penalty-exempt.

**Column selection.** `text_columns()` walks `src.sqlite_master` for tables,
then `pragma_table_info(?, 'src')` for each, keeping columns whose declared
type contains `TEXT` or `CHAR` (case-insensitively). This is a declared-type
test, not a value test: an untyped or `BLOB` column holding text is not
indexed, and a `TEXT` column holding uuids is. See the honest note on
identifier columns below.

**Rebuild policy.** `build_lexical_index(db, force)` skips the work when
`lex_values` already has rows and `force` is false; the server passes
`false`, so restarting is cheap. A rebuild is `DELETE FROM lex_values` plus
FTS5's `'delete-all'` command, then one `INSERT … SELECT` per text column
and two bulk `INSERT … SELECT` statements to populate the FTS indexes from
the fully-populated base table. Populating the FTS tables *after* all values
are inserted, rather than per column, is what keeps ingest to a small number
of large statements.

### What the index actually looks like

The bundled legal corpus (California Code of Regulations + eCFR, one user
database, two tables) produces:

| src_table | src_column | rows | is_doc | min len | max len | avg len |
|---|---|---:|---:|---:|---:|---:|
| `regulations` | `category` | 57,523 | 0 | 57 | 57 | 57 |
| `regulations` | `license` | 57,523 | 0 | 9 | 9 | 9 |
| `regulations` | `text` | 57,523 | 57,523 | 224 | 870,693 | 2,660 |
| `regulations` | `uuid` | 57,523 | 0 | 36 | 36 | 36 |
| `sections` | `category` | 35,173 | 0 | 31 | 31 | 31 |
| `sections` | `license` | 35,173 | 0 | 9 | 9 | 9 |
| `sections` | `text` | 35,173 | 35,173 | 257 | 157,170 | 16,151 |
| `sections` | `uuid` | 35,173 | 0 | 36 | 36 | 36 |

370,784 indexed cells, 92,696 of them documents, across 8 text columns.

Two honest observations from that table. First, **three quarters of the
index is not worth indexing**: `uuid` is all-distinct and never mentioned in
prose, `license` and `category` are single-valued constants. A per-column
profiling pass that skips all-distinct identifier-shaped columns and
single-valued constants is designed but unbuilt; the knowledge compiler
already computes the distinctness statistics that would drive it. Second,
the maximum `regulations.text` length is 870 KB — a single cell nearly a
megabyte long, which the trigram index must tokenize into hundreds of
thousands of trigrams. Document-shaped corpora stress the index in ways
value-shaped corpora do not.

### `lex_vocab`

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS lex_vocab
    USING fts5vocab('lex_fts', 'row');
```

Created by the knowledge compiler (`compile_term_profile`), not by ingest,
though it lives in the lexical namespace. `fts5vocab` in `'row'` mode exposes
one row per distinct term of `lex_fts` with `(term, doc, cnt)` — document
frequency and total occurrence count. It is the corpus statistics table that
term selection runs on. Its one limitation shapes the algorithm above it:
**`fts5vocab` reports statistics for the whole FTS index, not per source
table**, so document frequencies are corpus-wide even when term selection is
scoped to one table. For the common single-document-table store this is
exact; for the legal corpus it means `regulations` and `sections` share a
document-frequency denominator.

## Knowledge store schema

Owned by [`stemma-kg`](../../crates/stemma-kg/src/lib.rs); the algorithms
that fill it are in [04-knowledge-graph.md](04-knowledge-graph.md).

```sql
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

CREATE TABLE IF NOT EXISTS kg_meta (
    src_table   TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    compiled_at TEXT NOT NULL DEFAULT (datetime('now'))
) STRICT;
```

This is the "simple graph" shape — a node table and an edge table — chosen so
that traversal is expressible in recursive CTEs and the whole graph is
visible to any SQLite client. Four details carry weight:

- **`key` is a structured, human-readable stable identity**, not a surrogate:
  `table:offices`, `column:offices.city`, `value:offices.city:seattle`,
  `term:regulations:commissioner`, `phrase:regulations:california coastal
  commission`. Because keys are prefixed by kind and then by source table,
  *the set of nodes derived from one user table is a set of key prefixes* —
  which is exactly what incremental recompilation needs to delete. `UNIQUE`
  on `key` makes `upsert_node` a single `INSERT … ON CONFLICT(key) DO UPDATE`.
- **`PRIMARY KEY (src, dst, kind)` with `WITHOUT ROWID`** stores the edge
  table as a covering B-tree keyed by the triple. Edges are therefore
  multi-relational (the same pair may be connected by `fk` and by
  `inferred_fk`) but never duplicated, and `upsert_edge` is likewise a single
  conflict-resolving insert. The `REFERENCES kg_nodes(id)` constraints are
  live because `open()` sets `PRAGMA foreign_keys = on`.
- **`props` is a JSON bag** queried with SQLite's JSON1 operators
  (`json_extract(props, '$.centrality')`, `json_set`). Cold properties —
  counts, types, confidences, centrality, TextRank scores — vary by node kind
  and change as the compiler improves; a column per property would be a
  migration per idea.
- **Every edge carries provenance in `props`**: `{"method": "declared" |
  "inferred" | "profiled" | "textrank", "confidence": …}`. A test asserts
  that no edge exists without a `method`. An edge you cannot explain is an
  edge you cannot weight, and a consumer that cannot distinguish a declared
  foreign key from a 0.95-containment guess will eventually trust the guess
  too much.

The compiled legal-corpus graph:

| node kind | count | | edge kind | count |
|---|---:|---|---|---:|
| `table` | 2 | | `has_column` | 10 |
| `column` | 10 | | `term` | 88 |
| `value` | 4 | | `cooccurs` | 80 |
| `term` | 88 | | `frequent_value` | 4 |

`kind = 'term'` covers both single-word TextRank terms (24 per document
table) and mined capitalized phrases (20 per table); the two are told apart
by their key prefix (`term:` vs `phrase:`) and by `props.phrase`.

## Evidence and trace model

Two RPCs, two levels of detail, one underlying `Trace`.

### `Trace` — the internal artifact

`stemma_resolve::Trace` is the complete record of one resolution:

```rust
pub struct Trace {
    pub query: String,
    pub tokens: Vec<Token>,      // text, byte start/end, stopword
    pub spans: Vec<Span>,        // EVERY span enumerated, including skipped
    pub mentions: Vec<usize>,    // indices into spans, in query order
    pub elapsed_ms: f64,
}
```

Each `Span` carries its `status` (`selected` | `overlapped` | `no_candidates`
| `weak` | `skipped`), whether it matched a knowledge-graph entity
(`kg_alias`), and every `Candidate` gathered for it — selected and rejected
alike. Each `Candidate` carries `(table, column, rowid, value,
value_truncated, score, channels, selected, reject_reason, is_doc, snippet)`.
It is `serde::Serialize`, which is what makes
[`examples/trace_dump.rs`](../../crates/stemma-resolve/examples/trace_dump.rs)
a two-line program.

The design commitment: **the trace records what was considered, not what was
concluded.** A candidate that lost is kept with the reason it lost
(`below_threshold`, `outranked`, `span_not_selected`). This is not a
debugging convenience — it is the contract the console's trajectory view and
the Explain RPC are built on, and it is what makes a wrong resolution
diagnosable rather than merely wrong.

### `ResolveResponse` — the answer

`trace_to_proto` projects the trace down to selected mentions with selected
candidates:

```protobuf
message ResolveResponse {
  repeated Mention mentions = 1;
  string rewritten_query = 2;   // empty until substitution is implemented
}

message Mention {
  string text = 1;
  uint32 start = 2;             // byte offsets into ResolveRequest.query
  uint32 end = 3;
  repeated Candidate candidates = 4;
  bool nil = 5;                 // affirmative "no record matches"
}

message Candidate {
  string table = 1;
  int64  rowid = 2;             // 0 when the candidate is a schema element
  string column = 3;
  string value = 4;
  double score = 5;
  repeated Evidence evidence = 6;
  string snippet = 7;
  bool   is_doc = 8;
}
```

Byte offsets, not character offsets, and they are asserted round-trippable:
`proto_conversion_keeps_offsets_and_evidence` checks
`query[m.start..m.end] == m.text` for every mention.

`nil` distinguishes *"the pipeline concluded nothing matches"* from *"nothing
was found"* — the explicit-NIL discipline from the entity-linking literature.
It is a field in the wire format and always `false` today, because the stage
that would set it affirmatively (LM adjudication) is unbuilt.

`rewritten_query` is the designed substitution artifact — the query with
mentions replaced by canonical values, ready to feed a downstream generator.
It is always empty today. See
[05-encoders-decoders.md](05-encoders-decoders.md#the-rewritten_query-artifact).

### `Evidence` — why a candidate is believed

```protobuf
message Evidence {
  oneof kind {
    LexicalMatch  lexical = 1;      // channel, matched_text, score
    SemanticMatch semantic = 2;     // model, similarity
    KgPath        kg_path = 3;      // node labels, edge labels
    ProbeResult   probe = 4;        // sql, row_count
    Adjudication  adjudication = 5; // model, rationale
  }
}
```

Every candidate carries at least one; a test enforces it. The five variants
map one-to-one onto the five ways stemma can come to believe something, and
the union is closed deliberately — a candidate whose support cannot be
expressed as one of these five is a candidate the system should not be
returning.

**Only `LexicalMatch` is produced today**, one per channel that fired, with
`channel ∈ {exact, bm25, trigram, dense, kg}`. Two of those are not lexical
at all: `kg` records the knowledge-coherence bonus, and `dense` records a
vec0 KNN hit whose `score` is a cosine similarity. Both ride in
`LexicalMatch` because `trace_to_proto` maps every `ChannelScore` through the
same constructor. **`dense` hits should be emitting `SemanticMatch`** — the
message exists, carries exactly the right fields (`model`, `similarity`), and
would let a consumer tell a cosine from a BM25 score without string-matching
the channel name. That is a wire-format gap, listed in
[03-resolution.md](03-resolution.md#known-limitations). `ProbeResult` waits on
verification probes and `Adjudication` on the LM band; the richer `KgPath`
evidence belongs to collective disambiguation.

`LexicalMatch.matched_text` is the document snippet when there is one and the
stored value otherwise — the point being that evidence should show *what
matched*, and for an 800 KB regulation the value is not that.

### `ExplainResponse` — the trajectory

`Explain` takes the same `ResolveRequest` and returns the whole trace:
`TraceToken`s, every `TraceSpan` including `skipped` ones, every
`TraceCandidate` with its `TraceChannelScore`s and `reject_reason`, and the
`mentions` index list. It is the analogue of SQL's `EXPLAIN`, and it is what
the console renders and what the MCP `resolve` tool ships as structured
content alongside its compact digest.

`TraceCandidate.value` is truncated to 160 characters with an ellipsis and
flagged `value_truncated` — transport economy, with the full value always one
`SELECT` away via `(table, column, rowid)`.

## Field-by-field: what is live and what is declared

| Field | Status |
|---|---|
| `Candidate.{table, column, rowid, value, score, is_doc, snippet}` | live |
| `Candidate.evidence[].lexical` | live (`exact`/`bm25`/`trigram`/`dense`/`kg`) |
| `Candidate.evidence[].semantic` | declared, never emitted — **dense hits use `lexical` instead** |
| `Candidate.evidence[].{kg_path, probe, adjudication}` | declared, never emitted |
| `Mention.nil` | declared, always `false` |
| `ResolveResponse.rewritten_query` | declared, always `""` |
| `ResolveOptions.{source, session}` | live — written to `query_log` |
| `ResolveOptions.{max_candidates_per_mention, allow_lm, min_confidence}` | declared, **accepted and ignored by the server** |
| `model_registry` | live — one `vec_dense` row written at promotion |
| `vec_staging` → `vec_dense` | live (external staging, promoted at startup) |
| `embed_queue` | created, drained by nothing |

Everything in the "declared" rows is designed-but-unbuilt. The proto carries
them now so that the wire format does not need a breaking change when the
stage that fills them lands.

## References

- [Robertson 2009] Stephen Robertson, Hugo Zaragoza. "The
  Probabilistic Relevance Framework: BM25 and Beyond." *Foundations and
  Trends in Information Retrieval* 3(4), 2009.

See [00-bibliography.md](00-bibliography.md) for the full reference list.
