# stemma documentation

## Start here

- [Architecture](architecture.md) — the literature review and the design it
  produced: why entity/value resolution is the product, encoder vs decoder
  roles, and the "resolution engine beside a stock SQLite database" topology.
- [Walkthrough](walkthrough.md) — build it, load a corpus, query the Resolve
  API, look inside the `.stemmadb` store. Verified commands with expected
  output.
- [Architecture, visually](architecture-visuals.md) — the topology, the
  resolution pipeline, the store anatomy and one chat turn as diagrams, with
  a walkthrough of every component and edge.

## User guide

1. [Setup](user-guide/01-setup.md) — prerequisites, building with Bazel or
   Cargo, running the tests.
2. [Concepts](user-guide/02-concepts.md) — user DB vs `.stemmadb` store, the
   resolution pipeline, evidence, and what exists today vs what's planned.
3. [Running the server](user-guide/03-server.md) — stemma-server flags, the
   gRPC Resolve API, calling it with grpcurl.
4. [Corpora](user-guide/04-corpora.md) — the bundled test corpus, the
   California Code of Regulations corpus, BIRD, and how to build your own.

## Component docs

- [stemmadb](../crates/stemmadb/README.md) — the storage layer: file layout,
  store schema, invariants.
- [stemma console](../ui/README.md) — the optional web UI: data/metadata/KG
  navigation, read-only SQL, and the visual resolution trajectory.
- [Python client](../clients/python/README.md) — `StemmaClient` (gRPC
  resolve/explain) and `StoreBrowser` (read-only file access).
- [MCP server](../integrations/mcp/README.md) — stemma's tools for any MCP
  client, trajectories included.
- [Reference agent](../agents/stemma_agent/README.md) — the ADK example the
  console's chat is built on.

## Design

- [Technical design](design/README.md) — the deep reference: system
  decomposition and boundaries, the full store schema and migration
  discipline, the resolution pipeline with its actual constants and scoring
  mathematics, the knowledge compiler's algorithms, the designed dense
  channel and LM band, and the evaluation protocol. Includes a shared
  [bibliography](design/00-bibliography.md).
- [Brand](brand.md) — the mark and its grammar, the wordmark, the one color
  rule (`currentColor` strokes, the dot is the only colored element), clear
  space and minimum sizes, and the voice/microcopy conventions. Assets live in
  [`assets/brand/`](../assets/brand).

## For LLM agents

The [skills/](../skills) directory contains task-oriented guides written for
LLM coding agents (and equally usable by humans):

- `stemmadb-setup` — stand up stemmadb from a bare machine to a served corpus.
- `stemmadb-corpus` — turn arbitrary source data into a stemma-ready user DB.
- `stemmadb-eval` — run and extend the evaluation harness.
- `stemmadb-contribute` — repo invariants, build-system rules, and change
  recipes for contributing code.
