# stemmadb

The storage layer of the stemma ecosystem. Everything stemma knows lives in
SQLite, split across two files with a strict ownership boundary:

| File | Owner | Contents | Access |
|---|---|---|---|
| `user.db` (any name) | the user | their data, untouched | **read-only**, attached as schema `src` |
| `<name>.stemmadb` | stemmadb | every derived artifact | read-write, opened as `main` |

The `.stemmadb` sidecar is itself a plain SQLite database. It holds — or will
hold, as milestones land — the lexical indexes (FTS5 with the trigram
tokenizer, spellfix), the vector tables (sqlite-vec `vec0`), the compiled
knowledge store, the embed queue, and the model registry. Deleting it is always
safe: it is entirely derived state, rebuildable by re-ingesting.

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

## Store schema (version 1)

- **`model_registry`** — one row per vector table: `(vector_table, backend,
  model, revision, dimension, quantization, created_at)`. Embeddings from
  different models are never comparable, so a model change creates a *new*
  vector table, backfills it asynchronously, and swaps atomically (blue-green).
  Nothing ever mixes vector spaces in place.
- **`embed_queue`** — rows awaiting (re-)embedding: `(src_table, src_rowid,
  serialized)`. Ingest enqueues; an async worker drains through the Embedder
  backend. Writes never wait on a model, and if the embedder is down the
  system degrades to lexical-only retrieval instead of failing.

Schema changes bump `STORE_SCHEMA_VERSION`; an on-disk mismatch is a hard
error telling the user to re-ingest (derived state, so this is cheap).

## Invariants

1. **The user database is never written.** It is attached with `?mode=ro`;
   even a bug cannot mutate it. Tests assert this.
2. **All derived state is disposable.** Anything in `.stemmadb` can be rebuilt
   from `user.db` + configuration.
3. **SQL for a subsystem stays in that subsystem's backend.** Graph traversal
   SQL lives only in the SQLite `KnowledgeStore` backend (stemma-kg), index
   SQL in stemmadb/ingest — never in the resolution pipeline, which programs
   against traits.
4. **SQLite stays stock.** Extensions enter via `sqlite3_auto_extension` or
   documented virtual tables/functions only.

See [docs/architecture.md](../../docs/architecture.md) for why the layer is
shaped this way, and the [user guide](../../docs/user-guide/01-setup.md) to get
running.
