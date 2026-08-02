# stemma technical design

The deep reference. [`docs/architecture.md`](../architecture.md) states the
design and the literature review behind it in a few pages; these documents
specify it — actual constants, actual DDL, scoring as mathematics, algorithms
with their parameters, and measured numbers from the corpora in this
repository.

Two conventions hold throughout.

**Built and designed are labelled separately.** Anything described as
existing matches the code as of now, with the source file named. Anything
that is planned is marked *designed*, *designed but unbuilt*, or similar. A
design document that blurs the two is worse than no document, because it
makes the codebase unnavigable for the person who trusts it.

**Limitations are stated, not omitted.** Each document ends its technical
sections with what is actually wrong or missing — span caps, greedy
selection, sampling bias, uncalibrated scores, layering violations. The
system's whole premise is that a resolution you cannot inspect is a
resolution you cannot trust; the same standard applies to its documentation.

## The documents

**[01 — Architecture](01-architecture.md)**
System decomposition across nine Rust crates plus the Python client, MCP
server, reference agent and console; the process topology (one server
process, everything else optional and separate, browsing that bypasses the
server entirely); the ownership and trust boundaries — the read-only attach
enforced by SQLite's VFS rather than by convention, the disposable derived
state that makes drop-and-rebuild affordable, and the single sanctioned write
from outside the core; the three trait seams (`KnowledgeStore`, `Embedder`,
LM backends) and which of them exist; the gRPC and MCP surfaces including the
resolve-before-reference contract; and the argument for the shape — why a
resolution engine beside a stock database beats putting the model inside the
database, and why a purpose-built resolver produces an artifact that a stack
of general-purpose retrieval layers cannot.

**[02 — Data model](02-data-model.md)**
Every table with its DDL: the four `STRICT` bookkeeping tables
(`model_registry`, `embed_queue`, `query_log`, `chat_log`), the lexical index
(`lex_values` plus two external-content FTS5 tables and `lex_vocab`), the
vector path (`vec_staging` → `vec_dense`, and why the staging table is
deliberately not a virtual one), and the knowledge store (`kg_nodes`,
`kg_edges`, `kg_meta`). The migration discipline:
forward-only `PRAGMA user_version`, additive idempotent DDL re-applied
wholesale, version-guarded `ALTER`s, drop-and-rebuild for pure derived
indexes, and compiler-versioned fingerprints that let the knowledge
algorithms change without any migration at all. The evidence and trace model
end to end, from the internal `Trace` through `ResolveResponse` and
`ExplainResponse`, with a field-by-field table of what is live and what is
declared-but-never-emitted. The history model and why attribution
(`source`, `session`) is a schema concern.

**[03 — Resolution](03-resolution.md)**
The pipeline stage by stage with every real constant — `MIN_SPAN_CHARS`,
`MAX_SPAN_TOKENS`, `PER_CHANNEL_LIMIT`, `SELECT_THRESHOLD`, `TOP_K`,
`DENSE_MAX_SPANS`, `RRF_K`, `DOC_MIN_LEN`, `EXACT_MAX_LEN`. Tokenization and
soft span enumeration; knowledge-graph-assisted mention detection; the three
lexical channels and their exact SQL, plus the dense channel and why it is
the one channel that is *conditional* (a `vec0` KNN is a full table scan, so
it is spent only on spans the lexical channels left uncertain); reciprocal
rank fusion written out as mathematics, including why `K = 4` rather than the
standard 60; the three scoring branches and the
quantitative argument for giving documents their own — the *careg failure
mode*, where applying length affinity to a 2,660-character regulation drops
its score to 0.296 against a 0.35 threshold and the entire corpus returns
zero mentions for every query. The coherence bonus with a measured
reordering, greedy non-overlapping selection, the trace contract, complexity
analysis with measured latencies on a 92,696-document corpus, and fourteen
named limitations — including the fusion constants that adding a fourth
channel silently invalidated.

**[04 — Knowledge graph](04-knowledge-graph.md)**
The layered compiler and its algorithms: inclusion-dependency mining with the
0.95 containment threshold and why it is not 1.0; frequent-value profiling
that excludes identifier columns with no heuristic; characteristic-term
selection via a document-frequency *ceiling* (high DF is the least
distinctive signal in a single-domain corpus) plus a burstiness shortlist and
weighted TextRank at damping 0.85 over 40 iterations; the capitalized-phrase
grammar with its connector rule and subsumption filter; co-occurrence edges
scored by conditional probability; graph-wide PageRank centrality.
Fingerprint-driven incremental maintenance with a compiler-version prefix,
the key-prefix recompilation unit, and the convergence guarantee. The
provenance model and why an unexplained edge is an unusable edge. Then the
three live ways the graph feeds resolution today, with real examples — and
the design of the loop's future: the instance layer, collective
disambiguation over join paths, and KG-guided expansion.

**[05 — Encoders and decoders](05-encoders-decoders.md)**
The dense channel as built and as designed: `vec0` table shape, model-registry
identity and the empty-`revision` gap, external staging versus the unbuilt
embed queue, the four query-time symmetry requirements and which of them is
currently unchecked, and the fusion debt that adding a fourth channel created
without anyone editing the scoring code. Then the argument that makes
per-corpus encoders necessary rather than nice — **embedding-space
crowding**, where a domain-uniform corpus collapses into a narrow region of a
general-purpose encoder's space and retrieval discrimination goes with it —
grounded in the anisotropy, degeneration and hubness literature, and then
*measured*: on this repository's own legal corpus a general encoder places
California's regulations and the federal eCFR at centroid cosine 0.81, with
93 of 1024 dimensions carrying the variance. How
[ambit](https://github.com/pedapudi/ambit) measures that (σ\*, the corpus's
noise budget) and tunes against it, what three rounds of tuning actually did
— including an honest negative result — and how tuned encoders slot in as
registry-identified vector generations, with the corpus's 100%-covered
pre-computed vectors as the first payload. The decoder's two roles — mention
expansion before retrieval and constrained select-among-k with explicit NIL
after it — the evidence against making the decoder the retrieval mechanism,
the `rewritten_query` substitution artifact, and the agent/MCP layer as the
consumption pattern that ties them together.

**[06 — Evaluation](06-evaluation.md)**
The BIRD no-evidence protocol and why it is the only setting that measures
the thing stemma does; target derivation from gold SQL, including why the
column-versus-literal formulation excludes join keys by construction rather
than by heuristic, and the honest limitations of a derived ground truth; the
recall-weighted metric set with the k = 1 / k = 5 / unbounded decomposition
that separates ranking failures from threshold failures from retrieval
failures; the three corpora and what each is for; corpus-construction
guidelines read as evaluation rules; and the acceptance gate for each
milestone.

**[00 — Bibliography](00-bibliography.md)**
The shared reference list, grouped by topic. Every entry was checked against
the published record rather than cited from memory; entries that could not be
confirmed are labelled.

## Reading order

For the design argument, read 01 then 05 — the topology and the
encoder/decoder split are the same decision seen from two sides. For the
implementation, read 02 then 03 then 04, which is the order the data flows.
For the research framing, 06 and the bibliography.
