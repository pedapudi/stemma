# stemma architecture

stemma resolves oblique natural-language mentions — *the Q3 numbers for the
Seattle office*, *what did Chen's team ship* — to concrete records in a SQLite
database, returning candidate resolutions with the evidence that supports them.
It does **not** generate SQL: its output is the resolution artifact that a
downstream consumer (a query generator, an agent, a human) builds on.

## Why this problem

Error analyses on the BIRD text-to-SQL benchmark attribute the largest share of
failures — roughly a third, though the number varies by system analyzed — to
schema/value linking: finding the right table, column, and stored value, not
SQL logic. BIRD ships human-written "evidence" hints that
pre-solve exactly this linking, and state-of-the-art systems lose >10%
execution accuracy when the hints are removed (DIVER 2026, SEED 2025). The
field's newest systems converge on *resolve-then-generate*: produce a verified
resolution artifact before query generation. stemma is a purpose-built engine
for that artifact.

## Design conclusions from the literature

**Encoders do retrieval; decoders decide among presented options.**
Retrieve-then-rerank pipelines (BLINK → ReFinED → ReLiK lineage) beat
generative entity linking on both accuracy and latency, and constrained
autoregressive decoding over a catalog the model was not trained on carries a
systematic out-of-distribution error floor (SIGIR 2025). Constrained decoding
forces *validity*, not *correctness* — a confidently wrong-but-valid entity is
worse than an explicit no-match. So:

- The **encoder** (bi-encoder embeddings, optional cross-encoder reranker) is
  the workhorse: it embeds serialized rows at index time and mentions at query
  time, powering the dense retrieval channel and final reranking.
- The **LM** is invoked at exactly two points, and only for the ambiguous band:
  1. *Mention expansion* before retrieval ("the crown" → "the British
     monarchy; royal institution") — the single highest-leverage LM use
     (LLMAEL 2025: +8.9% absolute).
  2. *Constrained adjudication* after retrieval: select among k presented
     candidates (JSON-schema output, enum over candidate IDs, explicit NIL).
     LMs are strong selectors and weak open-recall linkers.
- The LM is **never** the retrieval mechanism.

**Candidate generation is always hybrid.** BM25-class lexical retrieval is an
embarrassingly strong baseline (Sparkly, VLDB 2023); dense retrieval catches
semantic mentions with no lexical overlap. Both channels always run, fused by
reciprocal rank fusion.

**Collective disambiguation is the moat.** Multi-hop associative mentions
("Chen's team") are unsolved in text-to-SQL value linking but solved in
collective entity linking (AIDA lineage): score candidate *tuples* jointly by
knowledge-graph coherence — the right "Chen" is the one with an edge to some
team. At query scale (2–4 mentions × ~10 candidates) exhaustive joint scoring
is microseconds.

**The model does not live inside the database.** In-database inference lost
the 2023–2026 natural experiment; the surviving pattern is stateless model
services beside the store, with async queue-driven embedding and versioned
vector tables. A local RPC hop is noise against a model forward pass, and model
lifecycle must not be coupled to database lifecycle.

## Topology

```
                       ┌──────────────────────────────┐
 NL query ──gRPC──►    │  stemma core (Rust, 1 proc)  │
                       │                              │
 resolution +          │  1 span mentions             │      ┌─────────────────┐
 evidence ◄──gRPC──    │  2 candidate gen: FTS5 BM25  │─gRPC►│ Embedder svc    │
                       │    ∪ trigram/spellfix ∪ vec0 │      │ (TEI first;     │
                       │    KNN → RRF fusion (SQL)    │      │  modular)       │
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
lexical indexes (FTS5/trigram/spellfix), vector tables (sqlite-vec `vec0`),
the compiled knowledge store, the embed queue, and the model registry. The
user's database is attached read-only and never modified — the strongest form
of "minimal surgery within SQLite". SQLite itself is stock; capability comes
from core modules plus the statically linked sqlite-vec extension registered
through `sqlite3_auto_extension`.

## Modularity contracts

Every model- or store-shaped dependency sits behind a trait with a registry,
so backends substitute without touching resolution code:

- **`KnowledgeStore`** (stemma-kg): upsert nodes/edges, alias→node lookup,
  neighbors, bounded path search, subgraph extraction. First backend: SQLite
  simple-graph tables in the `.stemmadb` store with recursive-CTE traversal.
  Graph-traversal SQL never leaks outside this backend, so a dedicated graph
  store can substitute later.
- **`Embedder`** (stemma-embed): embed batch, rerank, model identity. First
  backend: TEI over gRPC; in-process ONNX, OpenAI-compatible `/v1/embeddings`,
  or hosted endpoints slot in behind the same trait. The model registry in
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
2. **Generate candidates** — exact/normalized match → FTS5 BM25 →
   trigram/spellfix fuzzy → `vec0` KNN over serialized-row embeddings; fused
   with reciprocal rank fusion in SQL. Score-band routing: auto-accept
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
later stress test for abbreviation-heavy schemas.

## Build

Bazel (bzlmod). The Cargo workspace is the dependency manifest — crate_universe
consumes `Cargo.toml`/`Cargo.lock` — and `cargo test` stays usable for fast
iteration. Proto codegen is currently a checked-in artifact refreshed by
`tools/regen_protos.sh` (see `proto/` for the source of truth); wiring the
prost/tonic toolchain into Bazel directly is future work.
