#!/usr/bin/env python3
"""Builds the eCFR (federal regulations) user database for stemma.

Input: the Nemotron-Pretraining-Legal-California-Code-Of-Regulations parquet
(schema: text, license, metadata{category, models_used}, uuid), expected under
eval/ecfr/data/src/. Output: a stock SQLite database (eval/ecfr/data/ecfr.db)
that stemma treats as a read-only user DB — one row per regulation text, with
uuid preserved so the pre-computed embeddings in ~/ambit-legal/emb-1024-1M
(ecfr-*.parquet) can be joined in later without re-embedding.

Requires pyarrow. Usage:
    python3 eval/ecfr/build_ecfr_db.py [--src DIR] [--out FILE]
"""

import argparse
import glob
import os
import sqlite3

import pyarrow.parquet as pq

SCHEMA = """
CREATE TABLE sections (
    id       INTEGER PRIMARY KEY,
    uuid     TEXT NOT NULL UNIQUE,
    text     TEXT NOT NULL,
    license  TEXT NOT NULL,
    category TEXT NOT NULL
);
"""


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default=os.path.join(here, "data", "src"))
    ap.add_argument("--out", default=os.path.join(here, "data", "ecfr.db"))
    args = ap.parse_args()

    files = sorted(glob.glob(os.path.join(args.src, "*.parquet")))
    if not files:
        raise SystemExit(f"no parquet files under {args.src}")

    if os.path.exists(args.out):
        os.remove(args.out)
    conn = sqlite3.connect(args.out)
    conn.executescript(SCHEMA)

    n = 0
    for f in files:
        for batch in pq.ParquetFile(f).iter_batches(batch_size=2000):
            texts = batch.column("text").to_pylist()
            licenses = batch.column("license").to_pylist()
            metas = batch.column("metadata").to_pylist()
            uuids = batch.column("uuid").to_pylist()
            conn.executemany(
                "INSERT INTO sections (uuid, text, license, category) VALUES (?, ?, ?, ?)",
                [
                    (u, t, lic, (m or {}).get("category", ""))
                    for u, t, lic, m in zip(uuids, texts, licenses, metas)
                ],
            )
            n += len(uuids)
    conn.commit()

    avg_len = conn.execute("SELECT avg(length(text)) FROM sections").fetchone()[0]
    conn.close()
    size_mb = os.path.getsize(args.out) / 1e6
    print(f"{args.out}: {n} sections, avg text {avg_len:.0f} chars, {size_mb:.1f} MB")


if __name__ == "__main__":
    main()
