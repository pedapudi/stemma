#!/usr/bin/env python3
"""Builds the combined legal corpus for stemma: one user database, two tables.

  regulations — California Code of Regulations (Nemotron careg subset)
  sections    — eCFR, federal regulations (Nemotron eCFR subset)

One database is the right shape: stemma's knowledge graph, lexical index and
resolution all span tables, so state and federal regulation resolve side by
side. uuids are preserved per table for joining externally computed artifacts
(the careg subset has pre-computed 1024-dim embeddings keyed by uuid).

Requires pyarrow. Usage:
    python3 eval/legal/build_legal_db.py
"""

import glob
import os
import sqlite3

import pyarrow.parquet as pq

HERE = os.path.dirname(os.path.abspath(__file__))
EVAL = os.path.dirname(HERE)

SOURCES = {
    # table -> parquet source directory
    "regulations": os.path.join(EVAL, "careg", "data", "src"),
    "sections": os.path.join(EVAL, "ecfr", "data", "src"),
}

SCHEMA = """
CREATE TABLE {table} (
    id       INTEGER PRIMARY KEY,
    uuid     TEXT NOT NULL UNIQUE,
    text     TEXT NOT NULL,
    license  TEXT NOT NULL,
    category TEXT NOT NULL
);
"""


def main():
    out = os.path.join(HERE, "data", "legal.db")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    if os.path.exists(out):
        os.remove(out)
    conn = sqlite3.connect(out)

    for table, src in SOURCES.items():
        files = sorted(glob.glob(os.path.join(src, "*.parquet")))
        if not files:
            raise SystemExit(f"no parquet under {src} — fetch the subset first")
        conn.executescript(SCHEMA.format(table=table))
        n = 0
        for f in files:
            for batch in pq.ParquetFile(f).iter_batches(batch_size=2000):
                rows = zip(
                    batch.column("uuid").to_pylist(),
                    batch.column("text").to_pylist(),
                    batch.column("license").to_pylist(),
                    [(m or {}).get("category", "") for m in batch.column("metadata").to_pylist()],
                )
                conn.executemany(
                    f"INSERT INTO {table} (uuid, text, license, category) VALUES (?, ?, ?, ?)",
                    rows,
                )
                n += batch.num_rows
        conn.commit()
        print(f"{table}: {n} rows")

    size_mb = os.path.getsize(out) / 1e6
    print(f"{out}: {size_mb:.1f} MB")
    conn.close()


if __name__ == "__main__":
    main()
