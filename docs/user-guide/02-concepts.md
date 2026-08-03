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
human), and the published error analyses say this linking step — not SQL
logic — is where natural-language database interfaces actually fail (the
numbers, with their caveats, in
[architecture.md](../architecture.md#why-this-problem) and the
[bibliography](../design/00-bibliography.md)).

## Two files, one boundary

| | user database | `.stemmadb` store |
|---|---|---|
| example | `careg.db` | `careg.stemmadb` |
| owned by | you | stemmadb |
| contents | your tables | derived indexes, knowledge store, queues, registries |
| mutability | attached **read-only** | read-write |
| deletable? | it's your data | always — fully rebuildable\* |

Both are ordinary SQLite files. stemma never modifies the user database, and
SQLite itself is stock — the vector extension (sqlite-vec) is statically
linked and registered through the sanctioned auto-extension mechanism.

\* One caveat: the store also carries operational history (`query_log`,
`chat_log`), which is yours and *not* rebuildable — back it up like data if
the history matters.

## The resolution pipeline

A cheap-first cascade; each stage narrows what the next must consider. All
of this is built and running; the full specification with the real
constants is [design/03-resolution.md](../design/03-resolution.md).

1. **Span** — every n-gram up to four tokens is a potential mention, plus
   the whole query when an embedder is configured (semantic mentions have
   no n-gram boundary). Spans stay soft: alternate segmentations coexist
   until evidence decides.
2. **Candidate generation** — always hybrid: exact/normalized match, FTS5
   BM25, trigram fuzzy match, and targeted vector KNN, fused with
   reciprocal rank fusion. A dense cosine additionally floors the fused
   score after calibration — semantic evidence survives having no lexical
   company.
3. **Collective disambiguation** — candidates for co-occurring mentions are
   scored *jointly*: a pair whose tables connect through a foreign-key path
   is verified with a probe against the read-only user DB, and the winning
   tuple carries the path as evidence — the right "Chen" for *Chen's
   Billing team* is the one who leads it.
4. **LM adjudication** — only for near-ties the channels could not order: a
   language model chooses among the presented candidates (constrained
   output, explicit "none of the above"). The LM is never the retrieval
   mechanism.
5. **Resolution** — spans, ranked candidates, and the evidence for each:
   channels with scores, snippets, knowledge-graph paths, adjudication
   marks. The console renders all of it as the trajectory.

The pipeline is recall-biased: output is a *candidate set*, because a missed
record is unrecoverable downstream while an extra candidate is merely noise
(the argument in [design/06-evaluation.md](../design/06-evaluation.md#metrics)).

## Modularity contracts

Three trait-plus-registry seams let backends substitute without touching
resolution code:

- **KnowledgeStore** (stemma-kg): the KG API (alias lookup, neighbors,
  bounded path search between tables, subgraph extraction). First backend:
  SQLite tables + recursive CTEs inside the store. A dedicated graph
  database can replace it.
- **Embedder** (stemma-embed): batch embedding + model identity. First
  backend: the OpenAI-compatible `/v1/embeddings` client (vLLM, llama.cpp,
  LiteLLM in one implementation).
- **LM backends** (stemma-lm): chat completion with optional structured
  output, registry-by-model-string. The OpenAI-compatible client covers
  vLLM, llama.cpp, LiteLLM, and hosted compatibility endpoints.

The **model registry** in the store records which backend/model produced
every vector table. Vector spaces from different models are never mixed —
a mismatched identity is a hard error, not a warning.

## Status: what exists today

| Component | Status |
|---|---|
| Bazel/Cargo dual build, all tests | ✅ |
| stemmadb storage layer (store schema v4, migrations, history) | ✅ |
| Lexical resolution (exact/FTS5/trigram + RRF) | ✅ |
| Dense channel (openai-compat embedder, vec0, queue drain) | ✅ |
| Knowledge graph (terms, phrases, joins, centrality) + coherence | ✅ |
| Collective disambiguation over join paths | ✅ |
| LM adjudication band (`allow_lm`) | ✅ |
| MCP server, reference agent, console with trajectories | ✅ |
| Mention expansion | designed ([design/05](../design/05-encoders-decoders.md)) |
| Evaluation harness | designed ([design/07](../design/07-eval-harness.md)) |

The deep reference for each piece is [docs/design/](../design/README.md).
