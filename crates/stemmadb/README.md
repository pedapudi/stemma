# stemmadb

The storage layer of the stemma ecosystem. Everything stemma knows lives in
SQLite, split across two files with a strict ownership boundary:

| File | Owner | Contents | Access |
|---|---|---|---|
| `user.db` (any name) | the user | their data, untouched | **read-only**, attached as schema `src` |
| `<name>.stemmadb` | stemmadb | every derived artifact | read-write, opened as `main` |

The `.stemmadb` sidecar is itself a plain SQLite database. It holds lexical
indexes, vector tables, the compiled knowledge store, the embed queue, the
model registry, query and chat history, and explicit grounding feedback.
Derived indexes can be rebuilt from the user database and configuration.
History and feedback require their own retention and backup policy; deleting
the sidecar permanently removes them.

An optional approximate vector file is a separate rebuildable projection.
SQLite stores the receipt that binds the file to one corpus and vector
generation. An invalid projection falls back to exact SQLite search.

SQLite itself is stock. Capability comes from core modules plus the vendored
sqlite-vec extension (`third_party/sqlite_vec`), compiled into the binary and
registered process-wide through `sqlite3_auto_extension` — no runtime `.so`
loading, no fork, no patches.

## API sketch

```rust
use stemmadb::StemmaDb;

// Creates <store> if needed, attaches <user db> read-only as `src`,
// initializes/validates the store schema (versioned via PRAGMA user_version).
let db = StemmaDb::open(store_path, user_db_path)?;

db.vec_version()?;   // e.g. "v0.1.6" — proves sqlite-vec is linked
db.has_fts5()?;      // true on any stock modern SQLite
db.src_tables()?;    // tables of the attached user database

// Escape hatch while the typed API grows. `main` = store, `src` = user DB.
let conn = db.conn();
```

`StemmaDb::open_in_memory()` gives a throwaway store+source pair for tests.

## Store schema (version 7)

- **`model_registry`** — one row per vector table: `(vector_table, backend,
  model, revision, dimension, quantization, created_at, query_template,
  card_format)`. Embeddings from different vector spaces are never compared.
- **`embed_queue`** — document cells awaiting (re-)embedding:
  `(src_table, src_column, src_rowid, serialized, content_hash, status,
  attempts, error)`, with status `pending → done | failed`. Ingest enqueues;
  the server drains the queue in bounded batches with a retry budget.
- **`query_log`** — resolution and parse episodes with source, session,
  revision receipts, compact evidence selectors, and parse output.
- **`grounding_feedback`** — typed judgments linked to retained query episodes.
- **`vector_generations`** — monotonic invalidation tokens for vector-table
  content changes.
- **`vector_sidecar_receipts`** — corpus, vector-space, generation, shape,
  metric, and checksum identity for an optional approximate document-vector
  index.
- **`chat_log`** — per-database conversation history written by the console.

Schema changes bump `STORE_SCHEMA_VERSION`; a store from a *newer* build is a
hard error telling the user to re-ingest (derived state, so this is cheap),
while an older store is migrated in place at open.

## Invariants

1. **The user database is never written.** It is attached with `?mode=ro`;
   even a bug cannot mutate it. Tests assert this.
2. **Derived indexes are rebuildable.** Operational history and feedback are
   retained data and are outside that guarantee.
3. **SQL for a subsystem stays in that subsystem's backend.** Graph traversal
   SQL lives only in the SQLite `KnowledgeStore` backend (stemma-kg), index
   SQL in stemmadb/ingest — never in the resolution pipeline, which programs
   against traits.
4. **SQLite stays stock.** Extensions enter via `sqlite3_auto_extension` or
   documented virtual tables/functions only.

See [docs/architecture.md](../../docs/architecture.md) for why the layer is
shaped this way, and the [user guide](../../docs/user-guide/01-setup.md) to get
running.
