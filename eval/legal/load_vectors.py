#!/usr/bin/env python3
"""Stages pre-computed embeddings into the legal corpus's .stemmadb store.

Reads the uuid-keyed 1024-dim vectors produced over the careg subset
(~/ambit-legal/emb-1024-1M by default — the Qwen3-Embedding-0.6B base map;
point --src at emb-1024-1M-{tuned,v2,v3} to A/B a tuned checkpoint), joins
uuid → rowid through the user database, and writes a plain `vec_staging`
table. The stemma server promotes staging into the vec0 dense index and the
model registry at next startup — Python never needs the sqlite-vec extension.

Stop stemma-server before running (the store is WAL but promotion happens at
registration anyway).

Usage:
    python3 eval/legal/load_vectors.py [--src DIR] [--table regulations]
"""

import argparse
import glob
import os
import sqlite3

import pyarrow.parquet as pq

HERE = os.path.dirname(os.path.abspath(__file__))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default=os.path.expanduser("~/ambit-legal/emb-1024-1M"),
                    help="directory of vector parquet shards")
    ap.add_argument("--pattern", default="careg-*.parquet")
    ap.add_argument("--db", default=os.path.join(HERE, "data", "legal.db"))
    ap.add_argument("--store", default=os.path.join(HERE, "data", "legal.stemmadb"))
    ap.add_argument("--table", default="regulations", help="user table the vectors belong to")
    ap.add_argument("--column", default="text", help="embedded column")
    args = ap.parse_args()

    files = sorted(glob.glob(os.path.join(args.src, args.pattern)))
    if not files:
        raise SystemExit(f"no shards matching {args.pattern} under {args.src}")

    # model identity from the shard metadata written at embedding time
    meta = pq.ParquetFile(files[0]).schema_arrow.metadata or {}
    model = (meta.get(b"embedding_model") or b"unknown").decode()
    dim = int((meta.get(b"embedding_dim") or b"0").decode() or 0)

    user = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    uuid_to_rowid = dict(user.execute(f'SELECT uuid, id FROM "{args.table}"'))
    user.close()
    print(f"{args.table}: {len(uuid_to_rowid)} rows · model {model} · dim {dim}")

    store = sqlite3.connect(args.store)
    store.executescript(
        """
        DROP TABLE IF EXISTS vec_staging;
        CREATE TABLE vec_staging (
            src_table  TEXT NOT NULL,
            src_column TEXT NOT NULL,
            src_rowid  INTEGER NOT NULL,
            dim        INTEGER NOT NULL,
            model      TEXT NOT NULL,
            embedding  BLOB NOT NULL
        );
        """
    )
    staged = missed = 0
    for f in files:
        for batch in pq.ParquetFile(f).iter_batches(batch_size=2000, columns=["uuid", "embedding"]):
            uuids = batch.column("uuid").to_pylist()
            vecs = batch.column("embedding")
            rows = []
            for i, u in enumerate(uuids):
                rowid = uuid_to_rowid.get(u)
                if rowid is None:
                    missed += 1
                    continue
                vec = vecs[i].values.to_numpy(zero_copy_only=False).astype("float32")
                if not dim:
                    dim = len(vec)
                rows.append((args.table, args.column, rowid, dim, model, vec.tobytes()))
            store.executemany(
                "INSERT INTO vec_staging (src_table, src_column, src_rowid, dim, model, embedding) "
                "VALUES (?, ?, ?, ?, ?, ?)",
                rows,
            )
            staged += len(rows)
    store.commit()
    store.close()
    print(f"staged {staged} vectors ({missed} uuids not in {args.table}); "
          "restart stemma-server to promote into vec0")


if __name__ == "__main__":
    main()
