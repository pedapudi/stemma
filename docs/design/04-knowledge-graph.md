# The knowledge graph

The knowledge graph is stemma's signature. Every other part of the system has
a well-known analogue — a lexical index, a fusion rule, a gRPC surface. The
part that is genuinely stemma's own is the loop: **compile structure out of
the user's own database, then use that structure to resolve mentions against
that same database, and record which structure was used as evidence.** No
external catalog, no ontology to author, no LLM pass required.

This document specifies what the compiler builds today, with the algorithms
and their real parameters; how incremental maintenance works; how provenance
is modelled; exactly how the graph feeds resolution now; and what the
knowledge-leveraged loop is designed to become.

Source: [`crates/stemma-kg/src/lib.rs`](../../crates/stemma-kg/src/lib.rs).
Physical schema: [02-data-model.md](02-data-model.md#knowledge-store-schema).

## The `KnowledgeStore` seam

```rust
pub trait KnowledgeStore {
    fn upsert_node(&self, node: &Node) -> Result<()>;
    fn upsert_edge(&self, edge: &Edge) -> Result<()>;
    fn remove_by_key_prefixes(&self, prefixes: &[String]) -> Result<()>;
    fn stats(&self) -> Result<KgStats>;
}
```

Four methods, one of which — `remove_by_key_prefixes` — exists purely to
express *the unit of incremental recompilation*. That is not an accident of
the SQLite backend leaking through; it is the operation any backend must
support for stemma's maintenance model to work, so it belongs on the trait.

The first (and only) backend is `SqliteKnowledgeStore`, three tables inside
the `.stemmadb` store. The invariant that makes the seam real: **graph SQL
never leaves this module.** The resolution pipeline's two knowledge queries
are the one deliberate exception, discussed and justified
[below](#how-the-graph-feeds-resolution-today).

Query-side methods — neighbours, bounded path search, subgraph extraction —
are named in the trait's doc comment as the additions that land with
collective disambiguation. They are deliberately absent rather than stubbed:
an unimplemented trait method is a lie about capability that consumers write
code against.

## Layers

The graph is built in layers, each with a different epistemic status, and
consumers are expected to weight them differently.

| Layer | Node kinds | Edge kinds | Certainty |
|---|---|---|---|
| Schema | `table`, `column` | `has_column`, `fk` | Declared — certain |
| Discovered relations | — | `inferred_fk` | Statistical, confidence-scored |
| Value profile | `value` | `frequent_value` | Observed counts |
| Term profile | `term` (words), `term` (phrases) | `term`, `cooccurs` | Mined, ranked |
| Instance | *(designed)* | *(designed)* | — |

### Compilation order

```
compile(db, force)
 ├─ detect dirty tables by fingerprint
 ├─ sweep nodes of dropped tables
 ├─ remove nodes of dirty tables (by key prefix)
 ├─ compile_schema_layer   (dirty tables)
 ├─ compile_value_profile  (dirty tables)
 ├─ compile_term_profile   (dirty tables)  ─┬─ TextRank terms
 │                                          ├─ co-occurrence edges
 │                                          └─ compile_phrase_entities
 ├─ compile_declared_fks   (ALL tables)
 ├─ compile_inferred_joins (ALL tables)
 ├─ stamp fingerprints for dirty tables
 └─ compute_centrality     (whole graph)
```

The per-table passes are scoped to dirty tables; the cross-table passes run
globally whenever *anything* changed. That asymmetry is forced: removing a
dirty table's nodes also removes clean tables' edges *into* it, so the
relational passes must be able to restore them. Containment is a global
property anyway.

## Schema layer

`compile_schema_layer` walks `src.sqlite_master` and `pragma_table_info(?,
'src')`:

- **`table:{name}`** — `props = {"rows": coalesce(max(rowid), 0)}`. Note this
  is `max(rowid)`, not `count(*)`: an O(1) index probe rather than a full
  scan, which for a table that has never had deletions is exact and otherwise
  is an upper bound. It is a sizing hint, not a statistic.
- **`column:{table}.{name}`** — `props = {"type": …, "table": …}`, with a
  `has_column` edge from the table, `{"method": "declared", "confidence": 1.0}`.

`compile_declared_fks` reads `pragma_foreign_key_list(?, 'src')` and emits a
`fk` edge per declared constraint, `table → table`, labelled with the column
pair (`office_id → id`). FKs pointing at tables outside the served set are
skipped rather than creating dangling nodes.

This is the join graph that schema-linking work in text-to-SQL exploits —
from relation-aware schema encoding [B. Wang 2020] through schema
serialization with anchor text [Lin 2020] to decoupled schema linking
[H. Li 2023]. Getting it for free from the DDL is the easy half; the
hard half is that real databases mostly do not declare it.

## Discovered relations: inclusion-dependency mining

`compile_inferred_joins` recovers undeclared joins by testing **inclusion
dependencies** — whether one column's values are contained in another's. This
is a well-studied primitive: SPIDER's sort-merge formulation
[Bauckmann 2007], BINDER's divide-and-conquer scaling
[Papenbrock 2015b], and the profiling platform that collects and
compares them [Papenbrock 2015a].

**Candidate generation** narrows the search space before any data is touched:

- *Key columns*: single-column `INTEGER PRIMARY KEY`s. (`pk == 1` and the
  table has exactly one PK column and the declared type contains `INT`.)
- *Referencing columns*: integer columns that are not their table's primary
  key.
- Pairs already covered by a declared FK are skipped.
- Self-references (`kt == rt`) are skipped.

**The test**, per (referencing column, key column) pair:

```sql
SELECT count(*) FROM (
    SELECT DISTINCT "rc" AS v FROM src."rt" WHERE "rc" IS NOT NULL
    EXCEPT SELECT "kc" FROM src."kt"
)
```

$$
\text{containment} = 1 - \frac{|\,\pi_{rc}(R) \setminus \pi_{kc}(K)\,|}{|\pi_{rc}(R)|}
$$

An edge is emitted when containment ≥ `INFERRED_FK_MIN_CONTAINMENT = 0.95`,
carrying `{"method": "inferred", "confidence": 0.973, "distinct": 412}`.
Columns with more than `INFERRED_FK_MAX_DISTINCT = 500,000` distinct values
are skipped outright — the guard that keeps this from becoming the dominant
cost on a large database.

**Why 0.95 and not 1.0.** A perfect-containment requirement finds only
relationships that were never violated, which in real data means it finds
almost nothing: soft deletes, historical rows pointing at purged parents, and
sentinel values like `0` or `-1` all break exact containment while leaving
the relationship obviously real. 0.95 accepts one violation in twenty. The
cost of that tolerance is paid honestly — the confidence is stored on the
edge, so a consumer that needs certainty can filter for `method = "declared"`
and a consumer that needs recall can take everything and weight it.

**And containment alone is not a foreign key.** The discovery literature is
unambiguous that the set of valid inclusion dependencies contains many
spurious set inclusions — two unrelated small integer columns will contain
each other by accident — which is why later work classifies true foreign keys
from IND *features* rather than accepting every IND [Rostin 2009], and
why holistic approaches score primary and foreign keys jointly against the
whole candidate space, recovering roughly 88% of primary keys and 91% of
foreign keys [Jiang 2020]. stemma's single-threshold test is the
first-order version: it emits candidates with a confidence and labels them
`inferred`, and the classifier that would sharpen them is unbuilt.

**Complexity.** With *r* referencing columns and *k* key columns the pass is
O(*r·k*) `EXCEPT` queries, each linear in the two column sizes. On a wide
schema this is the most expensive part of compilation, and it is quadratic in
table count. Cheaper pruning — comparing min/max ranges, distinct counts, or
Bloom-filter sketches before the exact `EXCEPT`, as the inclusion-dependency
literature does [Bauckmann 2007] — is the obvious next step and is
unbuilt.

The regression test builds `depts(id)` / `staff(dept)` with no declared FK
and asserts the discovered edge is labelled `dept →? id` with
`"method":"inferred","confidence":1.000`. The `→?` in the label is
deliberate: the graph should look uncertain where it is uncertain, including
to a human reading the console.

## Value profile

`compile_value_profile` finds values worth naming as graph entities:

```sql
SELECT src_table, src_column, value, n FROM (
    SELECT src_table, src_column, value, count(*) AS n,
           row_number() OVER (
               PARTITION BY src_table, src_column ORDER BY count(*) DESC
           ) AS rk
    FROM lex_values WHERE is_doc = 0 AND src_table IN (…)
    GROUP BY src_table, src_column, value_norm
) WHERE n >= 2 AND rk <= 10
```

`MIN_VALUE_COUNT = 2`, `TOP_VALUES_PER_COLUMN = 10`. Grouping by `value_norm`
folds case and edge-whitespace variants together; the surface `value` is kept
as the node label.

The elegance here is what the filter does *for free*: **identifier columns
earn nothing, with no heuristic to detect them.** A uuid column, a primary
key, an email address column — every value occurs once, so nothing clears
`n >= 2`, so no nodes are created. Categorical columns, status enums and
foreign-key-shaped text columns fall out naturally. Detecting
"identifier-shaped" columns explicitly would need a threshold and would get
it wrong; requiring recurrence needs neither.

On the legal corpus this yields exactly four value nodes — the two `license`
constants and the two `category` constants — with `count` 57,523 and 35,173.
That is correct behaviour, and it is also a signal: a table whose only
frequent values are constants has no categorical structure to exploit.

## Term profile: characteristic terms of document corpora

This is the pass that gives a *document* corpus graph structure. A table of
57,523 regulation bodies has no categorical columns, no useful frequent
values, and — if you stop at the schema layer — no graph at all. The
GraphRAG lineage [Edge 2024; Guo 2025] solves this with an LLM
extraction pass over every document. stemma solves the first-order version of
it with corpus statistics, deterministically and at no per-document cost. The
LLM pass is designed as a *supplement*, not a replacement.

Runs per dirty table that has at least one `is_doc` value.

### Step 1 — candidate shortlist: a DF ceiling plus burstiness

```sql
SELECT term, doc, cnt FROM lex_vocab
WHERE length(term) >= 4 AND doc >= 5 AND doc <= {df_ceiling}
  AND term NOT GLOB '*[0-9]*'
ORDER BY doc DESC LIMIT 4000
```

with

$$ \text{df\_ceiling} = \lceil |D| \times \texttt{MAX\_TERM\_DF\_RATIO} \rceil, \quad \texttt{MAX\_TERM\_DF\_RATIO} = 0.25 $$

Two filters replace a stopword list entirely, which matters because **a
domain corpus has its own stopwords that no general list contains**. In
California regulations, *shall*, *section*, *subdivision*, *pursuant* and
*within* are as uninformative as *the* — and vastly more frequent than any
term you actually want.

- **The DF ceiling is the load-bearing filter.** A term appearing in more
  than a quarter of the documents is a corpus stopword *no matter how common
  or how domain-specific it looks*. High document frequency is the **least**
  distinctive signal in a single-domain corpus — the inverse of the intuition
  that frequency means importance. This is IDF's insight applied as a hard
  cut rather than a weight, and it is applied before ranking so that
  ranking never has to fight it.
- **`doc >= MIN_TERM_DOCS = 5`** removes hapax noise from the other end.
- **`length(term) >= MIN_TERM_LEN = 4`** and `NOT GLOB '*[0-9]*'` remove
  short function words and numeric tokens.

The 4,000 survivors are then ranked by a **burstiness** prior:

$$
b(t) = \frac{\mathrm{cnt}(t)}{\mathrm{df}(t)} \cdot \bigl(1 + \ln \mathrm{df}(t)\bigr)
$$

The first factor is occurrences per containing document — a term that appears
*repeatedly inside* the documents that use it is a topic of those documents,
while a term appearing once everywhere is background. The second factor is a
gentle coverage reward so a bursty term appearing in five documents does not
outrank a bursty term appearing in five hundred. The top
`PAGERANK_CANDIDATES = 200` proceed.

Burstiness as a topicality signal is the same observation that motivates
RAKE's degree/frequency ratio [Rose 2010] and YAKE's term-dispersion features
[Campos 2018; Campos 2020], arrived at from corpus statistics rather than from
a per-document parse.

The deliberate omission is **position**. Single-document keyphrase extractors
weight early occurrences heavily — PositionRank biases the random walk toward
terms appearing near the start of a document [Florescu 2017], and SingleRank's
neighbourhood extension exists because one short document has too little
signal on its own [Wan 2008]. stemma has the opposite problem: thousands of
documents in one register, where a term's position within any single
regulation says nothing, and the discriminating signal is which *other* terms
it keeps company with across the corpus. Corpus-level co-occurrence is the
right instrument here; per-document position is not.

### Step 2 — one document pass builds everything

```sql
SELECT value FROM lex_values WHERE is_doc = 1 AND src_table = ?
ORDER BY id LIMIT 1500          -- PHRASE_SAMPLE_DOCS
```

A single scan over at most 1,500 documents produces three things at once: the
term co-occurrence counts, the sample document frequencies, and the input to
phrase mining. For each document, the set of candidate terms present is
computed (words of ≥ 4 characters, lowercased, exact membership in the
candidate set); then `sample_df[i] += 1` for each present term and
`cooccur[(i,j)] += 1` for each present pair.

`ORDER BY id LIMIT 1500` is a *prefix*, not a random sample. On a corpus with
any ordering structure — and regulations are ordered by title and chapter —
the prefix is a biased sample of the corpus's topics. This is a real
weakness, and it is cheap to fix (sampling by `rowid % k` or by a hash), but
it is what the code does today.

### Step 3 — TextRank

The co-occurrence graph is ranked with weighted PageRank
[Mihalcea 2004; Page 1999]:

$$
R_{t+1}(i) = \frac{1-d}{n} + d\!\left(\frac{\sum_{j \in \text{dangling}} R_t(j)}{n}
+ \sum_{(i,j) \in E} \frac{w_{ij}}{\sum_{k} w_{jk}} R_t(j)\right)
$$

with damping **d = 0.85** and a fixed budget of **40 power iterations**, no
convergence tolerance. Edges are treated as undirected (co-occurrence is
symmetric): each edge contributes mass in both directions, normalized by
each endpoint's total incident weight. Dangling nodes — candidates that
co-occur with nothing — donate their mass uniformly, which keeps the
iteration mass-preserving.

Forty iterations without a tolerance check is right at these sizes: the
graph has at most 200 nodes, PageRank on a connected graph of that size is
converged well before iteration 40, and a tolerance check would cost more
code than the iterations it saves.

The top `TOP_TERMS_PER_TABLE = 24` ranked terms with non-zero sample document
frequency become `term:{table}:{term}` nodes carrying
`{"docs": …, "textrank": …}`, each with a `term` edge from its table
(`{"method": "textrank", "docs": …}`).

**Why centrality and not frequency.** Frequency ranking answers "what is
mentioned most"; TextRank answers "what is mentioned *alongside other things
that matter*". A term that is common but topically isolated ranks below a
term that is the hub of a topical cluster. That is precisely the property
that makes the resulting vocabulary useful as an alias table for mention
detection — hub terms are the ones queries are phrased around.

**A deviation from classic TextRank worth naming.** Mihalcea & Tarau build
the co-occurrence graph from a sliding window of 2–10 words within a
document. stemma's window is *the whole document*: two terms co-occur if they
appear in the same regulation. This is coarser — it loses local syntactic
association — but it is computable in one pass over the corpus rather than a
parse per document, and at document granularity it measures topical
co-membership, which is what the coherence bonus in
[03-resolution.md](03-resolution.md#stage-6--knowledge-graph-coherence)
actually consumes.

### Step 4 — co-occurrence edges

Among the kept terms, pairs are sorted by raw co-occurrence count and
filtered by a conditional ratio:

$$
\text{ratio}(a,b) = \frac{|\,D_a \cap D_b\,|}{\max\bigl(1, \min(|D_a|, |D_b|)\bigr)}
\;\ge\; \texttt{MIN\_COOCCUR\_RATIO} = 0.25
$$

Dividing by the *smaller* document frequency rather than by the union makes
this a conditional probability — "given the rarer of the two terms, how often
does the other appear" — which is the right question for a coherence signal
and which does not punish a strong association between a rare term and a
common one. At most `TOP_COOCCUR_PAIRS = 40` edges are kept per table, each
carrying `{"method": "profiled", "confidence": ratio, "docs": count}`.

Real edges from the legal corpus:

| a | b | docs | confidence |
|---|---|---:|---:|
| facility | data | 95 | 0.59 |
| corporation | account | 43 | 0.43 |
| facility | percent | 41 | 0.33 |
| corporation | percent | 43 | 0.35 |
| corporation | contract | 34 | 0.25 |

### Step 5 — capitalized-phrase mining

Named entities in prose are multi-word and capitalized, and no amount of
single-term statistics will recover *California Coastal Commission* as one
thing. `compile_phrase_entities` mines them from the same document sample
with a small deterministic grammar:

```
phrase  := Cap ( Cap | connector Cap )*        -- ≥ 2 words, ≤ 6 words, ≤ 60 chars
Cap     := word with uppercase initial, ≥ 2 chars, letters/apostrophes only
connector := "of" | "and" | "the" | "for"      -- only between two Caps
```

The scan starts at each capitalized word, extends greedily through
capitalized words, permits a connector only when the immediately preceding
word was capitalized, and **must end on a capitalized word** (`last_cap`), so
trailing connectors are trimmed by construction.

Phrases recurring at least `MIN_PHRASE_COUNT = 5` times survive, then a
subsumption filter removes any phrase that is a strict prefix of a longer
phrase with comparable support:

```rust
!counts.iter().any(|(longer, &ln)| longer != p
                   && longer.starts_with(p) && ln * 2 >= *n)
```

*California Coastal* is dropped in favour of *California Coastal Commission*
whenever the longer form has at least half the support of the shorter — the
"comparable support" tolerance, since the longer form is necessarily no more
frequent than its prefix. The top `TOP_PHRASES_PER_TABLE = 20` become
`phrase:{table}:{lowercased}` nodes of kind `term`, with
`{"count": n, "phrase": true, "sampled": |docs|}`.

Real phrases from the legal corpus: *California Code of Regulations Title*,
*Revenue and Taxation Code*, *Government Code*, *Executive Officer*, *State
Personnel Board Subchapter*, *United States*, *Social Security Division*.

The mining is honest about its own quality. *Annos Chapter* and *Health
Planning and Facility Construction Refs* are artefacts of the corpus's
boilerplate citation formatting (`Refs & Annos`), not entities. A purely
lexical miner will produce those; the LLM extraction pass of the instance
layer is what supersedes it, and the comment in the code says exactly that:
*"the LLM-based extraction pass of the instance layer supersedes, not
replaces, this."* Deterministic mining remains the floor that works with no
model available.

## Graph-wide centrality

After every compile, `compute_centrality` runs PageRank over the *compiled
graph itself* — all node kinds, all edge kinds, edges collapsed to an
undirected multigraph weighted by parallel-edge count — and writes the result
onto every node:

```sql
UPDATE kg_nodes SET props = json_set(props, '$.centrality', ?1) WHERE id = ?2
```

Same `pagerank()` function as TextRank, reused rather than reimplemented.
Legal-corpus values: `table:sections` 0.144, `table:regulations` 0.137 (hubs,
as expected — every column and term hangs off them), the term *percent*
0.027, *contract* 0.020, *commission* 0.004.

Centrality is consumed by the console (which sizes graph marks by it) and by
the MCP `knowledge_graph` tool (which sorts characteristic terms by it before
truncating to 30 for model context). It costs one `UPDATE` per node, which is
the pass's real weakness on a large graph.

## Incremental maintenance

Batch KG rebuilds do not survive contact with a corpus that changes. stemma's
maintenance model is fingerprint-driven dirty tracking.

### The fingerprint

```rust
SELECT count(*), coalesce(max(rowid),0), coalesce(sum(rowid),0) FROM src."{table}"
// → "kg2:{n}:{mx}:{sum}"
```

Three aggregates over the rowid column, no text hashing: O(n) with a tiny
constant, and computable by index scan. It catches inserts, deletes, and
rowid churn — every structural change to a table's row set.

**What it misses**, stated in the code and worth repeating: an in-place
`UPDATE` that changes text while preserving count, max rowid and rowid sum is
invisible to it. That is an accepted trade for derived state with a `force`
escape hatch. A content hash would close the gap at the cost of reading every
byte of every table on every startup, which for the 789 MB legal corpus is
the difference between a fast start and a slow one.

**The `kg2:` prefix versions the compiler, not the data.** Bump it and every
stored fingerprint mismatches, so every table recompiles on the next run —
which is exactly what an improvement to term selection or join mining
requires. Algorithm upgrades therefore need no migration, no store version
bump, and no user action. This is the mechanism that keeps the knowledge
graph safe to keep changing.

### The recompilation unit

Node keys are structured so that *the nodes derived from one table are a set
of key prefixes*:

```rust
store.remove_by_key_prefixes(&[
    format!("table:{t}"),
    format!("column:{t}."),
    format!("value:{t}."),
    format!("term:{t}:"),
])?;
```

`remove_by_key_prefixes` deletes matching nodes and every edge touching them,
in that order (edges first, so the foreign keys hold). This is why key design
is load-bearing rather than cosmetic: a surrogate node id would require a
provenance column and a join to achieve the same thing.

The same prefix sweep handles **dropped tables** — any `kg_meta` row for a
table no longer in `src_tables()` has its nodes removed and its meta row
deleted, so stale structure does not accumulate.

**A known gap:** the prefix list omits `phrase:{t}:`. Phrase nodes are
therefore not removed on recompilation. Because `upsert_node` is keyed, they
are refreshed rather than duplicated, but a phrase that stops qualifying
after the underlying data changes is never deleted.

### Convergence

`incremental_recompile_skips_clean_tables` asserts the property that makes
this trustworthy:

```rust
let a = compile(&db, false)?;   assert_eq!(a.recompiled_tables, 6);
let b = compile(&db, false)?;   assert_eq!(b.recompiled_tables, 0);  // no work
// dirty exactly one table
let c = compile(&db, false)?;   assert_eq!(c.recompiled_tables, 1);
assert_eq!(a.nodes, c.nodes);   assert_eq!(a.edges, c.edges);        // converges
```

An incremental recompile reaches the same graph a full recompile would. That
is the only guarantee that makes incremental maintenance safe to leave on by
default — which it is: `stemma-server` calls `compile(&db, false)` on every
registered database at startup.

## Provenance

Every edge carries `props` with a `method`, and a test enforces it:

```sql
SELECT count(*) FROM kg_edges WHERE props NOT LIKE '%method%'   -- must be 0
```

| `method` | Meaning | Emitted by |
|---|---|---|
| `declared` | Read from the DDL | schema layer, declared FKs |
| `inferred` | Statistical containment, with `confidence` | inclusion-dependency mining |
| `profiled` | Observed counts, with `count` or `confidence` | value profile, co-occurrence, phrases |
| `textrank` | Graph-centrality selection | term nodes |

Provenance is not documentation. It is what lets a consumer weight a
0.96-containment guess below a declared constraint, what lets the console
draw discovered joins dashed and declared joins solid, what lets the MCP tool
hand a model `{"method": "inferred", "confidence": 0.96}` instead of an
unqualified assertion, and what makes a wrong edge diagnosable. **An edge you
cannot explain is an edge you cannot trust** — and in a system whose entire
output is "here is a resolution and here is why", an unexplained internal
edge is a hole in the evidence chain.

## How the graph feeds resolution today

Three live consumers, plus two derived surfaces.

### 1. Mention detection — `kg_alias` spans

```sql
SELECT count(*) FROM kg_nodes WHERE kind = 'term' AND lower(label) = ?1
```

Run per non-skipped span. A hit sets `span.kg_alias`, which multiplies the
span's selection key by 1.08 in greedy selection. Because both TextRank terms
and mined phrases are `kind = 'term'`, this matches multi-word entities: the
graph proposes *coastal development permit* as a unit, and the nudge is what
lets it beat its own fragments for the byte range.

This is the corpus acting as its own alias table — the AIDA-lineage move
[Hoffart 2011] of using an entity vocabulary to propose spans, with
the vocabulary compiled rather than curated.

### 2. Candidate scoring — the `kg` coherence channel

Detailed in
[03-resolution.md](03-resolution.md#stage-6--knowledge-graph-coherence).
In short: co-occurring terms of the span's tokens are looked up in the
`cooccurs` edges, and document candidates containing them earn +0.04 each
(cap 0.9), recorded as a `kg` channel in the trace and as `LexicalMatch`
evidence over gRPC.

A measured reordering, query *"facility contract payment"*, span `facility`:
`regulations #57316` (BM25 rank 1) finishes at 0.590 and overtakes
`regulations #42595` (BM25 rank 0, 0.472), because #57316 contains two of the
graph's co-occurring neighbours of *facility* and #42595 contains none. The
lexical channels ranked them the other way round; the corpus's own topical
structure broke the tie.

### 3. Query suggestions

`StoreBrowser.examples()` mines the strongest `cooccurs` pairs (ordered by
`props.$.docs`) into two-word query suggestions, and the highest-count
`value` nodes into single-value suggestions. The console's example queries
are therefore generated *from the corpus*, not authored — a new database gets
sensible starting queries with no configuration.

### 4. Orientation surfaces

The MCP `knowledge_graph` tool returns a digest — table nodes with row
counts, the 30 most central characteristic terms, and every `fk`/`inferred_fk`
edge with its method and confidence — explicitly "useful for orienting in an
unfamiliar corpus". The console's graph view renders the whole thing.

### A note on the layering exception

The resolution pipeline issues those two knowledge queries directly against
`kg_nodes` and `kg_edges` rather than through the `KnowledgeStore` trait, and
guards each with an existence check on `sqlite_master`. That is a real
deviation from the invariant that graph SQL stays in its backend, taken
knowingly: the trait has no read-side methods yet, and adding
`is_entity(label)` / `neighbours(label)` to it is the correct fix. It is
listed here because a design document that describes the invariant without
naming its one violation is describing a different codebase.

## Designed: instance layer and collective disambiguation

Everything in this section is **designed, not built**.

### Instance layer

The layers above describe *schemas, columns, values and topics*. What they do
not describe is *entities as records*: this specific regulation, this
specific person, the aliases each is known by. The instance layer adds:

- **Per-record entity nodes** for tables the profiler identifies as entity
  tables (a name-like text column, a stable key, moderate cardinality).
- **Alias edges** from surface forms to records: the stored value, its
  normalizations, mined abbreviations and acronyms, and — for document
  corpora — the capitalized phrases that co-refer.
- **Embedding-assisted entity resolution across rows**: near-duplicate
  records linked by dense similarity above a threshold, so that *Wei Chen* in
  one table and *W. Chen* in another become one referent with two surface
  forms. This is the point where the dense channel and the knowledge graph
  meet, and it is why per-corpus encoder quality matters to the graph and not
  just to retrieval — see
  [05-encoders-decoders.md](05-encoders-decoders.md#where-tuned-encoders-matter-most-the-instance-layer).

The instance layer is what turns `kg_alias` from a coarse "this span is a
corpus term" into "this span is a known surface form of record 17".

### Collective disambiguation over join paths

The unsolved case in text-to-SQL value linking is the *associative* mention:
*Chen's team*, *the crown's holdings*. Neither mention is resolvable alone —
there are two Chens — but the **pair** is: the right Chen is the one with a
path to some team.

Collective entity linking solved this shape of problem more than a decade ago
[Hoffart 2011; Phan 2019]. Score candidate *tuples* jointly:

$$
\hat{c} = \arg\max_{c \in C_1 \times \cdots \times C_m}
\sum_{i} \bigl[\lambda_1 s_i(c_i) + \lambda_2 \mathrm{prior}(c_i)\bigr]
+ \lambda_3 \sum_{i<j} \mathrm{coh}(c_i, c_j)
$$

where `s_i` is the local fused score from
[03-resolution.md](03-resolution.md), and `coh` is knowledge-graph coherence
— for stemma, a bounded path search between the two candidates' records over
`fk` and `inferred_fk` edges, weighted by path length and edge confidence.

The scale argument is what makes this practical: a query has 2–4 mentions
with ~10 candidates each, so exhaustive joint scoring is 10² to 10⁴ pair
evaluations. That is microseconds — no approximation, no beam search, no
iterative message passing. The expensive part is the bounded path search, and
it is bounded precisely so it stays cheap.

Landing this requires the query-side `KnowledgeStore` methods (neighbours,
bounded path search, subgraph extraction), and it subsumes the greedy
selection of [stage 7](03-resolution.md#stage-7--greedy-non-overlapping-selection):
once tuples are scored jointly, segmentation can be part of the same
optimization instead of a separate greedy pass.

Each joint decision produces a `KgPath` evidence message — node labels and
edge labels along the connecting path — so a collective decision is as
inspectable as a lexical one.

### KG-guided expansion

Two further uses of the compiled graph, both cheap:

- **Term expansion before retrieval.** A span whose tokens are graph terms
  can be expanded with its strongest `cooccurs` neighbours as *additional
  retrieval queries* rather than only as a post-hoc bonus, recovering
  documents that use the topic's vocabulary without using the span's exact
  words. This is the deterministic counterpart to LM mention expansion
  [Xin 2025] and should run first, because it costs a graph lookup
  instead of a model call.
- **Schema-path priors.** The join graph tells the resolver which tables are
  reachable from which; a mention that resolves into a table with no path to
  the tables the other mentions resolved into is probably wrong. This is the
  same coherence signal as above, applied at the schema level rather than the
  instance level, and it needs only the edges that already exist.

## Parameter summary

| Constant | Value | Governs |
|---|---:|---|
| `TOP_VALUES_PER_COLUMN` | 10 | Frequent-value nodes per column |
| `MIN_VALUE_COUNT` | 2 | Recurrence needed for a value node |
| `TOP_TERMS_PER_TABLE` | 24 | TextRank terms kept per table |
| `MIN_TERM_LEN` | 4 | Shortest candidate term |
| `MIN_TERM_DOCS` | 5 | DF floor |
| `MAX_TERM_DF_RATIO` | 0.25 | DF ceiling — the corpus-stopword filter |
| `PAGERANK_CANDIDATES` | 200 | Shortlist size fed to TextRank |
| `PHRASE_SAMPLE_DOCS` | 1500 | Documents scanned per table |
| `MIN_PHRASE_COUNT` | 5 | Recurrence needed for a phrase node |
| `TOP_PHRASES_PER_TABLE` | 20 | Phrase nodes kept per table |
| `TOP_COOCCUR_PAIRS` | 40 | Co-occurrence edges kept per table |
| `MIN_COOCCUR_RATIO` | 0.25 | Conditional co-occurrence floor |
| `INFERRED_FK_MIN_CONTAINMENT` | 0.95 | Containment needed to propose a join |
| `INFERRED_FK_MAX_DISTINCT` | 500,000 | Cardinality guard on inclusion mining |
| PageRank damping | 0.85 | Both TextRank and graph centrality |
| PageRank iterations | 40 | Fixed budget, no tolerance check |

## References

- [Bauckmann 2007] Jana Bauckmann, Ulf Leser, Felix Naumann, Véronique
  Tietz. "Efficiently Detecting Inclusion Dependencies." 2007 (SPIDER; venue
  unverified — see [00-bibliography.md](00-bibliography.md)).
- [Campos 2018] Ricardo Campos et al. "A Text Feature Based Automatic Keyword
  Extraction Method for Single Documents." ECIR 2018.
- [Campos 2020] Ricardo Campos et al. "YAKE! Keyword extraction from
  single documents using multiple local features." *Information Sciences*
  509, 2020.
- [Florescu 2017] Corina Florescu, Cornelia Caragea. "PositionRank: An
  Unsupervised Approach to Keyphrase Extraction from Scholarly Documents."
  ACL 2017.
- [Jiang 2020] Lan Jiang, Felix Naumann. "Holistic primary key and
  foreign key detection." *J. Intelligent Information Systems* 54, 2020.
- [H. Li 2023] Haoyang Li, Jing Zhang, Cuiping Li, Hong Chen. "RESDSQL:
  Decoupling Schema Linking and Skeleton Parsing for Text-to-SQL." AAAI 2023.
- [Lin 2020] Xi Victoria Lin, Richard Socher, Caiming Xiong. "Bridging
  Textual and Tabular Data for Cross-Domain Text-to-SQL Semantic Parsing."
  Findings of EMNLP 2020.
- [Edge 2024] Darren Edge et al. "From Local to Global: A Graph RAG
  Approach to Query-Focused Summarization." arXiv:2404.16130.
- [Hoffart 2011] Johannes Hoffart et al. "Robust Disambiguation of
  Named Entities in Text." EMNLP 2011.
- [Mihalcea 2004] Rada Mihalcea, Paul Tarau. "TextRank: Bringing
  Order into Text." EMNLP 2004.
- [Page 1999] Lawrence Page, Sergey Brin, Rajeev Motwani, Terry
  Winograd. "The PageRank Citation Ranking: Bringing Order to the Web."
  Stanford InfoLab, 1999.
- [Papenbrock 2015a] Thorsten Papenbrock et al. "Data Profiling with
  Metanome." PVLDB 8(12), 2015.
- [Papenbrock 2015b] Thorsten Papenbrock et al. "Divide &
  Conquer-based Inclusion Dependency Discovery." PVLDB 8(7), 2015.
- [Phan 2019] Minh C. Phan et al. "Pair-Linking for Collective Entity
  Disambiguation: Two Could Be Better Than All." IEEE TKDE, 2019.
- [Rostin 2009] Alexandra Rostin, Oliver Albrecht, Jana Bauckmann,
  Felix Naumann, Ulf Leser. "A Machine Learning Approach to Foreign Key
  Discovery." WebDB 2009.
- [Wan 2008] Xiaojun Wan, Jianguo Xiao. "Single Document Keyphrase Extraction
  Using Neighborhood Knowledge." AAAI 2008.
- [B. Wang 2020] Bailin Wang et al. "RAT-SQL: Relation-Aware Schema
  Encoding and Linking for Text-to-SQL Parsers." ACL 2020.
- [Xin 2025] Amy Xin et al. "LLMAEL: Large Language Models are Good
  Context Augmenters for Entity Linking." CIKM 2025.

Full bibliography: [00-bibliography.md](00-bibliography.md).
