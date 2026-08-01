# stemmadb (Python client)

Client library for the stemma resolution engine.

```sh
pip install -e clients/python
```

```python
from stemmadb import StemmaClient, StoreBrowser

# resolution over gRPC (stemma-server must be running)
with StemmaClient("127.0.0.1:50051") as c:
    resp = c.resolve("the Q3 numbers for the Seattle office", database="mini")
    trace = c.explain_dict("what did Chen's team ship", database="mini")

# browsing straight off the SQLite files, read-only — no server needed
b = StoreBrowser("/tmp/mini.db")
b.schema()          # tables, columns, foreign keys, row counts
b.rows("offices")   # paginated rows
b.schema_graph()    # the knowledge graph, schema layer (FK edges)
b.store_meta()      # inside the .stemmadb: lexical index, model registry, queue
b.query("SELECT ... ")  # read-only SQL over store (main) + user DB (src)
```

`StemmaClient.explain*` returns the full resolution trajectory — every span
considered, every candidate with per-channel scores, and why each near-miss
lost. `StoreBrowser` opens everything with `mode=ro`; it cannot write.

Generated gRPC stubs are checked in under `stemmadb/_proto`; regenerate with
`clients/python/gen_protos.sh` after proto changes (requires grpcio-tools).
