# stemma console

The optional web UI: navigate the data, the metadata and knowledge graph,
query the store, and — the centerpiece — watch the full resolution
trajectory: how each span of a natural-language query was considered, which
retrieval channels fired, and every candidate record, chosen and near-miss
alike.

Entirely optional: a separate process; nothing in the core depends on it.

## Run

```sh
pip install -r ui/requirements.txt -e clients/python
python ui/serve.py --db mini=/tmp/mini.db --grpc 127.0.0.1:50051
# → http://127.0.0.1:8600
```

Register the same databases (same names) as the running stemma-server:
browsing reads the SQLite files directly (read-only), resolution goes over
gRPC.

## Views

- **resolve** — the search bar + trajectory: query line with mention spans
  wired to candidate lanes; per-channel chips (exact / bm25 / trigram) with
  ranks; score meters; rejected candidates stay visible with their reject
  reason; the "spans considered" ledger lists everything else that was tried.
- **data** — table browser with pagination, straight from the user DB.
- **graph** — the knowledge graph, schema layer: tables as entities, declared
  foreign keys as relations (the instance layer arrives with stemma-kg).
- **store** — inside the `.stemmadb` sidecar: lexical index stats, model
  registry, embed queue.
- **sql** — read-only console over `main` (the store) + `src` (the user DB).

## Design

Follows the zicato design language: sixteen terminal-derived themes over
fixed semantic role tokens (default `paper`; picker in the top bar), sans
for prose and controls with mono reserved for data, hairline borders instead
of shadows, one accent color earned by structure. All tokens live in
`static/ui.css`; no hex anywhere else.

## Development

The UI source is TypeScript (`src/ui.ts`), built with the deno binary — **no
npm, no node_modules**:

```sh
ui/build.sh    # deno check + deno bundle → static/ui.js (checked in)
```
