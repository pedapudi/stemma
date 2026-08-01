# stemma documentation

## Start here

- [Architecture](architecture.md) — the literature review and the design it
  produced: why entity/value resolution is the product, encoder vs decoder
  roles, and the "resolution engine beside a stock SQLite database" topology.
- [Walkthrough](walkthrough.md) — build it, load a corpus, query the Resolve
  API, look inside the `.stemmadb` store. Verified commands with expected
  output.

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

## For LLM agents

The [skills/](../skills) directory contains task-oriented guides written for
LLM coding agents (and equally usable by humans):

- `stemmadb-setup` — stand up stemmadb from a bare machine to a served corpus.
- `stemmadb-corpus` — turn arbitrary source data into a stemma-ready user DB.
- `stemmadb-eval` — run and extend the evaluation harness.
- `stemmadb-contribute` — repo invariants, build-system rules, and change
  recipes for contributing code.
