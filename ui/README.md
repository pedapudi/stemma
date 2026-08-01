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

- **query** — one view, two dialects behind a toggle. *natural*: the top-center
  search bar + trajectory — query line with mention spans wired to candidate
  lanes, per-channel chips (exact / bm25 / trigram / kg), score meters, marked
  document snippets, rejected candidates kept visible with their reject
  reason, and the "spans considered" ledger. example queries are mined from
  the database's own knowledge graph. *sql*: read-only console over `main`
  (the store) + `src` (the user DB) — every query ships with its
  EXPLAIN QUERY PLAN tree (full scans flagged in caution).
- **chat** — a right-hand rail beside the work, not a page of its own: any
  OpenAI-compatible model (`--lm-endpoint http://host:port/v1 --lm-model
  <name>`, bearer via `LM_API_KEY`) is given resolve/sql/schema as tools and
  must pin every mention through stemma before querying. tool calls render
  collapsibly in the rail, and each resolution's trajectory opens in the main
  query view — chat drives the visual.
- **data** — table browser with keyset pagination (no OFFSET degradation on
  big tables) and a substring filter served by the store's trigram index.
- **graph** — the compiled knowledge graph: schema layer, discovered
  relations (inclusion-mined joins, dashed with confidence), and the profile
  layer (frequent values, characteristic terms, co-occurrence). tables click
  through to data; terms and values click through to a query.
- the former store tab lives in the sidebar: size, lexical values, kg
  nodes/edges, embed queue, vector tables — always in view.

## Design

Follows the zicato design language: sixteen terminal-derived themes over
fixed semantic role tokens (default `paper`) and the twelve-face typeface
picker (technical / editorial / display groups, default T9, with the s/m/l
text-size control), both in the top bar with the family's swatch-strip and
true-specimen presentation; sans for prose and controls with mono reserved for data, hairline
borders instead of shadows, one accent color earned by structure. All tokens live in
`static/ui.css`; no hex anywhere else.

## Development

The UI source is TypeScript (`src/ui.ts`), built with the deno binary — **no
npm, no node_modules**:

```sh
ui/build.sh    # deno check + deno bundle → static/ui.js (checked in)
```
