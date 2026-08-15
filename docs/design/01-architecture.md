# Architecture

stemma is a grounding-first semantic parser. Its built resolution engine pins
entity and value mentions in a natural-language question to concrete SQLite
records and returns an inspectable evidence trace. The staged parser will
consume that trace and produce a grounded, parameterized, read-only query.
Grounding remains independently useful and independently measurable.

This document specifies the system decomposition, the process topology, the
ownership and trust boundaries, the trait seams that make backends
substitutable, the two external surfaces (gRPC and MCP), and the argument for
the shape: a resolution engine *beside* a stock database rather than inside
it, and a purpose-built resolver rather than a stack of general-purpose
layers.

It goes deeper than [`docs/architecture.md`](../architecture.md), which
remains the shorter statement of the same design. Where this document
describes something that is designed but not built, it says so.

## The problem this shape is for

A question names things obliquely — *the Q3 numbers for the Seattle office*,
*what did Chen's team ship*, *the crown's holdings*. Before any query can
run, each mention has to become a record: `offices` rowid 17, whose stored
name is `'Seattle - Northgate'`.

The measurement that carries the argument is an ablation, not an error
taxonomy. BIRD [J. Li 2023] ships human-written "evidence" hints alongside
each question — hints that pre-solve exactly the linking step. Remove them and
the loss is large and directly measured on the benchmark's own metric:
execution accuracy collapses by more than 10 points (57.17 → 45.24 and
55.02 → 43.42) [Nan 2026]. A second study puts the cost at 8.35 to 20.86
points across systems (65.78 → 54.50 and 56.32 → 35.46), with automatically
generated evidence recovering much of it [Yun 2025]. Only 5 of 52 BIRD methods report
no-evidence numbers at all [Nan 2026]. Spider 2.0's enterprise databases,
often exceeding a thousand columns, make the linking step harder still
[Lei 2025].

