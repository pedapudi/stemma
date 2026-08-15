# stemma architecture

stemma is a grounding-first semantic parser. It resolves oblique
natural-language mentions — *the Q3 numbers for the Seattle office*, *what did
Chen's team ship* — to concrete SQLite records and returns candidate readings
with evidence. Parsing builds on that trace to produce a grounded,
parameterized, read-only query; it never bypasses unresolved grounding.

## Why this problem

BIRD [J. Li 2023] ships human-written "evidence" hints that pre-solve schema
and value linking — finding the right table, column, and stored value.
Remove them and state-of-the-art systems collapse: more than 10 points of
execution accuracy (CodeS-7B 57.17→45.24 [Nan 2026]), or 8.35–20.86 points
across systems [Yun 2025]. Only 5 of 52 BIRD leaderboard methods report
no-evidence numbers at all [Nan 2026].

Published error analyses point the same way but less precisely: one
attributes 37% of its BIRD-dev errors to schema linking, defined to include
incorrect tables, columns or values [C. Li 2025], while others range from
20% to 57% depending on taxonomy and denominator [D. Lee 2025]. Treat that
as evidence about where failures concentrate, not as a constant.

The field's newest systems converge on *resolve-then-generate*: produce a
verified resolution artifact before query generation. stemma makes that
artifact the measured first stage of its parser. Full citations, with the
caveat on each number, are
in [docs/design/00-bibliography.md](design/00-bibliography.md).

## Design conclusions from the literature

**Encoders do retrieval; decoders decide among presented options.**
Retrieve-then-rerank pipelines — the BLINK [Wu 2020] → ReFinED
[Ayoola 2022] → ReLiK [Orlando 2024] lineage — beat generative entity
linking [De Cao 2021] on both accuracy and latency, and constrained
autoregressive decoding over a catalog the model was not trained on carries
a systematic out-of-distribution error floor [S. Wu 2025]. Constrained
decoding forces *validity*, not *correctness* — a confidently
wrong-but-valid entity is worse than an explicit no-match. So:

- The **encoder** (bi-encoder embeddings, optional cross-encoder reranker) is
  the workhorse: it embeds serialized rows at index time and mentions at query
  time, powering the dense retrieval channel and final reranking.
