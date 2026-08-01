---
name: stemmadb-corpus
description: Convert arbitrary source data (parquet, CSV, JSON, another DB) into a stemma-ready SQLite user database. Use when asked to load a dataset into stemma/stemmadb, build a corpus, or prepare data for resolution.
---

# Building a corpus for stemmadb

A stemma corpus is nothing more than a **stock SQLite database** — but a few
schema decisions decide how well resolution works and how cheaply embeddings
can be attached later. Follow the worked example
`eval/careg/build_careg_db.py` (parquet → SQLite, 57K rows).

## Rules (each one is load-bearing)

1. **Stock SQLite only.** No stemma tables, no indexes for stemma's benefit,
   no preprocessing artifacts. Everything derived lives in the `.stemmadb`
   sidecar that stemma builds itself. The user DB will be attached read-only.

2. **`INTEGER PRIMARY KEY` on every table.** Resolutions point at rowids;
   an explicit INTEGER PRIMARY KEY keeps them stable across VACUUM.

3. **Preserve external identifiers.** If the source data has uuids/IDs, keep
   them as a UNIQUE column even if they look redundant. This is what lets
   externally computed artifacts (pre-built embeddings, labels) join back
   without re-deriving. Example: `careg.db` keeps `uuid`, which joins 100% of
   rows to pre-computed 1024-dim embedding parquets — milestone 3 loads those
   directly instead of running an embedder for hours.

4. **Declare foreign keys** (`REFERENCES`). The knowledge store compiles the
   FK graph into join paths; associative mentions ("Chen's team") resolve
   through them. Real relationships without declared FKs are invisible until
   manually declared in metadata.

5. **One concept per text column.** `name`, `title`, `body` as separate
   columns — indexes are per-column and evidence cites `(table, column,
   value)`. Don't concatenate fields into one blob; don't split one field
   across many.

6. **Don't normalize surface forms.** Keep `'Seattle - Northgate'` exactly as
   stored. Mapping oblique mentions onto stored forms is stemma's job;
   normalizing destroys the evidence and the benchmark.

7. **Text encoding**: UTF-8. Trim obvious junk (null bytes), otherwise leave
   text alone.

## Conversion template

Adapt (source-reading side varies; SQLite side shouldn't):

```python
#!/usr/bin/env python3
import sqlite3
# read: pyarrow.parquet / csv / json — whatever the source needs

conn = sqlite3.connect(out_path)          # start from a fresh file
conn.executescript("""
CREATE TABLE things (
    id       INTEGER PRIMARY KEY,
    uuid     TEXT NOT NULL UNIQUE,        -- rule 3
    name     TEXT NOT NULL,               -- rule 5
    body     TEXT NOT NULL,
    category TEXT NOT NULL
);
""")
for batch in stream_source_batches():      # stream; don't load 5M rows in RAM
    conn.executemany(
        "INSERT INTO things (uuid, name, body, category) VALUES (?, ?, ?, ?)",
        batch,
    )
conn.commit()
# print verification stats — row count, avg text length, file size
```

Conventions:
- Script lives in `eval/<corpus>/build_<corpus>_db.py`; data goes to
  `eval/<corpus>/data/` which must be **gitignored** (add
  `/eval/<corpus>/data/` to `.gitignore`); the script is committed, the data
  never is.
- Print verification output at the end (row count, avg text length, size) —
  it documents expected results for the next run.
- Idempotent: delete the output file first, rebuild from scratch.

## Verify the corpus

```sh
# 1. Registers, sidecar created, extensions live:
./bazel-bin/crates/stemma-server/stemma-server --db mycorpus=path/to/corpus.db
#    -> log line: database registered name="mycorpus" ... vec="v0.1.6"

# 2. FKs actually declared:
sqlite3 path/to/corpus.db "PRAGMA foreign_key_list(things);"

# 3. External-ID join coverage, if applicable (see rule 3):
#    count(source uuids) == count(corpus uuids) == count(intersection)
```

## Attaching pre-computed embeddings (milestone 3+)

If embeddings exist for the corpus (keyed by the preserved external ID),
record the producing model in the store's `model_registry`
(`backend, model, revision, dimension, quantization`) when loading them into
a vec0 table. Never load vectors from two different models into one table —
create separate tables and let the registry disambiguate.
