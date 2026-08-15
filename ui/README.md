# stemma console

The optional web UI supports data, metadata, graph, and store queries. Its main
view renders the complete resolution trajectory. The trajectory shows every
considered span, retrieval channel, selected candidate, and near-miss.

Entirely optional: a separate process; nothing in the core depends on it.

## Run

```sh
pip install -r ui/requirements.txt -e clients/python
python ui/serve.py --db mini=eval/mini/data/mini.db --grpc 127.0.0.1:50051
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
- **chat** — a right-hand rail beside the work. Any compatible language
  service configured through `console.lm` is given
  resolve/sql/schema as tools and must pin every mention through stemma before
  querying. Tool calls render collapsibly in the rail. Each resolution appears
  in situ as a compact reasoning trajectory and can open in the main query
  view. Whole-episode approval and rejection controls sit directly under the
  trajectory, preserving the connection between a judgment and the evidence a
  person saw.
- **data** — table browser with keyset pagination (no OFFSET degradation on
  big tables) and a substring filter served by the store's trigram index.
- **graph** — the compiled knowledge graph: schema layer, discovered
  relations (inclusion-mined joins, dashed with confidence), and the profile
  layer (frequent values, characteristic terms, co-occurrence). tables click
  through to data; terms and values click through to a query.
- the former store tab lives in the sidebar: size, lexical values, kg
  nodes/edges, embed queue, vector tables — always in view.

## Design

The console follows the zicato design language. Sixteen terminal-derived themes
map through fixed semantic role tokens and default to `paper`. The top bar also
offers twelve typefaces in technical, editorial, and display groups, plus three
text sizes. Prose and controls use sans faces; data uses monospace. Hairline
borders replace shadows, and structure earns the single accent color. All theme
tokens live in `static/ui.css`; other files contain no color literals.

Feedback controls reuse the trajectory card, border, type scale, and semantic
status tokens. They do not introduce a detached review dashboard. Candidate
selection and graph-and-geometry ambiguity annotations remain designed work;
the current console records approval or rejection for the whole episode.

## Development

The UI source is TypeScript (`src/ui.ts`), built with the deno binary — **no
npm, no node_modules**:

```sh
ui/build.sh    # deno check + deno bundle → static/ui.js (checked in)
```
