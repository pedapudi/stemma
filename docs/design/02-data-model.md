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
3. **WAL on the store** [SQLite-WAL] lets the resolution server hold the
   store read-write while the console, the Python `StoreBrowser`, and
   `sqlite3` open it `mode=ro` concurrently.

`StemmaDb::open_in_memory()` gives a throwaway pair (`:memory:` store with a
`:memory:` `src`) for tests; it is the only way to get a *writable* `src`,
and exists so tests can construct fixtures.

### Extension loading

`register_extensions()` calls `sqlite3_auto_extension(sqlite3_vec_init)`
once per process behind a `std::sync::Once`, so every connection opened
afterwards — including ones stemma did not open — has `vec0` and the
`vec_*()` scalar functions. sqlite-vec [sqlite-vec] is compiled from
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

The table is written from two places:
`stemma_ingest::build_dense_index` upserts the `'vec_dense'` row at
promotion, with `backend = 'staged'` when the vectors arrived from an
external loader, and `stemma_ingest::drain_embed_queue` inserts a row (if
absent) for each vector table it feeds — `'vec_dense'` for document items,
`'vec_interp'` for interpretation items — on the first drained batch,
carrying the live embedder's `(backend, model)` identity and observed
dimension. The `model` string is the vector-space identity the drain checks
against, for both tables, before any embedding work; `backend` records how
vectors arrived, so staged vectors and a live embedder of the same model
share a table. `revision` is left at its default and `quantization` at
`'f32'`. `StoreBrowser.store_meta()` surfaces the whole table to the
console.

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

### `vec_interp`

The second `vec0` table, created by `stemma_ingest::drain_embed_queue` on
the first drained interpretation item, with the same shape as `vec_dense`
and its own `model_registry` row under the same one-model-per-table
invariant:

```sql
CREATE VIRTUAL TABLE vec_interp USING vec0(
    embedding  float[{dim}],
    src_table  text,
    src_column text,
    src_rowid  integer
);
```

