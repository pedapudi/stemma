# Concepts

## The problem stemma solves

A natural-language question names things obliquely — by nickname,
abbreviation, description, or association:

> *the Q3 numbers for the Seattle office* · *what did Chen's team ship* ·
> *the crown's holdings*

Before any query can run, each mention must be pinned to an actual record:
"the Seattle office" → `offices` rowid 17, whose stored name might be
`'Seattle - Northgate'`. stemma does that pinning — it **spans** the mentions,
**links** them to candidate records, and returns a **resolution with
evidence**. It deliberately does *not* generate SQL: the resolution artifact
is useful to any downstream consumer (a text-to-SQL system, an agent, a
human), and measured evidence says this linking step — not SQL logic — is
where natural-language database interfaces actually fail.

## Two files, one boundary

| | user database | `.stemmadb` store |
|---|---|---|
| example | `careg.db` | `careg.stemmadb` |
| owned by | you | stemmadb |
| contents | your tables | derived indexes, knowledge store, queues, registries |
| mutability | attached **read-only** | read-write |
| deletable? | it's your data | always — fully rebuildable |

Both are ordinary SQLite files. stemma never modifies the user database, and
SQLite itself is stock — the vector extension (sqlite-vec) is statically
linked and registered through the sanctioned auto-extension mechanism.

## The resolution pipeline

A cheap-first cascade; each stage narrows what the next must consider:

1. **Span** — find mention boundaries, using alias/n-gram matching plus typed
   span detection with types derived from the schema. Spans stay soft:
   alternate segmentations are carried forward rather than decided early.
2. **Candidate generation** — always hybrid: exact/normalized match, FTS5
   BM25, trigram/spellfix fuzzy match, and vector KNN over serialized rows,
   fused with reciprocal rank fusion. Unambiguous exact matches are accepted
   immediately; junk is rejected; the ambiguous middle band continues.
3. **Collective disambiguation** — candidates for co-occurring mentions are
   scored *jointly* using knowledge-graph coherence: the right "Chen" for
   *Chen's team* is the person with an edge to some team. Live verification
   probes (`SELECT DISTINCT`, `LIKE`) run against the read-only user DB.
4. **LM adjudication** — only for what is still ambiguous: a language model
   chooses among the presented candidates (constrained output, explicit
   "none of the above"). The LM is never the retrieval mechanism.
5. **Resolution** — spans, ranked candidates, and the evidence for each:
   matched aliases, KG paths, probe results, adjudication rationale.

The pipeline is recall-biased: output is a *candidate set*, because a missed
record is unrecoverable downstream while an extra candidate is merely noise.

## Modularity contracts

Three trait-plus-registry seams let backends substitute without touching
resolution code:

- **KnowledgeStore** (stemma-kg): the KG API (alias lookup, neighbors, bounded
  path search, subgraph extraction). First backend: SQLite tables +
  recursive CTEs inside the store. A dedicated graph database can replace it.
- **Embedder** (stemma-embed): embed/rerank/model-identity. First backend:
  TEI over gRPC; in-process ONNX or hosted endpoints slot in.
- **LM backends** (stemma-lm): normalized request/response with
  registry-by-model-string. The OpenAI-compatible chat-completions client
  covers vLLM, llama.cpp, LiteLLM, and Vertex's compatibility endpoint in one
  implementation.

The **model registry** in the store records which backend/model/revision
produced every vector table. Changing embedders triggers a blue-green
re-embed into a new table — vector spaces are never silently mixed.

## Status: what exists today

| Component | Status |
|---|---|
| Bazel/Cargo dual build, all tests | ✅ |
| stemmadb storage layer (open/attach/extensions/store schema) | ✅ |
| Resolve + Embedder gRPC APIs (protos, generated code) | ✅ |
| stemma-server (registers DBs, serves Resolve) | ✅ skeleton — returns empty resolutions |
| Eval harness (BIRD gold-SQL target derivation) | ✅ |
| CA Code of Regulations corpus builder | ✅ |
| Lexical resolution (FTS5/trigram/RRF) | milestone 2 |
| Dense channel (embedder, vec0, embed queue drain) | milestone 3 |
| Knowledge store + collective disambiguation | milestone 4 |
| LM band (expansion, adjudication) | milestone 5 |

The milestone plan with acceptance criteria is in
[architecture.md](../architecture.md).
