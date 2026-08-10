#!/usr/bin/env python3
"""Stages pre-computed embeddings into the legal corpus's .stemmadb store.

Reads the uuid-keyed 1024-dim vectors produced over the careg subset
(~/ambit-legal/emb-1024-1M by default — the Qwen3-Embedding-0.6B base map;
point --src at emb-1024-1M-{tuned,v2,v3} to A/B a tuned checkpoint), joins
uuid → rowid through the user database, and writes a plain `vec_staging`
table. The stemma server promotes staging into the vec0 dense index and the
model registry at next startup — Python never needs the sqlite-vec extension.

The staged batch carries its query template: the query-side convention
('{query}' placeholder) the anchors must be queried under is part of the
vector-space identity, and promotion records it in model_registry so query
time reads it as stored fact instead of guessing by model name. A fine-tuned
checkpoint served under a name the family lookup misses (qwen3-emb-legal-v3)
measured paraphrase recall@5 halved, 0.18 → 0.08, when that guess sent
queries out bare against templated anchors — pass --query-template whenever
the model tag does not name its family.

Stop stemma-server before running (the store is WAL but promotion happens at
registration anyway).

Usage:
    python3 eval/legal/load_vectors.py [--src DIR] [--table regulations]
        [--query-template TEMPLATE]
"""

import argparse
import glob
import os
import sqlite3
import sys

import pyarrow.parquet as pq

HERE = os.path.dirname(os.path.abspath(__file__))

# The Qwen3-Embedding family's retrieval instruction, as published with the
# models — byte-identical to stemma_embed::QWEN3_QUERY_TEMPLATE, which is the
# authoritative copy.
QWEN3_QUERY_TEMPLATE = (
    "Instruct: Given a search query, retrieve relevant passages that answer "
    "the query\nQuery: {query}"
)

# Explicit spelling of "queries embed bare": recording it makes bare a stated
# convention instead of an absence a later reader has to guess about.
BARE_TEMPLATE = "{query}"


def resolve_query_template(model, explicit):
    """The template to stage, and how it was chosen.

    An explicit template is taken as-is ('{query}' states bare). Otherwise
    the model tag resolves by family — the same lookup as
    stemma_embed::default_query_template — and what was resolved is printed,
    because promotion turns this value into the space's recorded identity.
    When the tag names no known family, bare is recorded WITH A LOUD WARNING:
    a tag like a fine-tuned checkpoint's serving alias may well belong to a
    templated family the name hides, and only --query-template can say so.
    """
    if explicit is not None:
        return explicit, "explicit --query-template"
    if "qwen3-embedding" in model.lower():
        return QWEN3_QUERY_TEMPLATE, "resolved by model family (qwen3-embedding)"
    print(
        f"WARNING: model tag {model!r} names no known template family; staging\n"
        f"WARNING: this batch as BARE ('{{query}}'). If this checkpoint expects a\n"
        f"WARNING: retrieval instruction (e.g. a fine-tuned Qwen3-Embedding served\n"
        f"WARNING: under another name), querying it bare roughly halves recall —\n"
        f"WARNING: re-run with --query-template to state the convention.",
        file=sys.stderr,
    )
    return BARE_TEMPLATE, "unresolved family — bare, see warning"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default=os.path.expanduser("~/ambit-legal/emb-1024-1M"),
                    help="directory of vector parquet shards")
    ap.add_argument("--pattern", default="careg-*.parquet")
    ap.add_argument("--db", default=os.path.join(HERE, "data", "legal.db"))
    ap.add_argument("--store", default=os.path.join(HERE, "data", "legal.stemmadb"))
    ap.add_argument("--table", default="regulations", help="user table the vectors belong to")
    ap.add_argument("--column", default="text", help="embedded column")
    ap.add_argument("--query-template", default=None,
                    help="query-side template with a '{query}' placeholder that this "
                         "batch's anchors must be queried under; '{query}' states bare. "
                         "Default: resolve by model family from the shard metadata's "
                         "model tag (bare, loudly, when nothing resolves)")
    args = ap.parse_args()

    files = sorted(glob.glob(os.path.join(args.src, args.pattern)))
    if not files:
        raise SystemExit(f"no shards matching {args.pattern} under {args.src}")

    # model identity from the shard metadata written at embedding time
    meta = pq.ParquetFile(files[0]).schema_arrow.metadata or {}
    model = (meta.get(b"embedding_model") or b"unknown").decode()
    dim = int((meta.get(b"embedding_dim") or b"0").decode() or 0)
    template, how = resolve_query_template(model, args.query_template)

    user = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    uuid_to_rowid = dict(user.execute(f'SELECT uuid, id FROM "{args.table}"'))
    user.close()
    print(f"{args.table}: {len(uuid_to_rowid)} rows · model {model} · dim {dim}")
    print(f"query template ({how}): {template!r}")

    store = sqlite3.connect(args.store)
    store.executescript(
        """
        DROP TABLE IF EXISTS vec_staging;
        CREATE TABLE vec_staging (
            src_table      TEXT NOT NULL,
            src_column     TEXT NOT NULL,
            src_rowid      INTEGER NOT NULL,
            dim            INTEGER NOT NULL,
            model          TEXT NOT NULL,
            embedding      BLOB NOT NULL,
            query_template TEXT NOT NULL
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
                rows.append((args.table, args.column, rowid, dim, model,
                             vec.tobytes(), template))
            store.executemany(
                "INSERT INTO vec_staging (src_table, src_column, src_rowid, dim, model, "
                "embedding, query_template) VALUES (?, ?, ?, ?, ?, ?, ?)",
                rows,
            )
            staged += len(rows)
    store.commit()
    store.close()
    print(f"staged {staged} vectors ({missed} uuids not in {args.table}); "
          "restart stemma-server to promote into vec0")


if __name__ == "__main__":
    main()
