# Encoders and decoders

**What is built, as of this writing:** the `Embedder` trait and its
OpenAI-compatible backend ([`stemma-embed`](../../crates/stemma-embed/src/lib.rs)),
the `vec_staging` → `vec_dense` promotion path with its `model_registry`
write, the targeted dense retrieval channel inside the pipeline, and the
consumption pattern (MCP surface and reference agent). The mechanics of the
built parts are specified in
[02-data-model.md](02-data-model.md#vec_staging-and-vec_dense) and
[03-resolution.md](03-resolution.md#stage-4b--the-dense-channel-targeted);
this document gives the *why*, and everything it describes beyond those
mechanics — index-time embedding through the queue, online blue-green swaps,
cross-encoder reranking, and the entire decoder half — is **designed, not
built**. `stemma-lm` is still a one-line placeholder.

The design rests on one division of labour, and this document argues it,
specifies both halves, and then addresses the failure mode that decides
whether the encoder half works at all: **embedding-space crowding**.

## The division of labour

> **Encoders do retrieval. Decoders decide among presented options. The LM is
> never the retrieval mechanism.**

The entity-linking literature converged on this by trying the alternative.
The dense retrieve-then-rerank lineage — BLINK [Wu et al. 2020], ELQ
[Li et al. 2020], ReFinED [Ayoola et al. 2022], ReLiK [Orlando et al. 2024]
— beats generative entity linking on accuracy *and* on latency, and the gap
widens as the catalog grows. Autoregressive entity retrieval [De Cao et al.
2021] is elegant and constrains generation to valid identifiers with a
prefix trie; the problem is what "valid" buys you.

**Constrained decoding forces validity, not correctness.** A model
constrained to emit only identifiers that exist in the catalog will always
emit one that exists. When the right answer is not in its learned
distribution — because the catalog changed, or because the model never saw
this database — it emits a *confidently wrong but structurally valid*
identifier. That is strictly worse than an explicit no-match, because a
no-match is detectable downstream and a valid-but-wrong record is not. The
SIGIR 2025 analysis of constrained auto-regressive decoding for generative
retrieval [Wu et al. 2025] measures the cost directly: the constraint
that guarantees valid output also constrains the model *away from* correct
output, and the effect is systematic rather than incidental.

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
encoders [Kusupati et al. 2022]: a 1024-dimension vector can be truncated to
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

The unit is the **serialized row**, not the raw cell. `embed_queue.serialized`
holds the text that the embedder will see, produced at ingest time. For a
value-shaped table that is a compact rendering of the row's identifying
columns; for a document-shaped table it is the document, chunked if it
exceeds the encoder's context.

Serializing the row rather than the cell is what makes the dense channel
complementary to the lexical one rather than redundant with it. The lexical
channels index cells because evidence must cite `(table, column, value)`. The
dense channel embeds rows because *"the Seattle office"* is about a row, and
the semantic content that makes `offices` row 17 the right answer is spread
across `name` and `city`. Candidates from the dense channel therefore arrive
at row granularity and are attributed to a representative column when fused.

### Two ways vectors get in

**Built: external staging.** A loader writes `vec_staging` — a plain table,
so it needs no sqlite-vec — and the server promotes it into `vec_dense` at
startup. Mechanics in
[02-data-model.md](02-data-model.md#vec_staging-and-vec_dense). This path
exists because the vectors for the first corpus already existed; it does not
require an embedding service at all, and it is what makes an offline-computed
1024-dimension map usable in minutes rather than GPU-hours.

**Designed: the queue and the drain.**

```sql
INSERT INTO embed_queue (src_table, src_rowid, serialized) VALUES (…)
ON CONFLICT (src_table, src_rowid) DO UPDATE SET serialized = …
```

Ingest enqueues and returns. An async worker drains in batches through the
`Embedder`, writes into the current vector table, and deletes the queue rows
it consumed. This is the path a corpus without pre-computed vectors needs,
and nothing drains the queue today.

The failure semantics are the point: **writes never wait on a model.** If the
embedder is down, slow, or being replaced, the queue grows and retrieval
degrades to lexical-plus-KG. Ingest still completes, resolution still works,
and nothing surfaces an error to the user for a channel that is one of four.
The `UNIQUE (src_table, src_rowid)` constraint makes re-enqueueing idempotent,
so a crashed drain is recovered by restarting it.

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
   Qwen3-Embedding family among them [Zhang et al. 2025] — expect an
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
degeneration problem* [Gao et al. 2019] shows that the standard likelihood
objective with tied output embeddings pushes representations toward a narrow
cone as a direct consequence of the loss, not as an artefact of the data, and
later work locates part of the effect in self-attention itself
[Godey et al. 2024]. Some of the measured anisotropy turns out to be a few
**rogue dimensions** dominating the cosine [Timkey & van Schijndel 2021], and
the space is better described as clusters on a manifold than as one uniform
cone [Cai et al. 2021].

Sentence-embedding work attacked it geometrically: removing the top dominant
directions [Mu et al. 2018], mapping the distribution to a Gaussian with a
normalizing flow [Li et al. 2020b], and achieving most of the same gain with
a linear whitening transform [Su et al. 2021]. SimCSE then reframed the whole
thing as a trade-off between **alignment** (similar pairs close) and
**uniformity** (representations spread over the sphere) [Gao et al. 2021],
importing the framework from contrastive representation learning
[Wang & Isola 2020] — where the failure mode has its own name, *dimensional
collapse* [Jing et al. 2022]. High-dimensional retrieval has its own version,
**hubness**: a few points become the nearest neighbour of disproportionately
many queries, and the effect worsens as intrinsic dimensionality rises
relative to extrinsic [Radovanović et al. 2010]. And a standing caution over
all of it: cosine similarity is only "similarity" to the extent the training
objective made it so [Steck et al. 2024].

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
- **Effective rank** [Roy & Vetterli 2007] and **participation ratio** of the
  embedding matrix's singular values — how many dimensions the corpus
  actually uses. IsoScore is the calibrated alternative when a single
  0–1 utilization number is wanted [Rudman et al. 2022].
- **Hubness**: the skew of the *k*-occurrence distribution (how often each
  point appears in others' top-*k*) [Radovanović et al. 2010]. A long right
  tail means a few documents are absorbing queries they have nothing to do
  with.
- **Retrieval recall@k on held-out queries**, which is the only one that
  settles anything. The geometric measures explain *why* a model is bad; only
  recall says whether it is.

These belong in the `model_registry` row's neighbourhood as recorded
properties of a vector generation, so that a blue-green swap can be justified
by numbers rather than by belief.

## ambit: measuring crowding, and tuning against the measurement

[ambit](https://github.com/pedapudi/ambit) is the sibling project that
addresses crowding directly. Its framing, in its own words: *"ambit tells you
where an embedded dataset is too crowded to work — and which items are in
trouble."*

Two things about its thesis matter for how it slots into stemma.

**Crowding hides from averages.** A corpus can look healthy on every global
statistic while one pocket of it has collapsed — a small tight clump barely
moves a mean. So ambit measures occupancy *continuously over every scale*
(no histogram bins or grid cells, whose size and placement change the answer),
*against a calibrated null* (every number read against what a well-spread
dataset of the same size and shape would show), and *down to named items*
— not "this region is dense" but "these documents, by id, are each expected
to collide with about twelve others". The measurement is unsupervised: no
labels, no gold pairs.

**The operational unit is a noise budget, not a geometry score.** Model a
query as its target item plus noise of scale σ. A competitor at distance *r*
wins the retrieval exactly when the noise crosses the halfway plane between
the two items, with probability Φ(−r/2σ). Summing over competitors gives the
expected number of wrong items outranking the right one, and **σ\*** — the
largest noise at which that expectation stays ≤ 1 — is the corpus's
resolution bandwidth: *how much query sloppiness the corpus tolerates before
wrong items win*. That is directly the quantity stemma's dense channel cares
about, because an oblique mention is a noisy query by construction.

The surface is a CLI over a library:

```sh
ambit report embeddings.parquet --id-col uuid --out report.html
ambit info   embeddings.parquet                    # terminal scan, numbers only
ambit report encoder-a.parquet --compare encoder-b.parquet --id-col uuid \
             --out diff.html                       # same items, two encoders
```

with `ambit.report()` / `ambit.diagnose()` / `ambit.scan()` in Python and a
training module (`ambit.training`) that turns the measurements into gradient
signal: `resolution_weights` (oversample the items measured to be in
trouble), `mine_confusable_negatives` (draw negatives from the measured
confusable window, with a guard that never mines an anchor's top-*m* base
neighbours, because that window is exactly where unlabelled true relatives
live), `confusion_loss` (minimize expected collisions at the measured σ) and
`preservation_loss` (pin who-is-similar-to-whom to the frozen base model).

The discipline it enforces is the one stemma needs: **measure first, apply
the cheapest fix the measurement licenses.** ambit's own decision tree says
*don't tune* when the data merely hugs the anisotropy-matched reference —
mean-centering or light whitening recovers it — and *fix the data, not the
model* when the pockets are near-duplicates. Only genuine clumping beyond the
cone at moderate scale is a tuning case. That matters here because it means
"train a per-corpus encoder" is a conclusion a measurement can license or
refuse, not a default.

### What this looked like on stemma's own legal corpus

ambit has already been run over a corpus that **shares this repository's
`regulations` table exactly**. The measurements below come from a
1,000,000-vector map of the Nemotron legal corpus — California Code of
Regulations 57,523 · Case-Law Summary 53,137 · CaseHOLD 444,670 · eCFR-QA
444,670 — embedded at 1024 dimensions with **Qwen3-Embedding-0.6B**
[Zhang et al. 2025] and with three successive fine-tunes of it, recorded in
`analyze-{base,v1,v2,v3}.txt` and the corresponding `legal-qwen3-*` ambit
reports.

The California Code of Regulations slice is row-for-row the same 57,523
records as `legal.db`'s `regulations` table (verified by uuid join, below).
The federal half differs: this repository's `sections` table is the Nemotron
**eCFR** subset, while the ambit map's fourth subset is **eCFR-QA**, a
question-answer derivative. So the numbers below describe the *neighbourhood*
stemma's regulations live in, and describe the regulations themselves
exactly.

The base-model geometry is the crowding argument stated as data:

| | base (Qwen3-Embedding-0.6B) |
|---|---:|
| mean random-pair cosine | **+0.236** |
| kNN@10 hubness (k-occurrence skew) | +1.90 |
| kNN@10 same-subdomain purity | 0.89 |
| participation ratio | 93 / 1024 |
| effective rank | 728 |
| 90% of variance in | 361 dims |
| centroid cosine, CA-Regs ↔ eCFR-QA | **0.81** |

Read the last row first. California's state regulations and the federal eCFR
are *different corpora about different law*, and the base encoder places their
centroids at cosine 0.81 — nearly collinear. A mention that should
discriminate between state and federal regulation has almost no angle to work
with. Meanwhile a mean random-pair cosine of +0.236 with only 93 of 1024
dimensions carrying meaningful variance is exactly the anisotropic-cone
picture of [Ethayarajh 2019] and [Gao et al. 2019], measured on the corpus
stemma actually serves.

The three tuning rounds are worth recording honestly, because they are a
demonstration that this is a real engineering problem and not a switch:

| | base | v1 | v2 | v3 |
|---|---:|---:|---:|---:|
| mean random-pair cosine | +0.236 | +0.181 | **+0.147** | +0.212 |
| kNN hubness skew | +1.90 | +2.27 | **+1.50** | +1.74 |
| kNN@10 purity | 0.89 | 0.91 | 0.81 | **0.90** |
| CA-Regs ↔ eCFR-QA centroid | 0.81 | **0.66** | 0.78 | 0.78 |

- **v1** (eCFR pairs only, hard negatives mined with the base model, full
  fine-tune with `MultipleNegativesRankingLoss` under `MatryoshkaLoss` at
  1024/768/512/256) separated the regulatory family best — 0.81 → 0.66 — and
  won its trained task decisively (eCFR question→section Recall@1 0.288 →
  0.355, +23%; mean rank 345 → 210). It also *spiked hubness* to +2.27 and
  regressed everything it was not trained on: open-corpus single-doc
  Recall@10 0.952 → 0.830, multi-hop set-Recall@10 0.687 → 0.366. A clean
  specialist, and a clean case of catastrophic forgetting.
- **v2** folded the other domains back in under one uniform schema. It opened
  the cone the most (+0.147) and pushed hubness *below* base (+1.50) — but
  kNN purity fell to 0.81, neighbourhoods got mixed, and multi-hop stayed
  well under base.
- **v3** (multi-positive InfoNCE with false-negative masking — necessary
  because 82% of gold documents are shared by two or more queries) produced
  the cleanest geometry of any tuned model, restoring purity to base level
  (0.90) while keeping the cone open, with the best mean rank on the trained
  task (197) and near-full recovery of single-doc retrieval (R@10 0.918 vs
  base 0.952).

And the honest negative result: **v3's multi-hop Recall@10 did not move**
(0.487 vs v2's 0.481), even with purity restored and the loss provably
correct. The diagnosis was data composition — only 1,808 of 48,413 training
queries (3.7%) were multi-positive — not geometry and not the objective.

Three lessons transfer directly to stemma's design:

1. **Better isotropy is not better retrieval.** v2 had the best global
   geometry and the worst neighbourhood purity. A blue-green swap justified
   by mean cosine alone would have shipped it. The gate has to be held-out
   retrieval on the tasks the corpus is actually used for
   ([06-evaluation.md](06-evaluation.md)), with geometry as the explanation
   rather than the verdict.
2. **Per-corpus tuning is per-*task* tuning in disguise.** v1's gain was
   eCFR-task-aligned and did not transfer. For stemma this means a tuned
   encoder is registered against the corpus *and* the retrieval pattern it
   was tuned for, and a corpus serving several patterns may want more than
   one vector generation.
3. **The base model can win.** For multi-hop retrieval on this corpus, it
   does. A design that treats the tuned encoder as strictly better than its
   base would have no way to express that; a registry that treats them as two
   equally legitimate vector generations does.

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

**The careg vectors are the first payload, and they are ready.** The corpus
builders preserve `uuid` for exactly this reason —
[`build_careg_db.py`](../../eval/careg/build_careg_db.py) says so in its
module docstring, and
[`docs/user-guide/04-corpora.md`](../user-guide/04-corpora.md) states the
intent. The artefact those documents refer to exists and has been checked
against the database in this repository:

| | |
|---|---|
| files | `careg-00000{0,1,2}.parquet` (one of four subsets in a 1M-row map) |
| columns | `row_id int64`, `uuid string`, `subset string`, `embedding fixed_size_list<float>[1024]` |
| parquet metadata | `embedding_model = Qwen3-Embedding-0.6B`, `embedding_dim = 1024`, `embedding_dtype = float32` |
| rows | 57,523 |
| uuid coverage against `legal.db` `regulations` | **57,523 / 57,523 — 100%** |

Two details make this a good first payload rather than merely an available
one. The **row count is exact** against the user table, and the **model
identity travels with the vectors** in the parquet's own key-value metadata —
so the `model_registry` row can be populated from the artefact instead of
from a human's recollection of which checkpoint produced it. That is the same
identity discipline the Embedder service's `ModelInfo` enforces at runtime,
arriving through a different door.

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
time operation when the encoder was trained for it [Kusupati et al. 2022],
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

The single highest-leverage use of a language model in an entity-linking
pipeline is not selection — it is giving the retriever something to retrieve
with. LLM-augmented entity linking [Xin et al. 2025] reports substantial
absolute gains from exactly this: the LM writes context for the mention, the
retriever does the retrieval, and neither does the other's job.

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
resolve-then-generate pattern [Talaei et al. 2024] made concrete at the
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

- [Ayoola et al. 2022] Tom Ayoola et al. "ReFinED: An Efficient Zero-shot-capable
  Approach to End-to-End Entity Linking." NAACL 2022 (Industry Track).
- [De Cao et al. 2021] Nicola De Cao et al. "Autoregressive Entity Retrieval."
  ICLR 2021.
- [Ethayarajh 2019] Kawin Ethayarajh. "How Contextual are Contextualized Word
  Representations?" EMNLP-IJCNLP 2019.
- [Gao et al. 2019] Jun Gao et al. "Representation Degeneration Problem in
  Training Natural Language Generation Models." ICLR 2019.
- [Gao et al. 2021] Tianyu Gao, Xingcheng Yao, Danqi Chen. "SimCSE: Simple
  Contrastive Learning of Sentence Embeddings." EMNLP 2021.
- [Kusupati et al. 2022] Aditya Kusupati et al. "Matryoshka Representation
  Learning." NeurIPS 2022.
- [Li et al. 2020] Belinda Z. Li et al. "Efficient One-Pass End-to-End Entity
  Linking for Questions." EMNLP 2020 (ELQ).
- [Li et al. 2020b] Bohan Li et al. "On the Sentence Embeddings from
  Pre-trained Language Models." EMNLP 2020.
- [Orlando et al. 2024] Riccardo Orlando et al. "ReLiK: Retrieve and LinK."
  ACL 2024 (Findings).
- [Radovanović et al. 2010] Miloš Radovanović, Alexandros Nanopoulos,
  Mirjana Ivanović. "Hubs in Space: Popular Nearest Neighbors in
  High-Dimensional Data." JMLR 11, 2010.
- [Su et al. 2021] Jianlin Su, Jiarun Cao, Weijie Liu, Yangyiwen Ou.
  "Whitening Sentence Representations for Better Semantics and Faster
  Retrieval." arXiv:2103.15316.
- [Talaei et al. 2024] Shayan Talaei et al. "CHESS: Contextual Harnessing for
  Efficient SQL Synthesis." arXiv:2405.16755.
- [Wang & Isola 2020] Tongzhou Wang, Phillip Isola. "Understanding
  Contrastive Representation Learning through Alignment and Uniformity on the
  Hypersphere." ICML 2020.
- [Wu et al. 2020] Ledell Wu et al. "Scalable Zero-shot Entity Linking with
  Dense Entity Retrieval." EMNLP 2020 (BLINK).
- [Wu et al. 2025] Shiguang Wu, Zhaochun Ren, Xin Xin, Jiyuan Yang, Mengqi
  Zhang, Zhumin Chen, Maarten de Rijke, Pengjie Ren. "Constrained
  Auto-Regressive Decoding Constrains Generative Retrieval." SIGIR 2025.
- [Xin et al. 2025] Amy Xin et al. "LLMAEL: Large Language Models are Good
  Context Augmenters for Entity Linking." CIKM 2025. arXiv:2407.04020.
- [Zhang et al. 2025] Yanzhao Zhang et al. "Qwen3 Embedding: Advancing Text
  Embedding and Reranking Through Foundation Models." arXiv:2506.05176.

Full bibliography, with venues and identifiers verified: [00-bibliography.md](00-bibliography.md).