Error taxonomies point the same way but are a weaker instrument, and this
document set does not lean on them. One published analysis attributes 37% of
its BIRD-dev errors to schema linking, defined to include incorrect tables,
columns **or values** [C. Li 2025]; others put the figure anywhere from 20% to
57% depending on taxonomy and denominator [D. Lee 2025]. No published work
reports "value linking" as a category with its own percentage — the closest is
a 24% "Value Misrepresentation" rate against an unstated denominator
[Qu 2024]. The spread is a disagreement about how to count, not about what is
failing. See
[00-bibliography.md](00-bibliography.md#notes-on-contested-claims).

The field's newest systems converge on *resolve-then-generate*: produce a
verified linking artifact, then generate against it [Talaei 2024].
stemma is a purpose-built engine for that artifact, extracted as a component
rather than embedded in a generator.

The honest counterargument is [Maamari 2024], often summarized as
"schema linking is dead": with sufficiently strong reasoning models, *pruning*
the schema before generation can lose more recall than the saved context is
worth. Note the scope. That is an argument against a filtering step, and
stemma is not a filter — it does not remove anything from the generator's
view. It answers a different question: *which stored record does this phrase
denote*. A model with a million-token context still cannot tell you that "the
Seattle office" is row 17 without looking, and looking is retrieval.

Extraction is the design decision that everything else follows from. A
linking artifact that is a component has to be *inspectable* (the consumer
did not compute it and must be able to check it), *recall-biased* (a missed
record is unrecoverable downstream; an extra candidate is noise), and
*evidence-carrying* (a bare ranked list is not checkable). Those three
requirements produce the trace model, the candidate-set output, and the
`Evidence` union, respectively.

## System decomposition

Ten Rust crates, one Python client, two integrations, one optional UI.

```
stemma-server
├── stemma-resolve ── stemma-kg · stemma-embed · stemma-lm · stemmadb
├── stemma-parse ──── stemma-resolve · stemma-lm · stemmadb
├── stemma-ingest ─── stemmadb
└── stemma-proto ◄─── proto/ (source of truth)

stemmadb ── rusqlite + sqlite-vec
```

| Crate | Role | Depends on |
|---|---|---|
| [`stemmadb`](../../crates/stemmadb) | Storage layer: opens the store, attaches the user DB read-only, registers sqlite-vec, owns the versioned store schema | rusqlite |
| [`stemma-ingest`](../../crates/stemma-ingest) | Builds the lexical index (`lex_values`, `lex_fts`, `lex_trigram`) from the attached user DB | stemmadb |
| [`stemma-kg`](../../crates/stemma-kg) | `KnowledgeStore` trait, SQLite backend, the layered knowledge compiler | stemmadb |
| [`stemma-resolve`](../../crates/stemma-resolve) | Resolution pipeline, optional approximate dense retrieval, `Trace`, and protocol projection | stemmadb, ingest, KG, embedder, language service, proto |
| [`stemma-parse`](../../crates/stemma-parse) | Bounded query proposal and deterministic validation into a grounded SQLite syntax tree | resolver, language service, stemmadb |
| [`stemma-server`](../../crates/stemma-server) | gRPC service, database registry, startup maintenance, episodes, feedback, and parsing | workspace crates, tonic, tokio |
| [`stemma-proto`](../../crates/stemma-proto) | Generated prost/tonic types, checked in under `src/gen` | prost, tonic |
| [`stemma-eval`](../../crates/stemma-eval) | Gold-target derivation, ablation runs, grading, paired statistics, and report data | resolver, sqlparser |
| [`stemma-embed`](../../crates/stemma-embed) | `Embedder` trait, configured remote backend, cooldown behavior, and query formatter | serde, ureq |
| [`stemma-lm`](../../crates/stemma-lm) | Optional bounded language-service trait, remote backend, structured output, and validated retry | serde, ureq |

Outside the workspace:

| Component | Role |
|---|---|
| [`clients/python/stemmadb`](../../clients/python/stemmadb) | `StemmaClient` (resolution, parsing, feedback) and `StoreBrowser` (direct read-only SQLite access) |
| [`integrations/mcp`](../../integrations/mcp) | MCP server exposing `resolve`, `sql`, `schema`, `knowledge_graph` |
| [`agents/stemma_agent`](../../agents/stemma_agent) | Reference tool-using agent — the smallest complete consumer |
| [`ui/`](../../ui) | Optional console: data browser, chat, resolution trajectory, and episode feedback |
| [`eval/`](../../eval) | Corpus builders (careg, eCFR, combined legal) and the BIRD fetcher |

**Two structural notes.** First, `stemma-resolve` depends on `stemma-kg` for
collective path search. Mention detection, term coherence, and context
coherence still query `kg_nodes` and `kg_edges` directly through `stemmadb`.
That exception to the `KnowledgeStore` boundary is
[documented with its removal condition](04-knowledge-graph.md#a-note-on-the-layering-exception).

Second, `stemma-parse` consumes the resolver's public trace structures and
produces a validated SQLite syntax tree. It does not introduce a second
grounding representation. The server coordinates proposal, validation,
episode persistence, and protocol projection without merging parser logic into
the resolution pipeline.

### Build

Bazel (bzlmod) is the build system; the Cargo workspace doubles as the
dependency manifest, consumed by `crate_universe`, so `cargo test` stays
usable for fast iteration and `bazel test //...` is the gate. Proto codegen
is a checked-in artifact under `crates/stemma-proto/src/gen`, refreshed by
[`tools/regen_protos.sh`](../../tools/regen_protos.sh) — `proto/` is the
source of truth. Wiring the prost/tonic toolchain into Bazel directly is
future work. sqlite-vec and the SQLite headers are vendored under
[`third_party/`](../../third_party) and statically linked.

The native approximate vector dependency is an explicit Cargo feature. Build
the server with `--features usearch-sidecar` before selecting
`dense_search = "usearch"`. The generated native bridge is not yet compatible
with Bazel's read-only external-repository output path, so Bazel retains the
exact backend. A Bazel-built server rejects approximate mode at startup.

## Process topology

Today, in the minimum useful configuration, there is exactly **one process**:

```
┌──────────────────────────────────────────────────────────┐
│ stemma-server (tokio + tonic)                            │
│   --listen 127.0.0.1:50051                               │
│   --db legal=eval/legal/data/legal.db  (repeatable)      │
│   --embed-endpoint http://host:8081/v1  --embed-model M  │
│                                                          │
│   per database: Mutex<StemmaDb>                          │
│     main = legal.stemmadb  (rw)                          │
│     src  = legal.db        (ro, ATTACHed)                │
│     approximate index = optional derived file            │
│                                                          │
│   startup: build_lexical_index() → kg::compile()         │
│            → build_dense_index()  (promote vec_staging)  │
│            → rebuild approximate index (when selected)   │
│   serving: Resolve, Explain, Parse, feedback             │
└──────────────────────────────────────────────────────────┘
```

Around it, all optional and all separate processes:

```
   ui/serve.py ──HTTP──► browser
       │  ├─ gRPC ──────────────────► stemma-server        (resolve/explain)
       │  └─ direct file read (ro) ──► *.db, *.stemmadb    (browse/SQL/graph)
       │  └─ writes chat_log ────────► *.stemmadb          (the one outside write)
       │
   reference agent (in-process with ui, or standalone)
       └─ stdio ──► stemmadb_mcp.py ──┬─ gRPC ──► stemma-server
                    (MCP server)      └─ direct file read (ro)
       └─ HTTP ──► configured language service

   stemma-server ──HTTP──► configured embedding service
                 └─HTTP──► configured language service

   load_vectors.py ──► writes vec_staging into *.stemmadb (server stopped)
```

Six properties of this topology are load-bearing.

**Browsing does not go through the server.** `StoreBrowser` opens both SQLite
files directly with `mode=ro`. Listing tables, paginating rows, running a
read-only `SELECT`, reading the knowledge graph, and reading the store's
metadata are all storage-layer concerns that need no RPC and no running
server. The console works against a corpus whose server is down; only
resolution needs the server. This is a direct consequence of the storage
model — everything stemma knows is in two plain SQLite files.

**The MCP server is a child of its client**, launched over stdio with its
server address and database registrations as command-line arguments
(`--grpc`, `--db name=path`, or `--config`). It holds no state of its own: a
`StemmaClient` for resolution and a `StoreBrowser` per database for
everything else.

**Model services are out of process.** The server reaches the configured
embedding service for query vectors and queue drains. Without an embedding
service, lexical and graph resolution remain available. The server reaches the
optional language service for bounded mention expansion, adjudication, and
query proposals. The console's reference agent can use a separately configured
language service. Service failures degrade only the stages that need them.

**Vector loading is an offline step.** External vectors are staged into the
store by a loader that does not need the sqlite-vec extension, and promoted
into a `vec0` table by the server at startup. The loader runs with the server
stopped. See
[02-data-model.md](02-data-model.md#vec_staging-and-vec_dense).

**Approximate vector search is a derived accelerator.** The optional USearch
file proposes dense candidates and never becomes a second vector authority.
SQLite retains vectors, source identities, model metadata, graph evidence,
feedback, and the receipt that binds the file to one vector generation. A
missing or invalid file leaves the exact SQLite path available. Resolution
also retains exact-search safeguards for ambiguity-sensitive decisions. The
full lifecycle and deployment gate are specified in
[03-resolution.md](03-resolution.md#optional-approximate-vector-sidecar).

**Resolution is serialized per database.** `rusqlite::Connection` is not
`Sync`, so each registered database sits behind a `Mutex`. This is
acknowledged in the code as a skeleton decision to revisit with a connection
pool. Since spans are independent, it is also what currently blocks the
obvious parallelization of the pipeline — and it now also serializes the
blocking HTTP call to the embedding endpoint, which happens inside the lock.

## Ownership and trust boundaries

### The read-only boundary

```rust
let uri = format!("file:{}?mode=ro", user_db_path…);
conn.execute("ATTACH DATABASE ?1 AS src", params![uri])?;
```

The user's database is attached read-only and is **never** written. Not "not
written by convention" — the `mode=ro` URI makes writes fail in SQLite's VFS,
so a bug in stemma cannot mutate user data. A test asserts the failure.

This is the strongest available form of "minimal surgery within SQLite": the
user's file keeps its original bytes, keeps working with every other SQLite
tool, and can be replaced underneath stemma at any time. It also means stemma
can be pointed at a database it has no write permission on at all.

### The sidecar-state boundary

The sidecar `.stemmadb` file contains derived state: lexical indexes, the
compiled knowledge store, the embed queue, and the model registry. It also
contains operational history and explicit feedback. Deleting the sidecar does
not affect the attached user database, but it permanently removes that history
and feedback. Operators must apply their backup and retention policy before
deletion.

The approximate vector file is separate from `.stemmadb` and is
fully rebuildable. Its validity depends on a receipt stored in SQLite. Moving
or restoring the approximate file without the matching SQLite store cannot
activate it. Deleting only the approximate file changes performance and leaves
resolution available through exact search.

The rebuildable portion makes several decisions affordable:

- Schema shape changes in derived tables are handled by *drop and rebuild*
  rather than by migration ([02-data-model.md](02-data-model.md#shape-change-self-healing-below-the-version)).
- The knowledge compiler is versioned by a string prefix on its fingerprints,
  so algorithm changes recompile without any migration machinery
  ([04-knowledge-graph.md](04-knowledge-graph.md#the-fingerprint)).
- A store from a *newer* build is a hard error with a "re-ingest" message,
  because re-ingesting costs minutes and correctness costs more.

Operational history (`query_log`, `chat_log`, and `grounding_feedback`) makes
the sidecar more than a cache. A rebuild restores derived indexes and graph
state. It cannot restore prior queries, conversations, or judgments.

### The write boundary

| Writer | Writes | Via |
|---|---|---|
| `stemma-server` | indexes, KG, `query_log`, `grounding_feedback` | `StemmaDb` (rw) |
| `stemma-server` | optional approximate vector file | deterministic build from SQLite vectors |
| `ui/agent_backend.py` | `chat_log` | direct `sqlite3` connection (rw) |
| everything else | nothing | `mode=ro` |

The console's `chat_log` write is the only store write from outside the Rust
core. It is sanctioned — the console owns conversational memory — and it is
safe because the store runs in WAL mode, but it is the one place where two
processes hold write capability on the same file.

### The trust boundary for SQL

The Python `StoreBrowser.query()` and `query_plan()` accept arbitrary SQL and
guard it two ways: a prefix allowlist (`select`, `with`, `explain`, `values`,
`pragma`) for a friendly early error, and `mode=ro` connections underneath so
that writes fail at the SQLite level regardless. The comment in the code is
explicit that the prefix check is ergonomics, not the security boundary — the
read-only connection is. That ordering matters: a prefix check alone is
defeated by a CTE, and a read-only connection alone gives an ugly error.

## Trait seams

Three model- or store-shaped dependencies sit behind traits with registries,
so backends substitute without touching resolution code.

### `KnowledgeStore` — built

```rust
pub trait KnowledgeStore {
    fn upsert_node(&self, node: &Node) -> Result<()>;
    fn upsert_edge(&self, edge: &Edge) -> Result<()>;
    fn remove_by_key_prefixes(&self, prefixes: &[String]) -> Result<()>;
    fn stats(&self) -> Result<KgStats>;
    fn table_paths(&self, from: &str, to: &str,
                   max_hops: usize, limit: usize) -> Result<Vec<Vec<PathHop>>>;
}
```

Implemented by `SqliteKnowledgeStore` over three tables in the store, with
bounded table-path search over declared and inferred relations. Neighbor and
subgraph extraction methods remain reserved for the instance layer.

`remove_by_key_prefixes` is on the trait because incremental recompilation is
a property of the *design*, not of the SQLite backend: any substitute store
must be able to delete everything derived from one source table.

### `Embedder` — built

```rust
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;   // order-preserving
    fn identity(&self) -> ModelIdentity;
    fn format_query(&self, mention: &str) -> String;
}
```

The pipeline holds the trait as `Option<&dyn Embedder>` — the type
states that the embedder is optional, and `Result` states that it is
fallible. Absent or failing, resolution degrades to the lexical and knowledge
channels. That is the modularity contract doing real work: the resolver has
no idea what is on the other side and no way to require that anything is.

The implemented remote backend learns its dimension from the first response.
The trait owns `format_query()`, which renders the query side of the asymmetric
retrieval scheme through `ModelIdentity::query_template`. The query convention
therefore travels with the vector-space identity instead of living in the
pipeline.

A richer wire shape is already specified in
[`embedder.proto`](../../proto/stemma/v1/embedder.proto) — `Embed`, `Rerank`,
and `ModelInfo` returning `(backend, model, revision, dimension)` — for a
gRPC sidecar backend. `Rerank` and `revision` have no Rust counterpart yet.

The registry is not a nicety: `model_registry` records
`(vector_table, backend, model, revision, dimension, quantization)`, keyed by
vector table. Promotion and queue drains reject an incompatible identity, so
vectors from distinct spaces cannot mix silently. Online blue-green swaps
remain designed. See
[05-encoders-decoders.md](05-encoders-decoders.md).

### Language-service backends — built

A `LmBackend` trait exposes one chat-completion call, structured-output
capability, and a stable identity. The implemented remote backend requests a
schema-constrained reply when the service supports one. Otherwise it includes
the schema in the instruction, validates the response, and allows one
corrective retry.

Resolution uses the backend only for bounded mention expansion and candidate
adjudication. Parsing uses it to propose a small set of parameterized queries
for deterministic validation. The reference agent uses the same configuration
boundary for its conversational layer.

## External surfaces

### gRPC — `ResolveService`

```protobuf
service ResolveService {
  rpc Resolve(ResolveRequest) returns (ResolveResponse);
  rpc Explain(ResolveRequest) returns (ExplainResponse);
  rpc Parse(ParseRequest) returns (ParseResponse);
  rpc SubmitFeedback(FeedbackRequest) returns (Feedback);
  rpc ListFeedback(ListFeedbackRequest) returns (FeedbackList);
  rpc DeleteFeedback(DeleteFeedbackRequest) returns (DeleteFeedbackResponse);
}
```

`Resolve` returns selected mentions with their candidates and evidence.
`Explain` returns every span and candidate, channel scores, and near-miss
reasons. `Parse` starts from the same grounding path and returns a validated,
parameterized, read-only SQLite query when one proposal survives. The feedback
RPCs attach typed judgments to persisted resolution or parse episodes.

Resolve and Explain are served from the same `Trace` by the same code path
(`Resolver::trace_for`), so `Explain` can never disagree with `Resolve`. That
is the difference between an explanation and a reconstruction: a
reconstruction is a second implementation that drifts.

The choice to make Explain a peer RPC rather than a debug flag is
deliberate. Its consumers are not developers debugging — they are the
console's trajectory view and the MCP tool's structured content, both
user-facing.

### MCP — four tools

[`integrations/mcp/stemmadb_mcp.py`](../../integrations/mcp/stemmadb_mcp.py)
exposes stemma to any MCP client over stdio:

| Tool | Returns |
|---|---|
| `resolve(query, database)` | Compact digest (selected candidates as `table.column #rowid`, values truncated to 200 chars, near-misses listed by span text) **plus** `trajectory`: the entire Explain response |
| `sql(query, database)` | Read-only `SELECT` over `src` (user DB) and `main` (store), first 12 rows |
| `schema(database)` | Tables, columns, declared foreign keys |
| `knowledge_graph(database)` | Compiled graph digest: tables with row counts, 30 most central terms, all join edges with method and confidence |

The two-payload shape of `resolve` is the interesting part. A model needs a
short digest — a full trajectory would blow its context for no benefit — but
a *client* rendering resolution needs everything. Returning both, with the
digest as the tool result proper and the trajectory as structured content
alongside, serves both without a second round trip. `ui/agent_backend.py`
strips `trajectory` out of what it shows in the tool trail and re-attaches
the trace separately.

The server's `instructions` field carries the contract that makes the tools
work as a set:

> Before referring to any entity, value, table or column, pin it with
> resolve; cite resolutions as `table.column #rowid`. Use sql (read-only) to
> fetch what resolve pointed at — never invent identifiers.

**Resolve-before-reference** is stemma's consumption contract. An agent that
follows it cannot hallucinate an identifier, because every identifier it uses
came from a resolution it can cite, and every resolution carries the evidence
that produced it. `agents/stemma_agent/agent.py` restates the same rule in
its system instruction, adds "if resolution is ambiguous, say so and show the
top candidates instead of guessing", and is otherwise a plain `LlmAgent` — the
point being that the contract lives in the tool surface, not in a particular
framework.

### Query history as a first-class surface

Each non-empty Resolve, Explain, or Parse request attempts a `query_log` write.
The row carries `ResolveOptions.source`, `ResolveOptions.session`, revision
receipts, compact evidence selectors, and an opaque episode identifier. A
history-write failure leaves resolution available and returns an empty episode
identifier. Typed feedback can attach only to a persisted episode. History is
queryable with ordinary SQL and stored beside the corpus it describes. See
[02-data-model.md](02-data-model.md#query_log).

## Why this shape

### Why not inside the database

In-database model inference — UDFs that call a model, extensions that embed
an inference runtime — lost the 2023–2026 natural experiment. The surviving
production pattern is a stateless model service beside the store, with async
queue-driven embedding and versioned vector tables. stemma adopts that
pattern, and the reasoning is worth being precise about because "put the
model in the database" keeps sounding attractive.

**Latency is not the argument.** A local RPC hop is tens to hundreds of
microseconds; a model forward pass is milliseconds to hundreds of
milliseconds. The hop is noise. Anyone optimizing it away is optimizing the
wrong term.

**Lifecycle coupling is the argument.** A model inside the database means the
model's memory is the database's memory, the model's GPU is the database's
GPU, the model's crash is the database's crash, and — worst — the model's
upgrade is a database migration. Models change far more often than schemas
do. Every property the storage layer is supposed to provide (durability,
concurrent readers, being an ordinary file you can copy) is weakened by
attaching a model's lifecycle to it.

**Blast radius is the second argument.** stemma's core guarantee is that the
user's database is never written. That guarantee is much easier to hold when
the process holding a read-only handle is a small Rust binary than when it is
also hosting an inference runtime.

So: SQLite stays stock. Capability comes from core modules (FTS5 with the
trigram tokenizer, JSON1) plus one statically linked extension registered
through the sanctioned `sqlite3_auto_extension` hook. No patched SQLite, no
runtime `.so` loading, no fork.

### Why not just layers

The alternative to a purpose-built resolver is composing general-purpose
parts: a vector database, a reranker, an agent loop that queries them. That
composition can retrieve. It cannot produce the artifact stemma produces,
for four reasons that are all about the artifact rather than about retrieval
quality.

**Records, not chunks.** A resolution points at `(table, column, rowid)`. A
chunk-based retriever points at text it once ingested, and the mapping back
to a row is either lost or maintained by hand. Every candidate stemma returns
is a live pointer into the user's database, which is what makes verification
probes and downstream SQL generation possible at all.

**Evidence, not scores.** The consumer did not compute the resolution and
must be able to check it. A ranked list with cosine similarities is not
checkable. `Evidence` is a closed union of the five ways stemma can come to
believe something — lexical match, semantic match, KG path, live probe, LM
adjudication — and a candidate whose support is not expressible as one of
them is a candidate the system should not return.

**Near-misses, not answers.** The trace keeps what lost and why. A
disambiguation UI needs the rival Chen; an evaluation harness needs to know
whether the correct record was retrieved and rejected (a ranking bug) or
never retrieved (a recall bug). Those are different failures with different
fixes, and a system that returns only its answer cannot tell them apart.

**Corpus-derived structure.** The knowledge graph is compiled from the user's
own database — declared and discovered joins, frequent values, characteristic
terms, mined phrases — and it feeds back into mention detection and scoring.
A general retrieval stack has nowhere to put that, because it does not know
it is looking at a database. This loop is the part of stemma that is not
assemblable from parts, and [04-knowledge-graph.md](04-knowledge-graph.md)
is about it.

### Why encoders retrieve and decoders decide

The design conclusion that shapes the pipeline's future stages, stated here
because it explains the topology and argued in full in
[05-encoders-decoders.md](05-encoders-decoders.md):

Retrieve-then-rerank pipelines beat generative entity linking on both
accuracy and latency, and constrained autoregressive decoding over a catalog
the model was not trained on carries a systematic out-of-distribution error
floor. Constrained decoding forces *validity*, not *correctness* — and a
confidently wrong-but-valid record is worse than an explicit no-match,
because it is undetectable downstream.

So the encoder is the workhorse, embedding rows at index time and mentions at
query time. Resolution invokes the optional language service in two bounded
places: mention expansion before retrieval and constrained select-among-k with
an explicit NIL afterward. Parsing may invoke it again for query proposals,
which deterministic validation can reject. The language service is never the
retrieval mechanism. This division keeps the grounding pipeline a cheap-first
cascade.

## Current state

| Component | State |
|---|---|
| stemmadb storage layer, read-only attach, versioned store schema | built |
| Lexical index (exact / FTS5 BM25 / FTS5 trigram, `is_doc`) | built |
| Resolution pipeline (spans, three channels, RRF, greedy selection, full trace) | built |
| Knowledge compiler (schema, inclusion mining, value/term/phrase profile, centrality) | built |
| KG-assisted mention detection and coherence bonus | built |
| gRPC Resolve, Explain, Parse, and typed feedback; episode history with source/session | built |
| Python client, MCP server, reference agent, console | built |
| BIRD target derivation | built |
| `Embedder` trait + configured remote backend | built |
| Dense channel: `vec_staging` → `vec0` promotion, targeted KNN, model-registry write | built |
| Revision-checked approximate vector sidecar with exact mention safeguards | built behind a Cargo feature; native Bazel parity open; exact by default |
| Index-time embedding and content-hashed queue drain | built |
| Online blue-green vector swap and `Rerank` | designed |
| Fusion constants re-derived for four channels; `SemanticMatch` evidence | outstanding |
| Collective disambiguation and verification probes | built |
| Verified mention expansion for spans with no candidates | built |
| Persisted instance-alias layer | designed |
| Constrained adjudication and explicit `nil` | built |
| Scoring resolver output against BIRD targets | built |
| Query-level outcomes and deterministic grounding clarification | built; alternative-recall and dialogue evaluation open |
| Grounded SQLite parser and deterministic validation | first read-only slice built; broader coverage open |
| Trace-linked typed feedback | built; learning from feedback unsupported |

The table distinguishes shipping paths from staged work. Detailed evaluation
status is in [07-eval-harness.md](07-eval-harness.md); the query-level vertical
slice and its remaining gates are in
[08-query-disambiguation.md](08-query-disambiguation.md).

## References

- [C. Li 2025] Chaofan Li, Yingxia Shao, Yawen Li, Zheng Liu. "SEA-SQL:
  Semantic-Enhanced Text-to-SQL with Adaptive Refinement." *Frontiers of
  Computer Science*, 2025. arXiv:2408.04919.
- [D. Lee 2025] Dongjun Lee, Choongwon Park, Jaehyuk Kim, Heesoo Park.
  "MCS-SQL: Leveraging Multiple Prompts and Multiple-Choice Selection For
  Text-to-SQL Generation." COLING 2025.
- [Lei 2025] Fangyu Lei et al. "Spider 2.0: Evaluating Language Models
  on Real-World Enterprise Text-to-SQL Workflows." ICLR 2025 (Oral).
- [J. Li 2023] Jinyang Li et al. "Can LLM Already Serve as a Database
  Interface? A Big Bench for Large-Scale Database Grounded Text-to-SQLs."
  NeurIPS 2023 (BIRD).
- [Maamari 2024] Karime Maamari, Fadhil Abubaker, Daniel Jaroslawicz,
  Amine Mhedhbi. "The Death of Schema Linking? Text-to-SQL in the Age of
  Well-Reasoned Language Models." arXiv:2408.07702.
- [Nan 2026] Yafeng Nan et al. "DIVER: A Robust Text-to-SQL System
  with Dynamic Interactive Value Linking and Evidence Reasoning."
  arXiv:2602.12064.
- [Talaei 2024] Shayan Talaei et al. "CHESS: Contextual Harnessing for
  Efficient SQL Synthesis." arXiv:2405.16755.
- [Yun 2025] Janghyeon Yun, Sang-goo Lee. "SEED: Enhancing Text-to-SQL
  Performance and Practical Usability Through Automatic Evidence Generation."
  IEEE ICDEW 2025. arXiv:2506.07423.

Full bibliography: [00-bibliography.md](00-bibliography.md).