- The **LM** is invoked at exactly two points, and only for the ambiguous band:
  1. *Mention expansion* before retrieval ("the crown" → "the British
     monarchy; royal institution") — the single highest-leverage LM use
     (+8.9% absolute on average across linkers [Xin 2025]).
  2. *Constrained adjudication* after retrieval: select among k presented
     candidates (JSON-schema output, enum over candidate IDs, explicit NIL).
     LMs select well among presented options and recall poorly over open
     catalogs [D. Lee 2025].
- The LM is **never** the retrieval mechanism.

**Candidate generation is always hybrid.** BM25-class lexical retrieval is an
embarrassingly strong baseline for entity matching [Paulsen 2023] and for
zero-shot retrieval generally [Thakur 2021]; dense retrieval catches
semantic mentions with no lexical overlap. Both channels always run, fused
by reciprocal rank fusion [Cormack 2009].

**Collective disambiguation is the moat.** Multi-hop associative mentions
("Chen's team") are unsolved in text-to-SQL value linking but solved in
collective entity linking [Hoffart 2011; Phan 2019]: score candidate
*tuples* jointly by knowledge-graph coherence — the right "Chen" is the one
with an edge to some team. At query scale (2–4 mentions × ~10 candidates)
exhaustive joint scoring is microseconds.

**The model does not live inside the database.** In-database inference lost
the 2023–2026 natural experiment; the surviving pattern is stateless model
services beside the store, with async queue-driven embedding and versioned
vector tables (the argument, with the case studies, in
[design/01-architecture.md](design/01-architecture.md)). A local RPC hop is
noise against a model forward pass, and model lifecycle must not be coupled
to database lifecycle.

Bracketed citations resolve in the
[shared bibliography](design/00-bibliography.md).

## Topology

Diagrammed, with the pipeline, store anatomy and chat flow, in
[architecture-visuals.md](architecture-visuals.md).

```
                       ┌──────────────────────────────┐
 NL query ──gRPC──►    │  stemma core (Rust, 1 proc)  │
                       │                              │
 resolution +          │  1 span mentions             │      ┌─────────────────┐
 evidence ◄──gRPC──    │  2 candidate gen: FTS5 BM25  │─HTTP►│ Embedding svc   │
                       │    ∪ trigram ∪ vec0 KNN      │(OAI  │ (openai-compat: │
                       │    → RRF fusion (SQL)        │compat│  vllm, li,...)  │
                       │  3 KG-coherence rerank +     │      └─────────────────┘
                       │    live verification probes  │      ┌─────────────────┐
                       │  4 LM adjudication           │─HTTP►│ OpenAI-compat   │
                       │    (ambiguous band only)     │(OAI  │ endpoint: vLLM, │
                       │                              │compat│ llama.cpp,      │
                       │  stemmadb layer: user.db     │proto)│ LiteLLM, Vertex │
                       │  (stock, read-only) +        │      └─────────────────┘
                       │  ATTACHed <name>.stemmadb    │
                       └──────────────────────────────┘
```

Naming: the repository and ecosystem are **stemma**; the core data-and-metadata
storage layer is **stemmadb** — the `stemmadb` crate plus the sidecar
`.stemmadb` file (itself a SQLite database) holding every derived artifact:
lexical indexes (FTS5 unicode61 and trigram), vector tables (sqlite-vec `vec0`),
the compiled knowledge store, the embed queue, and the model registry. The
user's database is attached read-only and never modified — the strongest form
of "minimal surgery within SQLite". SQLite itself is stock; capability comes
from core modules plus the statically linked sqlite-vec extension registered
through `sqlite3_auto_extension`.

An optional USearch file is a rebuildable dense-candidate
accelerator for larger scopes. SQLite remains authoritative and stores the
generation receipt. Invalid files fall back to exact SQLite search.
Approximate results map to SQLite identities and receive exact rescoring.
Every span that could become a mention retains an exact-search safeguard. The
operator contract is in
[design/03-resolution.md](design/03-resolution.md#optional-approximate-vector-sidecar).

## Modularity contracts

Every model- or store-shaped dependency sits behind a trait with a registry,
so backends substitute without touching resolution code:

- **`KnowledgeStore`** (stemma-kg): upsert nodes/edges, alias→node lookup,
  neighbors, bounded path search, subgraph extraction. First backend: SQLite
  simple-graph tables in the `.stemmadb` store with recursive-CTE traversal.
  Graph-traversal SQL never leaks outside this backend, so a dedicated graph
  store can substitute later.
- **`Embedder`** (stemma-embed): embed batch, model identity. First backend
  (built): the OpenAI-compatible `/v1/embeddings` client, which covers vLLM,
  llama.cpp and LiteLLM in one implementation; TEI-native or in-process ONNX
  backends slot in behind the same trait. The model registry in
  stemmadb records `(backend, model, revision, dim, quantization)` per vector
  table; an embedder change triggers a blue-green re-embed, never a silent mix
  of vector spaces.
- **LM backends** (stemma-lm): a backend trait with request/response
  normalization and registry-by-model-string (modeled on agent-framework model
  layers). The primary backend speaks the OpenAI-compatible chat-completions
  protocol, which covers vLLM, llama.cpp, LiteLLM proxies, and Vertex's
  compatibility endpoint with one implementation; native backends register in
  the same registry. Structured output (JSON-schema / enum over candidate IDs)
  is a capability flag with validate-and-retry fallback.

## Resolution pipeline

Cheap-first cascade, recall-biased, candidate-set output:

1. **Span** — alias-table n-gram matching plus typed open-vocabulary span
   detection (types derived from the schema/KG). Spans stay soft; alternate
   segmentations are carried forward.
2. **Generate candidates** — exact/normalized match → FTS5 BM25 → trigram
   fuzzy → targeted `vec0` KNN; fused with reciprocal rank fusion. Score-band routing: auto-accept
   unambiguous exact matches, auto-reject junk, continue with the middle band.
3. **Disambiguate collectively** — joint scoring of candidate tuples: local
   score + type compatibility + prior + pairwise KG-coherence (bounded path
   search between candidates). Live verification probes (`SELECT DISTINCT`,
   `LIKE`) run against the read-only user DB.
4. **Adjudicate** (ambiguous band only) — LM selects among k with the evidence
   attached, constrained output, explicit NIL.
5. **Return** — spans, ranked candidates, and evidence: matched aliases,
   KG paths, probe results, adjudication rationale.

## Evaluation

BIRD dev in the **no-evidence** setting, consumed directly (BIRD databases are
SQLite). Ground truth is derived from gold SQL rather than hand-labeled:
literals in WHERE-class predicates are value targets; referenced tables are
schema targets; the shipped human evidence is the reference for an
evidence-reconstruction score. Metrics are recall-weighted (a missed record is
unrecoverable downstream; an extra candidate is noise). KaggleDBQA is the
later stress test for abbreviation-heavy schemas. The full protocol is
[design/06-evaluation.md](design/06-evaluation.md) and the runnable harness
design is [design/07-eval-harness.md](design/07-eval-harness.md).

## Build

Bazel (bzlmod). The Cargo workspace is the dependency manifest — crate_universe
consumes `Cargo.toml`/`Cargo.lock` — and `cargo test` stays usable for fast
iteration. Proto codegen is currently a checked-in artifact refreshed by
`tools/regen_protos.sh` (see `proto/` for the source of truth); wiring the
prost/tonic toolchain into Bazel directly is future work.