One vector per **distinct value interpretation** — `(src_table, src_column,
value_norm)` with `is_doc = 0` — keyed by the interpretation's
representative cell, `src_rowid = MIN(src_rowid)` over the rows sharing the
value. What is embedded is not the value but its **interpretation card**
(see [05-encoders-decoders.md](05-encoders-decoders.md#what-gets-embedded)):
`table · column · value` plus up to two `col: value` fragments from the
representative row, ≤ 300 characters, built at enqueue time into
`embed_queue.serialized`.

The volume argument for a per-interpretation rather than per-cell table:
interpretations dedupe over `value_norm`, so a column with a million rows
and two hundred distinct values costs two hundred vectors — on value-shaped
corpora, interpretations ≪ cells. The honest exception is identifier-shaped
columns (uuids), where every cell is its own interpretation and the dedupe
buys nothing; the designed per-column profiling pass that would skip them
(noted [below](#what-the-index-actually-looks-like)) applies here with extra
force.

The resolve pipeline reads `vec_interp` two ways: never (the table is not a
retrieval channel — cards exist to *separate ties*, not to generate
candidates) and by **direct key lookup** in the context-affinity pass, where
a tied candidate's `(table, column, representative rowid)` selects its
card's stored `embedding` back out of the virtual table in a plain filtered
scan, no KNN involved, and the cosine against the query embedding is
computed in-process.

### `embed_queue`

```sql
CREATE TABLE IF NOT EXISTS embed_queue (
    id           INTEGER PRIMARY KEY,
    src_table    TEXT NOT NULL,
    src_column   TEXT NOT NULL,
    src_rowid    INTEGER NOT NULL,
    serialized   TEXT NOT NULL DEFAULT '',
    content_hash TEXT NOT NULL DEFAULT '',           -- v5: hash of the embedded text
    status       TEXT NOT NULL DEFAULT 'pending',   -- pending | done | failed
    attempts     INTEGER NOT NULL DEFAULT 0,
    error        TEXT NOT NULL DEFAULT '',
    enqueued_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (src_table, src_column, src_rowid)
) STRICT;
CREATE INDEX IF NOT EXISTS embed_queue_status ON embed_queue(status);
```

The write path never waits on a model — the queue-driven external-worker
pattern proven by the PostgreSQL vectorizer lineage [pgai-vectorizer],
transplanted to SQLite. Two enqueue passes fill it:
`stemma_ingest::enqueue_missing_embeddings` inserts one pending item per
document cell (`lex_values.is_doc = 1`) that has no vector in `vec_dense`,
and `stemma_ingest::enqueue_missing_interpretations` one per distinct value
interpretation with no vector in `vec_interp`, keyed by the representative
`MIN(src_rowid)`. `stemma_ingest::drain_embed_queue` then works the queue in
batches through the `Embedder` backend, routing each item's vector to its
table by kind. The unique key over the provenance triple makes both
enqueues idempotent: an item already pending is a no-op, and an item once
`done` is reset to pending only if its vector has since disappeared or its
`content_hash` no longer matches the current text. (The two kinds cannot
collide on the key: a cell is either a document or a short value, never
both.)

`serialized` is the text the embedder will see, and it is also the kind
discriminator. It is empty for document items — the drain fetches the
stored value from `lex_values` at embed time rather than duplicating
megabyte documents into the queue — and holds the interpretation card
(≤ 300 chars) for interpretation items, which therefore survive even a
lexical-index rebuild: the card travels with the queue item.

`content_hash` (v5) is the refresh discipline applied to vectors: an FNV-1a
hash of the exact text the item was enqueued to embed — the raw document
for document items, the card for interpretation items. The enqueue passes
recompute it from current data; a mismatch deletes the stale vector and
resets the item to pending, so a changed source row re-embeds through the
ordinary drain with no rebuild and no restart (see
[the refresh discipline](#the-refresh-discipline)). Items from before v5
(empty hash) adopt the current text as their baseline instead of resetting.

The item lifecycle is `pending → done | failed`, and every transition keeps
the table honestly queryable — `SELECT status, count(*) FROM embed_queue
GROUP BY status` is the whole observability story, and the console's store
panel shows the pending backlog. `failed` is reached three ways, each with an
`error` note: the embedding call failed `attempts` times
(`EMBED_MAX_ATTEMPTS = 3` — a retry budget, not a forever loop), the source
text vanished from `lex_values` (index rebuilt out from under the queue —
document items only, since interpretation items carry their card), or the
`model_registry` row for `vec_dense` or `vec_interp` names a *different*
model than the configured embedder — in which case the drain refuses the
entire queue and errors loudly rather than mixing vector spaces.

The failure semantics matter more than the schema: if the embedder is down,
the queue keeps its pending items (with their attempt counts) and retrieval
degrades to lexical-only. It does not fail, and the next server start picks
the queue back up.

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
`stemmadb::STORE_SCHEMA_VERSION` (currently **5**). `init_store_schema()`
implements the whole policy in a few lines:

```rust
let found: i32 = pragma_query_value("user_version");
if found > STORE_SCHEMA_VERSION {
    return Err(Error::StoreVersionMismatch { found, supported });
}
if found < STORE_SCHEMA_VERSION {
    if /* embed_queue exists without a status column */ {
        conn.execute_batch("DROP TABLE embed_queue;")?;   // v4, guarded by shape
    }
    conn.execute_batch(SCHEMA_SQL)?;        // idempotent, whole schema
    if found == 2 { /* guarded ALTERs (v3: query_log attribution) */ }
    if /* embed_queue exists without content_hash */ {
        /* guarded ALTER (v5: content_hash, additive) */
    }
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
3. **`ALTER` is the exception, and is guarded.** `ALTER TABLE …
   ADD COLUMN` is not idempotent, so it cannot live in `SCHEMA_SQL`. v3's
   addition of `query_log.source` and `query_log.session` is guarded by
   `found == 2` — precisely the case where `query_log` exists without them.
   A v0 or v1 store never had `query_log` at all, so `SCHEMA_SQL` creates it
   already carrying the new columns and the `ALTER` must not run. v5's
   addition of `embed_queue.content_hash` follows the same pattern guarded
   by *shape* (the queue exists without the column), which is exact across
   every upgrade path — a fresh store gets the column from `SCHEMA_SQL`, a
   v4 store gets the `ALTER`, a pre-v4 store gets the v4 drop-and-recreate
   which already carries it.
4. **Additive only.** No column is ever dropped or retyped, and no table is
   renamed. This is affordable precisely because the store is derived: if a
   change ever genuinely needs to be destructive, the answer is to bump the
   version and let the user re-ingest. v4 is the one exercised case:
   reshaping `embed_queue` (per-column items, status tracking) widened its
   unique key, which no `ALTER` can do, so the migration drops and recreates
   the table — safe because the queue is transient work state that nothing
   pre-v4 ever populated, and guarded by *shape* (`embed_queue` present
   without a `status` column) rather than by version, so it runs exactly
   when the old table exists.

### Shape-change self-healing below the version

Two subsystems own tables whose *shape* can change without a store-version
bump, because they are recomputable in seconds-to-minutes and a version bump
would force an unnecessary full re-ingest of everything else. Both detect
their own staleness and repair by dropping and rebuilding:

- **Lexical index** (`build_lexical_index`): if `lex_values` exists but
  `pragma_table_info('lex_values')` has no `is_doc` column, drop
  `lex_values`, `lex_fts`, `lex_trigram`, `lex_columns` and rebuild; if
  `lex_columns` exists without `median_len` (the kind-ladder shape), drop
  just the profile table — it re-derives from `lex_values` without a
  reindex.
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
Ok(format!("kg4:{}", db.src_table_fingerprint(table)?))   // "kg4:{count}:{max}:{sum}"
```

The `kg4:` prefix versions the *algorithm*, not the data. Bumping it
invalidates every stored fingerprint at once, so an improvement to term
selection or join mining recompiles every table on the next run without any
migration machinery and without touching `PRAGMA user_version`. This is the
mechanism that makes the knowledge graph safe to keep improving. See
[04-knowledge-graph.md](04-knowledge-graph.md#incremental-maintenance).

## The refresh discipline

Every derived artifact in the store answers the same three questions: *what
inputs produced you, under which algorithm, and when?* One law covers the
lexical index, the column profiles, the derived document boundary, the
knowledge graph, and the vectors — stated once here, enforced in four
mechanisms.

### Receipts: the `derivations` table

```sql
CREATE TABLE IF NOT EXISTS derivations (
    artifact           TEXT PRIMARY KEY,   -- 'lex:{table}' | 'profiles' | 'doc_cut'
    input_fingerprint  TEXT NOT NULL,
    derivation_version INTEGER NOT NULL,   -- stemma_ingest::LEX_DERIVATION_VERSION
    derived_at         TEXT NOT NULL DEFAULT (datetime('now')),
    value_json         TEXT NOT NULL DEFAULT '{}'
) STRICT;
```

Created by ingest alongside the lex tables. A receipt is valid only when
*both* the fingerprint and the version match: the fingerprint tracks the
data, the version tracks the algorithm, and either moving re-derives the
artifact. Scalar derivations put the derived value itself in `value_json` —
the `doc_cut` receipt holds the Otsu boundary in both its `current` and
`adopted` readings (see hysteresis below) — so the store carries not just
*that* something was derived but *what was concluded*, inspectable with
plain `json_extract`.

The knowledge graph's `kg_meta` is this same discipline, older: its
fingerprint folds the algorithm version into a tag prefix (`kg4:{triple}`)
instead of a separate column, and its `value_json` is the graph itself. It
is deliberately **not** migrated into `derivations` — two tables, one law.

### Change detection: the shared fingerprint

`StemmaDb::src_table_fingerprint(table)` returns `"{count}:{max_rowid}:
{sum_rowid}"` — three aggregates, no text hashing, computable by index scan.
Both derivers compare against it: the knowledge compiler per `kg_meta` row,
and `build_lexical_index` per `lex:{table}` receipt. At every registration
(server startup, and every refresh wake) changed tables re-ingest — their
lex rows and FTS entries replaced table-wise, their receipts restamped —
and the corpus-level artifacts (profiles, document cut) re-derive whenever
any table moved. Unchanged corpora cost one aggregate scan per table and no
writes. The blind spot is documented in
[04-knowledge-graph.md](04-knowledge-graph.md#the-fingerprint): an in-place
`UPDATE` preserving all three aggregates is invisible, and `force` is the
escape hatch.

The cheap *global* signal is `PRAGMA src.data_version`, a counter SQLite
bumps when another connection commits to the attached user database. Each
served database gets one background thread that polls it on a modest
interval (`REFRESH_POLL_SECS = 60` in stemma-server — an operational
cadence, not a data-derived quantity) and, only when the counter moved, runs
the registration path again: lexical receipts, KG fingerprints, embed pass.
No filesystem watcher, no restart, and the poll itself is one pragma read.

### Re-embedding on change: content hashes

Embed-queue items carry `content_hash` (v5, additive column) — an FNV-1a
hash of the exact text the item was enqueued to embed: the raw document for
document items, the serialization card for interpretation items. The
enqueue passes recompute the hash from current data; a mismatch means the
encoder's input changed, so the stale vector is deleted and the item resets
to pending, where the ordinary drain picks it up. Nothing else re-embeds:
a table re-ingest touches every row of that table, but only the rows whose
*text* actually changed get new vectors. Items predating v5 (empty hash)
adopt the current text as baseline rather than resetting — their provenance
is unknowable, and every change from then on is caught.

### Hysteresis: derived boundaries move deliberately

The document cut re-derives on every profile pass and is recorded as
`current` in the `doc_cut` receipt — but the stamped `is_doc` bits follow
the `adopted` value, which is replaced only when adopting the fresh cut
would change at least one column's document-ness. A boundary that drifts a
few percent with every appended row must not churn state that only cares
which side of it each column lands on. When adoption *does* flip a column,
the flip changes the column's channel (document ↔ interpretation card), so
its queue items and vectors are invalid in a way no content hash can see;
they are dropped for re-enqueue and the blast radius — columns flipped,
items reset — is logged at info.

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

**`is_doc` is a per-column derivation, stamped at profile time.** A
document is text a mention resolves **into** (BM25/snippet semantics) rather
than **equals**, and that is a fact about a *column's role in the corpus*,
not about any one cell's character count. `profile_columns` computes each
column's median value length, takes Otsu's 2-class natural break over the
median log-lengths, and stamps every cell of the columns in the prose class
`is_doc = 1` — consumers read the same bit they always did, but it now means
"this cell belongs to a document column of this corpus" rather than "this
cell crossed 200 characters".

Three cases, one expression (`stemma_ingest::derive_doc_boundary`):

- medians uniformly beyond value scale → every column is a document column
  (a pure prose corpus, however its lengths cluster);
- otherwise, the boundary is the Otsu cut, never lowered below value scale —
  on value-shaped corpora (BIRD and its kin) the break falls among short
  medians, the floor prevails, and no column is a document;
- fewer than two distinct medians → no break exists, and the same floor
  decides.

"Value scale" is anchored by the one length constant that survives:
`EXACT_MAX_LEN = 120` bounds the exact-match channel — a 3,000-character
regulation body is never a value a mention equals — and it stays because it
is a transport/UX bound on that channel, not a guess about the corpus: it
defines where *being a value* operationally ends (what a mention can
literally equal, what evidence can display unabridged), which is exactly
the anchor the derivation needs. The break being corpus-relative is the
point: a 150-char-median column clusters with the values in a corpus whose
documents run to thousands of characters, where any fixed length threshold
would have guessed. The boundary's receipt — current and adopted values —
lives in `derivations` under the
[hysteresis rule](#hysteresis-derived-boundaries-move-deliberately).

**Column selection.** `text_columns()` walks `src.sqlite_master` for tables,
then `pragma_table_info(?, 'src')` for each, keeping columns whose declared
type contains `TEXT` or `CHAR` (case-insensitively). This is a declared-type
test, not a value test: an untyped or `BLOB` column holding text is not
indexed, and a `TEXT` column holding uuids is. See the honest note on
identifier columns below.

**Refresh policy.** `build_lexical_index(db, force)` is receipt-driven (see
[the refresh discipline](#the-refresh-discipline)): each table's content
fingerprint is compared against its `lex:{table}` receipt, and only changed,
new, or receipt-less tables re-ingest — their FTS rows removed with the
external-content `'delete'` command (which needs the text, so it runs while
the base rows still exist), their `lex_values` rows replaced by one `INSERT
… SELECT` per text column, their FTS entries re-inserted table-wise, their
receipt restamped. `force` treats every table as changed; an unchanged
corpus costs one aggregate scan per table and performs no writes, so both a
server restart and a refresh wake are cheap. Profiles and the document
boundary re-derive whenever any table moved. Dropped tables are swept —
rows, FTS entries, and receipt.

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
prose, `license` and `category` are single-valued constants. The column
measurements below expose such columns (both `uuid` columns carry an
`idlike_lcb` near 1, so no purpose predicate admits them) and downstream
passes skip them; the indexing itself still pays for them. Second, the
maximum `regulations.text` length is 870 KB — a
single cell nearly a megabyte long, which the trigram index must tokenize
into hundreds of thousands of trigrams. Document-shaped corpora stress the
index in ways value-shaped corpora do not.

### `lex_columns` — column measurements

```sql
CREATE TABLE IF NOT EXISTS lex_columns (
    src_table      TEXT NOT NULL,
    src_column     TEXT NOT NULL,
    n_values       INTEGER NOT NULL,
    n_distinct     INTEGER NOT NULL,   -- over value_norm
    distinct_ratio REAL NOT NULL,      -- each ratio is paired with its
    distinct_lcb   REAL NOT NULL,      -- Jeffreys lower confidence bound
    alpha_ratio    REAL NOT NULL,      -- values containing any [a-z]
    alpha_lcb      REAL NOT NULL,
    numeric_ratio  REAL NOT NULL,      -- digits and . e + - only
    numeric_lcb    REAL NOT NULL,
    temporal_ratio REAL NOT NULL,      -- epoch-ranged numbers or ISO dates
    temporal_lcb   REAL NOT NULL,
    idlike_ratio   REAL NOT NULL,      -- uuid / long hex / long digit runs
    idlike_lcb     REAL NOT NULL,
    avg_len        REAL NOT NULL,
    median_len     REAL NOT NULL,      -- input to the document boundary
    PRIMARY KEY (src_table, src_column)
) STRICT;
```

`lex_values` records what values exist; `lex_columns` records **measurements
about each column** — and nothing else. There is no stored classification:
what used to be a six-kind `kind` column assigned by a priority ladder of
fixed thresholds (0.5 / 0.8 / 0.95, plus a 20-row minimum-sample gate) is
now three **purpose predicates**, documented functions over the
measurements, evaluated where they are consumed:

| predicate | definition | consumer |
|---|---|---|
| `is_document_column` | median log-length beyond the [derived boundary](#the-lexical-index) | `is_doc` stamping; both enqueue passes |
| `is_paraphrasable_column` | letter-bearing by majority (`alpha_ratio > 1/2`) and not confidently shape-structural (`numeric_lcb`, `temporal_lcb`, `idlike_lcb` all ≤ 1/2) | interpretation-card candidacy ([05](05-encoders-decoders.md#what-gets-embedded)) |
| `is_vocabulary_column` | ≡ `is_paraphrasable_column` today — a term recurs meaningfully exactly where a paraphrase can reach; a separate name so the consumers state their purpose and can diverge without a hunt | KG term→column affinity ([04](04-knowledge-graph.md#step-6--termcolumn-affinity)) |

Every `*_ratio` is paired with its **Jeffreys lower confidence bound** at
the one conventional level (`CONFIDENCE_LEVEL = 0.95` — the only
probability constant in the crate): the 5% quantile of
Beta(k + ½, n − k + ½). Structural *denial* requires a confident majority —
LCB > ½ — while admission is the default, which is how the old
minimum-sample gate's job gets done without a gate: 5/5 numeric values
bound the proportion at only ~0.69 against arithmetic, not against a
hand-picked N. A five-row all-distinct column of names keeps its
vocabulary status because nothing about it can be confidently condemned
(`five_distinct_rows_cannot_be_condemned` asserts exactly this).

The structural shape tests themselves — parses as a number, uuid/hex
shapes, epoch ranges (`TEMPORAL_EPOCH_RANGES`), ISO dates — are
*definitions*, not tunables, and remain fixed GLOB/CAST expressions.

What the predicates deliberately no longer test: **cardinality**. The old
`identifier`-by-distinctness and `code` rules died with the ladder — keys
are caught by their shape, near-unique prose (names, titles) is legitimate
vocabulary, and a letter-bearing code scheme (`SKU-0001-Q3`) is admitted,
kept harmless by the recurrence requirement downstream. `distinct_ratio`
and `distinct_lcb` stay as measurements for anyone (the console, future
passes) to read.

`stemma_ingest::profile_columns` fills the table in one grouped pass plus
one windowed median pass over `lex_values`, then derives and stamps the
document boundary; it re-runs whenever any table's receipt moved. The table
is derived state with the same lifecycle as the FTS tables: dropped and
rebuilt with the index, never migrated, no store schema version involved —
a store with the old (`kind`-bearing) shape is detected by the missing
`median_len` column and rebuilt in place.

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
that fill it are in [04-knowledge-graph.md](04-knowledge-graph.md). The
shape is the nodes/edges-tables-plus-recursive-CTE pattern
[simple-graph], adequate to ~10⁶ edges — research scale needs no graph
engine, and the `KnowledgeStore` trait is the seam through which one could
substitute later.

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
by their key prefix (`term:` vs `phrase:`) and by `props.phrase`. The
counts are a pre-`kg3` snapshot: the `kg3` compiler additionally emits
`col_affinity` edges (term → column; see
[04-knowledge-graph.md](04-knowledge-graph.md#step-6--termcolumn-affinity)),
not reflected here.

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
`channel ∈ {exact, bm25, trigram, dense, kg, context}`. Three of those are
not lexical at all: `kg` records the knowledge-coherence bonus, `dense`
records a vec0 KNN hit whose `score` is a cosine similarity, and `context`
records the query-conditioned interpretation-card cosine that separates tied
value interpretations. All ride in
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
| `Candidate.evidence[].lexical` | live (`exact`/`bm25`/`trigram`/`dense`/`kg`/`context`) |
| `Candidate.evidence[].semantic` | declared, never emitted — **dense hits use `lexical` instead** |
| `Candidate.evidence[].{kg_path, probe, adjudication}` | declared, never emitted |
| `Mention.nil` | declared, always `false` |
| `ResolveResponse.rewritten_query` | declared, always `""` |
| `ResolveOptions.{source, session}` | live — written to `query_log` |
| `ResolveOptions.{max_candidates_per_mention, allow_lm, min_confidence}` | declared, **accepted and ignored by the server** |
| `model_registry` | live — `vec_dense` row written at promotion or first drain; `vec_interp` row at first interpretation drain |
| `vec_staging` → `vec_dense` | live (external staging, promoted at startup) |
| `vec_interp` | live — interpretation cards, created and filled by the drain |
| `embed_queue` | live — filled by `enqueue_missing_embeddings` + `enqueue_missing_interpretations`, drained through the `Embedder` at server startup |

Everything in the "declared" rows is designed-but-unbuilt. The proto carries
them now so that the wire format does not need a breaking change when the
stage that fills them lands.

## References

- [Robertson 2009] Stephen Robertson, Hugo Zaragoza. "The
  Probabilistic Relevance Framework: BM25 and Beyond." *Foundations and
  Trends in Information Retrieval* 3(4), 2009.
- [sqlite-vec] Alex Garcia. *sqlite-vec* v0.1.6 (software).
- [pgai-vectorizer] Timescale. *pgai / pgvectorizer* (software) — the
  queue-driven external-worker embedding pattern.
- [simple-graph] Denis Papathanasiou. *simple-graph* (software) — graph
  tables + recursive CTEs in SQLite.
- [SQLite-WAL] SQLite Consortium. "Write-Ahead Logging."
  sqlite.org/wal.html.

See [00-bibliography.md](00-bibliography.md) for the full reference list.
