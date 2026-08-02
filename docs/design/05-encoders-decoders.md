# Encoders and decoders

**What is built, as of this writing:** the `Embedder` trait and its
OpenAI-compatible backend ([`stemma-embed`](../../crates/stemma-embed/src/lib.rs)),
the `vec_staging` → `vec_dense` promotion path with its `model_registry`
write, **index-time embedding through the queue** (enqueue at startup, an
async drain through the `Embedder`), the targeted dense retrieval channel
inside the pipeline, and the consumption pattern (MCP surface and reference
agent). The mechanics of the built parts are specified in
[02-data-model.md](02-data-model.md#vec_staging-and-vec_dense) and
[03-resolution.md](03-resolution.md#stage-4b--the-dense-channel-targeted);
this document gives the *why*, and everything it describes beyond those
mechanics — online blue-green swaps, re-embedding on data change,
cross-encoder reranking, and the entire decoder half — is **designed, not
built**. `stemma-lm` is still a one-line placeholder.

The design rests on one division of labour, and this document argues it,
specifies both halves, and then addresses the failure mode that decides
whether the encoder half works at all: **embedding-space crowding**.

## The division of labour

> **Encoders do retrieval. Decoders decide among presented options. The LM is
> never the retrieval mechanism.**

The entity-linking literature converged on this by trying the alternative.
The dense retrieve-then-rerank lineage — BLINK [Wu 2020], ELQ [B.Z. Li 2020],
ReFinED [Ayoola 2022], ReLiK [Orlando 2024] — beats generative entity linking
on accuracy *and* on latency, and the gap widens as the catalog grows.
Autoregressive entity retrieval [De Cao 2021] is elegant and constrains
generation to valid identifiers with a prefix trie; the problem is what
"valid" buys you.

**Constrained decoding forces validity, not correctness.** A model
constrained to emit only identifiers that exist in the catalog will always
emit one that exists. When the right answer is not in its learned
distribution — because the catalog changed, or because the model never saw
this database — it emits a *confidently wrong but structurally valid*
identifier. That is strictly worse than an explicit no-match, because a
no-match is detectable downstream and a valid-but-wrong record is not.

[S. Wu 2025] makes this precise, and it is worth being clear that the paper is
**theoretical rather than an ablation**. It derives a lower bound on the KL
divergence between the true and predicted step-wise marginals, arising because
the model is unaware of future constraints while it is generating — the
constraint is applied to a distribution that was not computed with the
constraint in mind. It further shows that beam search over those marginals
optimizes the wrong objective, so on sparse relevance distributions a model
can achieve *perfect top-1 precision while suffering poor top-k recall*: on
TREC DL 2019, R@50 of 53.7 against P@1 of 69.8; on MS MARCO-dev, 67.5 against
90.5. The gap opens at the very first decoding step.

That precision/recall asymmetry is the specific reason constrained generation
is wrong for stemma: the pipeline's whole output contract is a **recall-biased
candidate set**, and this is a mechanism that trades recall for top-1
precision by construction.

For stemma the argument is sharper than for the open-domain case, because
stemma's "catalog" is *the user's database* — a catalog no model was trained
on, that changes whenever the data changes, and whose identifiers are
rowids. There is no version of generative retrieval that is not permanently
out of distribution here.

Hence the shape:

| Component | Role | Invoked |
|---|---|---|
| Lexical channels | Exact, token, and substring retrieval | Always |
| **Encoder** | Dense retrieval over serialized rows; optional cross-encoder rerank | Always (once built) |
| Knowledge graph | Mention detection, coherence, collective disambiguation | Always |
| **Decoder** | Mention expansion; constrained select-among-k with NIL | Ambiguous band only |

## The dense channel

### Vector table shape

sqlite-vec v0.1.6 is statically linked (`SQLITE_VEC_VERSION "v0.1.6"`,
[`third_party/sqlite_vec`](../../third_party/sqlite_vec)) and registered
process-wide through `sqlite3_auto_extension`. It provides the `vec0` virtual
table with typed vector columns (`float[N]`, `int8[N]`, `bit[N]`), a choice
of distance metric, partition keys, filterable metadata columns, and
auxiliary columns (`+name type`) that ride along without being indexed.

**The table as built** (`stemma_ingest::build_dense_index`):

```sql
CREATE VIRTUAL TABLE vec_dense USING vec0(
    embedding  float[1024],
    src_table  text,
    src_column text,
    src_rowid  integer
);
```

One decision in it is load-bearing and permanent: **the dimension is in the
type**, so a vector of the wrong size fails at insert rather than producing a
meaningless distance. That is the structural half of vector-space hygiene;
`model_registry` is the bookkeeping half, and neither substitutes for the
other.

The metric is the `vec0` default, L2, with the pipeline converting to cosine
analytically — exact for unit vectors, since `cos = 1 − d²/2`.

**Three things the design wants that the built table does not have yet:**

- **A generation suffix in the name.** `vec_regulations_text_v1` /
  `_v2` is what makes blue-green a rename rather than a rebuild; `vec_dense`
  is a single fixed name, so promotion drops and recreates.
- **`src_rowid integer partition key`.** A partition key would let a probe
  restrict its scan, which matters a great deal given that `vec0` KNN in
  v0.1.6 is a full table scan — see
  [03-resolution.md](03-resolution.md#complexity). Partitioning by
  `src_table` is the obvious first cut for a multi-table corpus.
- **Auxiliary columns** (`+name type`) for payload that rides along without
  being indexed.

`vec_slice()` is available in this build, which matters for Matryoshka-style
encoders [Kusupati 2022]: a 1024-dimension vector can be truncated to
256 or 512 at query time for a cheap first pass, with the full dimension used
for rescoring, without storing three copies. Nothing uses it yet, and it is
the cheapest available answer to the full-scan cost — a 256-dimension first
pass reads a quarter of the bytes.

### Model identity

Every vector table has exactly one row in `model_registry`, keyed by the
table name
([02-data-model.md](02-data-model.md#model_registry)):

```
vector_table | backend | model                 | revision | dimension | quantization
vec_dense    | staged  | Qwen3-Embedding-0.6B  |          | 1024      | f32
```

That is the row a promotion writes today — `backend = 'staged'` because the
vectors came from an external loader rather than a live service, and
`revision` empty because the parquet metadata that carried the identity
(`embedding_model`, `embedding_dim`, `embedding_dtype`) has no revision
field. **The empty revision is the weak link**: it means two checkpoints of
the same base model are indistinguishable in the registry, which is exactly
the distinction the tuned-encoder work of the next section depends on. A
loader staging `emb-1024-1M-v3` rather than `emb-1024-1M` should be recording
that, and it has nowhere to put it.

The identity tuple `(backend, model, revision)` is exactly what the Embedder
service returns from `ModelInfo`
([`embedder.proto`](../../proto/stemma/v1/embedder.proto)), so a stored
vector table can always be traced back to a running service, and a service
whose identity no longer matches the registry is a detectable mismatch rather
than a silent one.

The invariant this enforces: **vector spaces are never mixed.** Cosine
similarity between vectors from two different models is not noisy — it is
meaningless. A single row embedded by the wrong model does not degrade
retrieval slightly; it produces a distance that has no relationship to
semantic similarity, and it is invisible unless the identity is recorded.

### What gets embedded

**As built: documents, raw, one vector per document cell.** The queue path
embeds exactly the cells the lexical index classified `is_doc` — long text
that mentions resolve *into* — and embeds them verbatim, with no instruction
prefix. That is the asymmetric convention of instruction-tuned retrieval
encoders: `format_query()` decorates the query-time mention, documents stay
raw, and applying the instruction on the document side would desert the
space the queries live in. Short values are deliberately not embedded: the
exact and trigram channels already serve them, and dense retrieval pays its
full-scan cost only where meaning is spread through prose. Documents longer
than the encoder's context are truncated by the endpoint today — chunking is
designed, not built.

The *designed* unit is broader: the **serialized row**.
`embed_queue.serialized` holds the text the embedder will see when it is
set; document items leave it empty and the drain fetches the stored value
from `lex_values`, so the store keeps one copy of each document. For a
value-shaped table the designed serialization is a compact rendering of the
row's identifying columns — *"the Seattle office"* is about a row, and the
semantic content that makes `offices` row 17 the right answer is spread
across `name` and `city`. That serialization pass does not exist yet; the
column is where its output will land.

### Two ways vectors get in

**Built: external staging.** A loader writes `vec_staging` — a plain table,
so it needs no sqlite-vec — and the server promotes it into `vec_dense` at
startup. Mechanics in
[02-data-model.md](02-data-model.md#vec_staging-and-vec_dense). This path
exists because the vectors for the first corpus already existed; it does not
require an embedding service at all, and it is what makes an offline-computed
1024-dimension map usable in minutes rather than GPU-hours.

**Built: the queue and the drain.** This is the path a corpus without
pre-computed vectors needs. At server startup, when an embedder is
configured, each database gets a background task on its own store connection
(the store is WAL, so serving never blocks on it):

1. `stemma_ingest::enqueue_missing_embeddings` inserts a pending
   `embed_queue` item for every document cell with no `vec_dense` vector —
   idempotent via the unique provenance key, and a `done` item is only reset
   if its vector has since disappeared.
2. `stemma_ingest::drain_embed_queue` repeats until the queue is empty:
   take a batch of 32 pending items (least-retried first), fetch their raw
   document text, embed through the `Embedder`, insert into `vec_dense` —
   creating the vec0 table at the embedder's observed dimension and its
   `model_registry` row on first use — and mark the items `done`. Progress
   (`queued`, `drained`, `failed`, `remaining`) is logged per batch, and the
   task exits when the queue is empty; there is no polling loop.

Model identity is checked before any embedding work: if `model_registry`
already binds `vec_dense` to a *different* model, the drain marks every
pending item `failed` with the mismatch spelled out and errors — refusing
loudly beats mixing vector spaces. A staged table and a live embedder of the
*same* model compose: the drain tops up whatever staging did not cover.

The failure semantics are the point: **writes never wait on a model.** If
the embedder is down, slow, or being replaced, items stay pending (bounded
by a retry budget of `EMBED_MAX_ATTEMPTS = 3`, after which they are marked
`failed` with an error note rather than retried forever) and retrieval
degrades to lexical-plus-KG. Resolution still works, the next server start
picks the queue back up, and the queue's status column keeps the whole
story queryable in plain SQL.

Honest limits of what is built: the drain runs at startup and exits — no
watcher notices data changes afterward, and re-embedding after an edit means
restarting the server; there is no online re-embed and no blue-green
generation swap (the next section is still designed, not built); and only
documents flow through the queue — the value-shaped serialized-row path has
a column reserved and no code.

### Query-time requirements

The dense channel is only correct if the query encoder and the index encoder
agree on everything:

1. **Same model, same revision.** *Designed:* checked against
   `model_registry` before the channel runs, with a mismatch disabling the
   channel rather than returning garbage. **Not implemented.** Today the
   server's `--embed-model` and the registry's `model` are independent
   strings, and pointing the flag at a different model than produced
   `vec_dense` yields silently meaningless distances. Of everything listed in
   this document as outstanding, this is the one that fails quietly, and the
   check is four lines.
2. **Same instruction prefix.** Instruction-tuned retrieval encoders — the
   Qwen3-Embedding family among them [Zhang 2025] — expect an
   asymmetric prompt: a task instruction on the query side, documents raw.
   `stemma_embed::format_query()` implements it, and the same asymmetry was
   used when the vectors were produced. Getting this wrong is the most common
   way to lose most of an encoder's quality while everything still runs. It
   is currently a hard-coded string in the crate; it belongs in the registry
   beside the model identity, because a different encoder wants a different
   instruction.
3. **Same normalization and pooling.** Handled inside the Embedder backend,
   which is why `Embedder` is a trait with a model-identity method rather
   than a raw HTTP call. Note the pipeline's L2→cosine conversion assumes
   unit-length vectors; an unnormalized backend breaks it silently, and
   `vec_normalize()` is available but unused.
4. **Same dimension**, structurally enforced by the `float[N]` column type —
   the one requirement on this list that cannot be violated silently.

### Entering fusion

The dense channel joins as a fourth channel in
[reciprocal rank fusion](03-resolution.md#stage-5--reciprocal-rank-fusion) —
and this is where adding it created a debt that has not been paid.

The normalization denominator is the literal constant `3/K`: three channels
at rank 0 gives `base = 1.0`. That was calibrated when there were exactly
three channels, and it encodes the fact that **documents cannot reach the
exact channel**, so their base topped out at 2/3 and their score at
0.85 × 2/3 = 0.567 — comfortably below the 0.9 exact floor.

The denominator did not change when the fourth channel landed. Documents can
now reach BM25, trigram *and* dense at rank 0, so:

$$ \mathrm{base}^{\text{doc}}_{\max} = \frac{\tfrac14 + \tfrac14 + \tfrac14}{3/4} = 1.0
\quad\Longrightarrow\quad \mathrm{score}^{\text{doc}}_{\max} = 0.85 $$

and a non-document value matched by all three non-exact channels can reach
`1.0 × (0.4 + 0.6·affinity)` — up to 1.0, tying an exact match. The
[score bands](03-resolution.md#reachable-score-bands) were documented as an
invariant precisely because this is the kind of change that is silent: no
test asserts absolute scores, so nothing failed. Re-deriving the denominator
and the 0.85 / 0.9 constants against four channels is outstanding.

The general lesson for a fusion stage: **a normalization constant that
encodes the channel count is a coupling between the fusion rule and the
retrieval topology**, and adding a channel is therefore a scoring change
whether or not anyone edits the scoring code. Deriving the denominator from
the number of channels that actually fired, rather than from a literal, would
remove the coupling entirely.

A cross-encoder rerank (`EmbedderService.Rerank`) is designed as a final pass
over the fused top-*n*. It is the highest-precision and highest-cost stage of
the retrieval half, and like every other stage it should produce evidence — a
`SemanticMatch` with the model identity and the score. Neither the rerank nor
the `SemanticMatch` emission exists; dense hits currently ride in
`LexicalMatch` with `channel = "dense"`.

### Blue-green re-embedding

*Designed.* What exists today is a **drop-and-recreate** promotion:
`build_dense_index` drops `vec_dense` and rebuilds it from staging at
startup, so swapping encoders means stopping the server, re-staging, and
restarting. That is safe — there is never a moment when two vector spaces are
queried together, which is the property that matters most — but it is an
outage, and it cannot run while serving.

The designed version. Changing the encoder is a data migration, and the
registry's table-keyed design makes it a safe one:

1. Create `vec_<table>_<column>_v2` with the new dimension, and insert its
   `model_registry` row with the new identity.
2. Enqueue every row; drain into `_v2` while `_v1` continues serving.
3. Validate: row count parity with the source, and a retrieval smoke test on
   a held-out query set (the same evaluation machinery as
   [06-evaluation.md](06-evaluation.md)).
4. Switch the resolver to `_v2` — a registry-driven lookup, so the switch is
   a single row update.
5. Drop `_v1`.

At no point are both tables queried together. The failure mode this design
exists to prevent is not "the new model is worse" — that is caught in step 3
— but "half the corpus is in one space and half in another", which is silent,
produces plausible-looking results, and is nearly impossible to diagnose from
retrieval output.

## Embedding-space crowding

The dense channel is only worth building if it discriminates. On the corpora
stemma is aimed at, a general-purpose encoder often does not, and the reason
is geometric.

### The problem

Take the legal corpus: 92,696 documents, every one of them regulatory prose
in the same register, with the same citation formatting, the same procedural
boilerplate, the same modal verbs. Encode them with a general-purpose
retrieval model and they land in a narrow region of the space. Pairwise
cosine similarities that should span a wide range instead cluster tightly and
high — every document looks somewhat like every other document, because on
the axes the general model learned, they *are* all alike. Between "the
regulation about coastal development permits" and "the regulation about
insurance filing deadlines" the model has a small angle to work with, and
retrieval discrimination is exactly that angle.

This is embedding-space crowding: **the corpus occupies far less of the
representation space than the space could give it, so the encoder's
resolution is spent representing distinctions the corpus does not contain
and starved for the distinctions it does.**

### Why this is the expected behaviour, not a defect

The geometry has been documented for years, from several directions.

Contextual embeddings are **anisotropic**: representations from pretrained
language models occupy a narrow cone rather than the full sphere, so two
random words have far higher expected cosine similarity than chance
[Ethayarajh 2019]. The training objective produces this — the *representation
degeneration problem* [J. Gao 2019] shows that the standard likelihood
objective with tied output embeddings pushes representations toward a narrow
cone as a direct consequence of the loss, not as an artefact of the data, and
later work locates part of the effect in self-attention itself
[Godey 2024]. Some of the measured anisotropy turns out to be a few
**rogue dimensions** dominating the cosine [Timkey 2021], and
the space is better described as clusters on a manifold than as one uniform
cone [Cai 2021].

Sentence-embedding work attacked it geometrically: removing the top dominant
directions [Mu 2018], mapping the distribution to a Gaussian with a
normalizing flow [Bohan Li 2020], and achieving most of the same gain with
a linear whitening transform [Su 2021]. SimCSE then reframed the whole
thing as a trade-off between **alignment** (similar pairs close) and
**uniformity** (representations spread over the sphere) [T. Gao 2021],
importing the framework from contrastive representation learning
[T. Wang 2020] — where the failure mode has its own name, *dimensional
collapse* [Jing 2022]. High-dimensional retrieval has its own version,
**hubness**: a few points become the nearest neighbour of disproportionately
many queries, and the effect worsens as intrinsic dimensionality rises
relative to extrinsic [Radovanović 2010]. And a standing caution over
all of it: cosine similarity is only "similarity" to the extent the training
objective made it so [Steck 2024].

Two things follow, and the second is the one that matters here.

**First**, a general-purpose encoder allocates its capacity across the
diversity of its training distribution — web text, code, dialogue, news. When
the deployment corpus is a *single narrow slice* of that distribution, most
of the learned axes are inert. The corpus's own internal variation, which is
the only variation that matters at retrieval time, is compressed into
whatever is left.

**Second**, isotropy repairs help but are the wrong shape of fix. Whitening
and flow-based mapping spread out the distribution you already have; they
cannot manufacture axes that separate documents the encoder never learned to
distinguish. If *coastal development permit* and *insurance filing deadline*
are nearly collinear in the source space, no linear transform makes them
orthogonal in a way that generalizes. The fix has to change what is encoded,
which means changing the encoder.

### stemma already measures the same uniformity, lexically

The knowledge compiler's document-frequency ceiling
([04-knowledge-graph.md](04-knowledge-graph.md#step-1--candidate-shortlist-a-df-ceiling-plus-burstiness))
exists because in a single-domain corpus, terms appearing in more than a
quarter of documents are corpus stopwords no matter how domain-specific they
look — *shall*, *pursuant*, *subdivision*, *section*. The compiler has to
discard them explicitly because frequency, the usual importance signal, is
inverted in a uniform corpus.

That is the *lexical* shadow of the same fact that causes crowding in the
*dense* channel. A corpus whose vocabulary is uniform enough to need a DF
ceiling is a corpus whose embeddings will be uniform enough to need a tuned
encoder. It also gives a cheap prior: the DF distribution the compiler
already computes is a corpus-uniformity diagnostic available before a single
vector is produced.

### Diagnostics worth recording

Before and after any encoder change, on a sample of the corpus:

- **Mean and spread of pairwise cosine similarity.** Crowding shows as a high
  mean with a small standard deviation. This is the direct measurement.
- **Effective rank** [Roy 2007] and **participation ratio** of the
  embedding matrix's singular values — how many dimensions the corpus
  actually uses. IsoScore is the calibrated alternative when a single
  0–1 utilization number is wanted [Rudman 2022].
- **Hubness**: the skew of the *k*-occurrence distribution (how often each
  point appears in others' top-*k*) [Radovanović 2010]. A long right
  tail means a few documents are absorbing queries they have nothing to do
  with.
- **Retrieval recall@k on held-out queries**, which is the only one that
  settles anything. The geometric measures explain *why* a model is bad; only
  recall says whether it is.

These belong in the `model_registry` row's neighbourhood as recorded
properties of a vector generation, so that a blue-green swap can be justified
by numbers rather than by belief.

## ambit: a measurement instrument

[ambit](https://github.com/pedapudi/ambit) is the sibling project for this
problem, and the first thing to be clear about is what it is not. It is a
**numpy-first Python library with a thin CLI** — one hard dependency, `numpy`
— that reads *already-embedded vectors* and renders a self-contained HTML
report. It does not train models. It does not serve them. It has no model
registry. Its framing, in its own words: *"ambit tells you where an embedded
dataset is too crowded to work — and which items are in trouble."*

```sh
ambit info   embeddings.parquet                        # streaming scan, scalars
ambit report embeddings.parquet --id-col uuid --out report.html
ambit report a.parquet --compare b.parquet --id-col uuid --out diff.html
```

with `ambit.diagnose()` / `ambit.report()` / `ambit.scan()` in Python. Nothing
is read from the environment; a `Config` object or keyword overrides drive
everything. It also ships `ambit embed`, which produces vectors through an
OpenAI-compatible `/v1/embeddings` endpoint — the same protocol stemma's
`Embedder` backend speaks.

Its thesis is that **crowding is a loss of resolution**: when items pack too
tightly, cosine similarity can no longer tell them apart. Three properties of
that framing matter here.

**It is relatedness-agnostic.** The harm is *unresolvability*, not density.
A tight group of genuinely related items is still a group a query cannot rank
within — which is exactly stemma's problem when a mention has to pick one
regulation out of forty near-identical ones.

**Averages cannot see it, and there is arithmetic for that.** A planted pocket
of 200 near-duplicates in a 4,000-item corpus (median intra-pocket cosine
0.929) moves the mean pair cosine by about +0.002 — roughly 6% of one null
standard deviation, invisible without a calibrated test and uninformative
about *which* items failed. In a matched power study the continuous layer
detects a 20-item (1%) pocket at power 1.00 where mean-cosine detection has
power 0.04 and hubness 0.18.

**High dimension is a blessing, not a curse.** At d = 1024 the null standard
deviation of a random pair cosine is 1/√1024 ≈ 0.031, so a healthy corpus has
essentially *no* close pairs and every close pair is a finding.

### The doctrine: cheapest sufficient fix wins

This is the part of ambit's design most likely to be misread, so it is worth
stating flatly: **ambit does not argue that corpus-tuned embedding models fix
crowding.** Its documented ordering puts training *last*:

| diagnosis | licensed fix |
|---|---|
| Data hugs the anisotropy-matched reference (a cone and nothing more) | **Do not tune.** Mean-centering, all-but-the-top [Mu 2018], light whitening [Su 2021; Huang 2021]. Re-measure. |
| Pockets born near distance 0 (near-duplicates) | **Fix the data, not the model.** Deduplicate, or accept the pocket as one entity. Training against true duplicates wastes gradient and hurts recall. |
| Genuine clumping beyond the cone at moderate scale | **Only this licenses training.** |

The evidence for the cheap path is strong: on real corpora, mean-centering
plus all-but-the-top *"removes ~100% of the impostor floor and widens margins
4× … no training, no labels."* ambit's technical report lists model-versus-data
attribution — *"the fraction of measured crowding removable by a budgeted
transform class, computed rather than predicted"* — as future work, not as a
settled result.

For stemma this ordering is a design constraint, not a footnote. It means the
first response to a crowded corpus is a linear transform applied at index and
query time, recorded in `model_registry` as a property of the vector
generation — not a fine-tuning run. It also means "tune an encoder per corpus"
is a conclusion a measurement can *refuse*.

### What ambit measures, and why it is exact in stemma's regime

The load-bearing quantity is a collision probability with a closed form.
Model a query as its target plus isotropic noise, `q = x + σg`. A competitor
`y` beats the target exactly when the noise crosses the halfway plane between
them, so

$$ P(y \text{ beats } x) = \Phi\!\left(-\frac{\lVert x-y \rVert}{2\sigma}\right) $$

**exactly, in any dimension** — verified by simulation to a maximum deviation
of 9×10⁻⁴. Summing over competitors gives the *confusion functional*

$$ C(\sigma) = \sum_{j \ne i} \Phi\!\left(-\frac{\lVert x - x_j \rVert}{2\sigma}\right) $$

— the expected number of wrong items outranking the right one — and **σ\*** is
the largest σ at which `C(σ) ≤ 1`. It is the corpus's **noise budget**: how
much query sloppiness the corpus tolerates before wrong items win. Being a
union bound, it is conservative under any competitor correlation, which makes
it a guarantee rather than an estimate.

Two consequences are what make this the strongest hook between the two
projects.

**First, σ\* is exact — not extrapolated — precisely in stemma's
instance-layer regime.** ambit is explicit about its scope: everything treats
*the corpus as its own query population*, which "covers dedup, clustering,
related-item retrieval, and corpus-as-queries search directly", and
extrapolating to an external query workload assumes that workload lands where
the documents are. The technical report is blunter: the treatment is "exact
for deduplication, clustering, and corpus-as-queries retrieval."

That is a description of
[the knowledge graph's instance layer](04-knowledge-graph.md#instance-layer):
alias clustering, and entity resolution across rows — deciding that *Wei Chen*
in one table and *W. Chen* in another are one referent. Records are queried
against records. **For stemma's document retrieval the σ\* number is an
informative proxy; for stemma's entity resolution it is the right number,
exactly.**

**Second, near-duplicates are provably the worst pathology.** As the distance
between two items goes to zero, Φ(−r/2σ) → 1/2 *at every σ*: a near-duplicate
is a coin flip at any noise level, no matter how good the encoder. That is the
formal reason a duplicate pocket cannot be fixed by tuning, and it is why
ambit's decision tree routes duplicates to deduplication rather than to
training.

It is also the one place where the confusion functional and the
alignment/uniformity framework [T. Wang 2020] **disagree**, and the
disagreement is entity-resolution-shaped. Across eight synthetic corpus types
the two agree on 27 of 28 pairwise orderings (rank correlation 0.978). The
single flip is "cone plus duplicate pocket" (C = 17.78) versus "low-rank
collapse" (C = 15.49): the confusion kernel calls the duplicate corpus worse,
uniformity calls it better. For a system whose job is to tell records apart,
the confusion ordering is the operationally correct one — a duplicate pocket
is unresolvable, and a low-rank space merely has less room. (The two are not
unrelated: by a Chernoff bound, uniformity at t = 2 *is* a collision bound at
σ = 0.25.)

### The division of labour ambit implies

ambit names the confusable records. Its per-entity fields report, by id, the
radius each record needs to gather a fixed share of the corpus, and the
expected collision count at σ\*. Its pocket detector surfaces tight groups
with birth and death scales and lists their members by id.

What it does not have — verified by grep across its documentation — is any
notion of *linking*: no pair-linking, no transitive closure, no blocking keys,
no match/non-match decision. It produces a diagnosis, not a resolution.

**That is exactly the seam stemma fills.** ambit says *these records are
unresolvable at this noise budget*; stemma's entity-resolution and coherence
layer decides *which of them are the same referent, and with what evidence*.
The two halves compose without overlap, and the compose point is concrete: a
record ambit flags with a high expected collision count is a record whose
alias edges should carry a lower confidence, and a pocket ambit names is a
candidate cluster for the instance layer to adjudicate rather than a set of
independent rows.

Three honest limits on the diagnosis side, worth knowing before building on
it: the merge tree runs on a subsample of at most 4,096 points, so pockets are
a profile over a reservoir rather than a full-corpus clustering; σ\* is a
union bound and therefore a conservative screen; and the pocket detector's
minimum size of 8 means duplicate *pairs* and *triples* never appear as
pockets at all — they surface only in the low tail of the per-entity field.

### The legal corpus, measured

The corpus in this repository is a subset of one ambit has been run over. The
California Code of Regulations slice is row-for-row the same 57,523 records as
`legal.db`'s `regulations` table (verified by uuid join, below). The federal
half differs: this repository's `sections` table is the Nemotron **eCFR**
subset, while the ambit map's fourth subset is **eCFR-QA**, a question-answer
derivative.

Base-model geometry — Qwen3-Embedding-0.6B [Zhang 2025] at 1024 dimensions
over the full 1,000,000-vector map:

| | base |
|---|---:|
| mean random-pair cosine | **+0.236** (null sd ≈ 0.031 at d = 1024) |
| crowding onset | ≈ cos **+0.82**, global rank-envelope p = **0.010** [Myllymäki 2017] |
| resolution bandwidth σ\* | **0.123** vs **0.148** for a well-spread corpus |
| — noise budget retained | **83%** |
| effective rank | 728 of 1024 |
| 90% of variance in | 361 dims |
| IsoScore | 0.090 |
| kNN@10 hubness skew | +1.90 |
| kNN@10 same-subdomain purity | 0.89 |
| centroid cosine, CA-Regs ↔ eCFR-QA | **0.81** |

Read the last row first. California's state regulations and the federal
eCFR-QA set are *different corpora about different law*, and the base encoder
places their centroids at cosine 0.81 — nearly collinear. A mention that
should discriminate between state and federal regulation has very little angle
to work with.

The σ\* line is the one that translates to stemma directly: the corpus retains
83% of the noise budget a well-spread corpus of the same size and shape would
have. That is a real but not catastrophic loss — which is itself a useful
result, because it says this corpus is a *cone* problem more than a *pocket*
problem, and ambit's own decision tree routes cone problems to centering and
whitening rather than to training.

The per-entity field names names. The most crowded records carry expected
collision counts of about 12 at σ\* (uuids `9b3dd305…`, `a310931d…`), against
a most-isolated tail near 1.21 on the same radius scale; the most prominent
pocket holds 204 sampled points, forming at radius 0.41 and persisting for
0.09. Those are the records for which a lexical mention will not discriminate,
listed by the identifier stemma joins on.

Two caveats on citing these numbers. The occupancy z-score of −4,211 that
appears in the same report is **a test statistic, not an effect size** — it
grows with the pair-sample count and is comparable only at matched sampling.
And the report-level numbers here are for the base model only: the tuned
models' reports were rendered with `--approx 200000`, whose reservoir happened
to draw only two of the four subsets, so their header figures are not
comparable to the base report's. The cross-model table below comes from a
separate script run identically over the full map.

### Three rounds of tuning, and a split verdict

**These models were not trained with ambit.** They were trained with
sentence-transformers — `MultipleNegativesRankingLoss` inside `MatryoshkaLoss`
(1024/768/512/256) for v1 and v2, a custom multi-positive InfoNCE with
false-negative masking for v3 — as full fine-tunes of Qwen3-Embedding-0.6B, no
LoRA, last-token pooling with left padding, global batch 256 across two GPUs,
lr 2e-5, two epochs. ambit was the **audit layer**: it measured the corpus
before, and compared the checkpoints after.

Geometry, from one script run identically over all four full 1M maps:

| | base | v1 | v2 | v3 |
|---|---:|---:|---:|---:|
| mean random-pair cosine | +0.236 | +0.181 | **+0.147** | +0.212 |
| kNN@10 hubness skew | +1.90 | +2.27 | **+1.50** | +1.74 |
| kNN@10 purity | 0.89 | 0.91 | 0.81 | **0.90** |
| CA-Regs ↔ eCFR-QA centroid | 0.81 | **0.66** | 0.78 | 0.78 |

Retrieval, on held-out splits (1,000 Set A questions over 35,173 eCFR
sections; 747 single-doc and 749 multi-doc Set B questions over a ~1.035M-doc
corpus):

| | base | v1 | v2 | v3 |
|---|---:|---:|---:|---:|
| **Set A** (trained task) R@1 | 0.288 | **0.355** | 0.336 | 0.350 |
| Set A R@10 | 0.750 | **0.840** | 0.832 | 0.829 |
| Set A mean rank | 345 | 210 | 240 | **197** |
| **Set B** single-doc R@10 | **0.952** | 0.830 | 0.906 | 0.918 |
| **Set B** multi-doc set-Recall@10 | **0.687** | 0.366 | 0.481 | 0.487 |
| Set B multi-doc set-Recall@100 | **0.872** | 0.634 | 0.790 | 0.814 |

The verdict is split, and the project states it that way: **ship v3 for
single-target legal retrieval; for multi-hop retrieval the base model still
wins.** Three lessons transfer directly into stemma's design.

**Better isotropy is not better retrieval.** v1 improved essentially every
geometry number — the cone opened, the regulatory family separated 0.81 → 0.66,
participation ratio rose from 93 to 115 of 1024 dimensions — and *regressed*
open-corpus retrieval badly (multi-doc set-Recall@10 0.687 → 0.366). The
project's own summary: "better isotropy ≠ better retrieval on every task."
A blue-green swap justified by mean cosine alone would have shipped it.

**Per-corpus tuning is per-*task* tuning in disguise.** v1's gains were
eCFR-task-aligned and did not transfer. For stemma this means a tuned encoder
is registered against the corpus *and* the retrieval pattern it was tuned for,
and a corpus serving several patterns may need more than one vector
generation.

**The residual gap was data, not geometry and not the loss.** v3 restored kNN
purity to base level *and* used a provably correct multi-positive objective,
and multi-hop@10 still did not move (0.487 vs v2's 0.481; only deeper k
improved). The cause was training-set composition — only 1,808 of 48,413
queries (3.7%) were multi-positive, so the single-target signal dominated.
That is a negative result about *measurement-guided tuning generally*: the
geometry was fixed and the retrieval metric did not follow, because the
limiting factor was never the geometry.

*Provenance note: the geometry table is reproduced from analysis outputs on
this machine; the retrieval table and the training configuration are reported
by the project's own write-up and were not re-run here.*

### When measurement licenses training: ambit's hooks

When the diagnosis does land in the third row of the decision table, ambit
ships the pieces to aim a fine-tune — about 200 lines, deliberately small:

- **`resolution_weights(X, sigma, floor=0.25)`** — sampling weights
  proportional to per-entity collision counts, with a uniform floor so the
  bulk stays anchored. The items measured to be in trouble get oversampled.
- **`mine_confusable_negatives(X, cos_window=(liftoff, 0.98), guard_top_m=5,
  per_anchor=8)`** — negatives drawn from the measured confusable window, with
  a **false-negative guard**: an anchor's top-*m* base-model neighbours are
  never negatives, because that window is exactly where unlabelled true
  relatives live. For stemma this guard is not optional — in the legal corpus,
  82% of gold documents are shared by two or more queries.
- **`confusion_loss(z, sigma, exclude)`** — a batch estimate of `C(σ)/(n−1)`.
  Its **gradient locality is a theorem**, not a hope: the derivative of
  Φ(−r/2σ) decays as exp(−r²/8σ²), so pairs beyond ≈3σ contribute vanishing
  gradient, and the loss provably cannot disturb global dissimilarity
  structure. It widens margins *inside* the confusable window and nowhere
  else.
- **`preservation_loss(z, z_base)`** — per-anchor KL from the frozen base
  model's in-batch similarity distribution. "Don't hurt similarity" as a
  differentiable statement.

`σ` comes from the measurement, and the window's lower bound is the measured
crowding onset — neither is a swept hyperparameter. The published effect is
**synthetic only**: on a planted 300-of-4,000 pocket at d = 64, a linear
adapter reduced flagged-item median expected collisions from 2.59 to 1.25 at
held-out neighbour overlap 0.92. That is the honest state of the training
layer, and it is why this document treats measurement-guided tuning as a
designed path rather than a proven one.

The verification loop is the part stemma should copy regardless: hold out a
reservoir that neither mining nor batching ever sees, and grade every round on
it with a compare report — σ\* rising, crowding onset retreating toward higher
cosine, collision counts falling on the entities flagged in the first
diagnosis, and neighbour overlap against the base staying high. That last one
is the drift alarm; if it collapses you are re-skinning the space, not
repairing it.


### The integration

**One tuned encoder per corpus, registered as a model identity.** stemma
already keys vector tables by model identity and already refuses to mix
vector spaces. An ambit-tuned encoder is just another `(backend, model,
revision)` in `model_registry`; nothing in the resolver knows or cares that
the model was tuned rather than downloaded. The Embedder trait's
`ModelInfo` is what carries the identity across, and the `revision` field is
where a tuning run's identifier lands — which is what makes a tuned model
distinguishable from its own base model in the registry, and what makes two
tuning runs distinguishable from each other.

**Deployment is a re-embed.** Today that means: stop the server, stage the
other checkpoint's vectors, restart — the loader already takes
`--src emb-1024-1M-{tuned,v2,v3}` for exactly this. The designed version is
the blue-green swap above, where a new tuned checkpoint is a new vector table
backfilled while the old one serves, validated on held-out queries, switched
by a registry update, then dropped. That is also the A/B mechanism: two
vector tables from two checkpoints can coexist for as long as evaluation
needs, because they are separate tables with separate registry rows, and the
invariant that forbids *mixing* spaces is precisely what makes *comparing*
them safe. The single fixed `vec_dense` name is what currently stands between
the two.

**And the registry needs somewhere to put the checkpoint.** The four
generations of the legal encoder are all `Qwen3-Embedding-0.6B` by model
name; what distinguishes them is which fine-tune produced them. With
`revision` unpopulated, a store holding v3 vectors and a store holding base
vectors have identical registry rows. Given the numbers below — where v1 and
v2 differ by 0.32 absolute on multi-hop recall — that is not a cosmetic gap.

**The careg vectors are the first payload, and there are four of them.**

First, a correction worth making precisely, because the repository's own prose
invites the wrong reading. The careg **source parquet contains no embedding
column** — its schema is `text`, `license`, `metadata{category, models_used}`,
`uuid`, and nothing else. The vectors are a *separate, external artefact*,
uuid-aligned to it. What
[`build_careg_db.py`](../../eval/careg/build_careg_db.py) preserves is the
join key, and its docstring says exactly that: uuid is kept "so the
pre-computed embeddings … can be joined in later without re-embedding."

There are **four uuid-aligned vector sets**, one per encoder generation, each
verified against the database in this repository:

| set | `embedding_model` metadata | rows | uuid coverage of `regulations` |
|---|---|---:|---|
| `emb-1024-1M` | `Qwen3-Embedding-0.6B` (base) | 57,523 | **57,523 / 57,523 — 100%** |
| `emb-1024-1M-tuned` | `qwen3-emb-legal-v1` | 57,523 | **100%** |
| `emb-1024-1M-v2` | `qwen3-emb-legal-v2` | 57,523 | **100%** |
| `emb-1024-1M-v3` | `qwen3-emb-legal-v3` | 57,523 | **100%** |

All are `row_id int64`, `uuid string`, `subset string`,
`embedding fixed_size_list<float>[1024]`, with key-value schema metadata
`{embedding_model, embedding_dim: "1024", embedding_dtype: "float32"}`.

**This maps one-to-one onto the blue-green design, and is the reason that
design exists.** Four generations of the same 57,523 records, each carrying
its own model identity, is precisely the situation `model_registry` was shaped
for: four vector tables, four registry rows, no mixing, and an A/B comparison
that needs no embedding run. The **model identity travels with the vectors**
in the parquet metadata, so a registry row can be populated from the artefact
rather than from a human's recollection of which checkpoint produced it —
the same discipline `ModelInfo` enforces at runtime, arriving through a
different door.

It also sharpens the `revision` gap noted above: all four sets would land in
the registry as dimension 1024, quantization f32, and a `model` string that
distinguishes them only because the fine-tunes were given distinct served
names. Nothing structural prevents two generations of the *same* served name
from becoming indistinguishable.

The load path is a join, not an embedding run:

```sql
-- src.regulations(id, uuid, …) ⋈ external vectors keyed by uuid
INSERT INTO vec_regulations_text_v1 (src_rowid, embedding, +src_table, +src_column)
SELECT r.id, ext.embedding, 'regulations', 'text'
FROM src.regulations r JOIN ext.vectors ext ON ext.uuid = r.uuid;
```

Complete uuid coverage is what makes this a clean join with no partial-fill
case to handle — and a partially filled vector table is precisely the silent
half-and-half failure that the blue-green discipline exists to prevent. The
corpus-construction guideline *"keep stable identifiers … so external
artefacts can join back"*
([06-evaluation.md](06-evaluation.md#corpus-construction-guidelines)) is this
requirement, generalized.

Note what the current builders do **not** do: neither
[`build_careg_db.py`](../../eval/careg/build_careg_db.py) nor
[`build_legal_db.py`](../../eval/legal/build_legal_db.py) reads the embedding
column — they carry `uuid`, `text`, `license` and `category` into SQLite and
nothing else. That is correct as it stands: vectors are derived state and
belong in the `.stemmadb` store, not in the user database. The loader that
joins them into a `vec0` table is the piece that does not exist yet.

Because the same 1M map exists under four model generations
(`emb-1024-1M` base, `-tuned`, `-v2`, `-v3`), the careg slice is also a
ready-made A/B corpus: four vector tables, four `model_registry` rows, the
same 57,523 rowids, and a retrieval evaluation that can compare them without
re-embedding anything.

**Dimension and quantization are registry facts, not assumptions.** A
1024-dimension `f32` generation and a Matryoshka-truncated 256-dimension
generation are different rows in `model_registry` pointing at different
tables. `vec_slice()` in the linked sqlite-vec build makes truncation a query-
time operation when the encoder was trained for it [Kusupati 2022],
and `vec_quantize_int8()` / `vec_quantize_binary()` make quantized generations
expressible — with `quantization` in the registry recording which, so that a
distance computed against the wrong representation is impossible to produce
by accident.

### Where tuned encoders matter most: the instance layer

The dense channel's headline use is document retrieval, but the place a
corpus-tuned encoder is most decisive is
[the knowledge graph's instance layer](04-knowledge-graph.md#instance-layer):
**embedding-assisted entity resolution across rows** — deciding that
*Wei Chen* in one table and *W. Chen* in another are one referent, or that
two regulation sections describe the same regulatory object.

Crowding is worse here than for documents, for a structural reason. Entity
strings are short, share a naming convention, and come from a single
generator: a table of California agency names, a column of statute citations,
a roster of employee names. There is almost no lexical or topical variation
for a general encoder to grip, so the strings land closer together than
documents do, and the decision threshold that separates "same entity" from
"different entity in the same family" is finer than the encoder's usable
resolution. A tuned encoder that has seen this corpus's naming conventions
has a chance at that threshold; a general one is guessing.

This is also why the entity-resolution edges of the instance layer must carry
`{"method": "…", "confidence": …}` like every other edge
([04-knowledge-graph.md](04-knowledge-graph.md#provenance)): a similarity-derived
identity claim is exactly the kind of edge that should be weighted below a
declared one, and exactly the kind a consumer will over-trust if it arrives
unqualified.

## Decoder roles

Two invocation points, both narrow, both on the ambiguous band only, both
producing evidence.

### 1. Mention expansion, before retrieval

*"the crown"* → *"the British monarchy; royal institution"*.

The most promising use of a language model in an entity-linking pipeline is
not selection — it is giving the retriever something to retrieve with.
LLM-augmented entity linking works exactly this way: the LM writes context for
the mention, a specialized linker does the linking, and neither does the
other's job. [Xin 2025] reports an absolute 8.9% entity-linking accuracy gain
across six benchmarks — **measured against prior methods that integrate
tuning-free LLMs into entity linking, not against specialized linkers in
general.** The claim to take from it is that *this* way of using an LM beats
other ways of using an LM, which is the architectural point, not a claim that
LM augmentation beats everything.

For stemma the expansion is fed to the lexical and dense channels as
additional queries, and the results fuse into the same RRF as everything
else. The deterministic counterpart already exists in the knowledge graph —
co-occurring terms of a span are a corpus-grounded expansion available for a
graph lookup
([04-knowledge-graph.md](04-knowledge-graph.md#kg-guided-expansion)) — and it
should run first, because it is free and it cannot hallucinate. The LM
expansion is for mentions the corpus's own vocabulary cannot expand.

### 2. Constrained adjudication, after retrieval

The LM is shown *k* candidates with their evidence and asked to select one,
with a JSON-schema-constrained response over an enum of candidate ids plus an
explicit **NIL** option.

Three properties are non-negotiable:

- **Closed set.** The model chooses among presented options and cannot name a
  record that was not retrieved. This is what makes the constraint safe here
  and unsafe in generative retrieval: the enum is built from *this query's
  candidates*, so "valid" and "retrieved" are the same set, and the
  out-of-distribution failure mode of decoding over a static catalog does not
  arise.
- **NIL is a first-class choice**, surfaced as `Mention.nil = true` — the
  field already exists in the wire format
  ([02-data-model.md](02-data-model.md#resolveresponse--the-answer)). Forcing
  a choice when none is right converts a recoverable "I don't know" into an
  unrecoverable wrong answer. LMs are strong selectors and weak open-recall
  linkers; the design uses the first property and refuses the second.
- **The decision is evidence.** Every adjudication produces an
  `Adjudication { model, rationale }`, so an LM decision is inspectable on
  the same terms as a BM25 hit.

`ResolveOptions.allow_lm` gates the whole band. With it off, resolution is
purely lexical + dense + KG and fully local — no network, no model, no
non-determinism. That is the default posture, and the LM is an escalation.
(The field is currently accepted and ignored, since the band does not exist.)

### The `rewritten_query` artifact

`ResolveResponse.rewritten_query` is the query with its mentions substituted
by canonical values:

> *"the Q3 numbers for the Seattle office"*
> → *"the 2025Q3 numbers for the Seattle - Northgate office"*

It exists because the most common downstream consumer is a query generator,
and handing it a *pre-linked* question turns value linking from something it
must do into something it must merely transcribe. This is the
resolve-then-generate pattern [Talaei 2024] made concrete at the
interface: the artifact carries the linking, and the generator writes SQL
against a question whose oblique references have already been pinned.

Substitution is mechanical once resolution is done — mentions carry byte
offsets, candidates carry canonical values — but it is only *safe* once the
pipeline can express confidence and abstention properly. Substituting a wrong
value is worse than substituting nothing, because it launders a resolution
error into a question that then looks unambiguous. So the field stays empty
until adjudication and NIL exist: the substitution should happen for mentions
the pipeline is confident about and leave the rest verbatim, which requires
knowing the difference.

### What the decoder must never do

- It never enumerates candidates. Retrieval is the encoders' and the lexical
  channels' job.
- It never sees the whole database. It sees *k* candidates and their evidence.
- It never runs on the unambiguous band. An exact match scoring in [0.9, 1.0]
  does not need a model's opinion, and asking for one adds latency,
  non-determinism and a chance of being talked out of a correct answer.
- It is never required. Every stage above it produces a complete, usable
  resolution on its own.

## The consumption pattern

The agent and MCP layers are **built**, and they are the shape all of the
above is for.

[`integrations/mcp/stemmadb_mcp.py`](../../integrations/mcp/stemmadb_mcp.py)
exposes `resolve`, `sql`, `schema` and `knowledge_graph` over MCP, with the
server-level instruction:

> Before referring to any entity, value, table or column, pin it with
> resolve; cite resolutions as `table.column #rowid`. Use sql (read-only) to
> fetch what resolve pointed at — never invent identifiers.

This is **resolve-before-reference**, and it is what the whole design buys.
An agent that follows it cannot hallucinate an identifier: every table name,
column name and stored value it uses came from a resolution or a schema call,
each carrying the evidence that produced it, each citable as
`table.column #rowid`. The reference agent
([`agents/stemma_agent/agent.py`](../../agents/stemma_agent/agent.py))
restates the rule and adds the ambiguity discipline — *"if resolution is
ambiguous, say so and show the top candidates instead of guessing; if it
finds nothing, say that plainly"* — which is the consumer-side counterpart of
NIL.

Note the architecture this implies. The agent's own LM is a *decoder in the
select-among-k role*, one level up: stemma hands it candidates with evidence,
and it chooses and explains. That is the same division of labour as the
in-pipeline adjudication band, applied at the application layer, and it is
why the MCP `resolve` tool returns both a compact digest (for the model's
context) and the full `trajectory` (for the client's rendering). The model
gets the options; the human gets the trace.

## References

Full bibliography, with venues and identifiers verified:
[00-bibliography.md](00-bibliography.md). Works cited in this document:

- **Entity linking** — [Wu 2020] (BLINK), [B.Z. Li 2020] (ELQ),
  [De Cao 2021] (GENRE), [Ayoola 2022] (ReFinED), [Orlando 2024] (ReLiK),
  [Xin 2025] (LLMAEL).
- **Generative retrieval** — [S. Wu 2025].
- **Text-to-SQL** — [Talaei 2024] (CHESS).
- **Embedding geometry** — [Ethayarajh 2019], [J. Gao 2019], [Bohan Li 2020],
  [Su 2021], [Huang 2021], [T. Gao 2021], [T. Wang 2020], [Radovanović 2010],
  [Kusupati 2022], [Zhang 2025] (Qwen3-Embedding), and from section H:
  [Mu 2018], [Timkey 2021], [Cai 2021], [Godey 2024], [Jing 2022], [Roy 2007],
  [Rudman 2022], [Steck 2024], [Myllymäki 2017].
