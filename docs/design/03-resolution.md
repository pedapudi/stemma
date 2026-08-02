# Resolution

The resolution pipeline is the product. This document specifies it stage by
stage as it exists today: the real constants, the scoring as mathematics, the
reasoning behind the document-versus-value split, the trace contract,
complexity, and the limitations that are actually there.

Everything below describes
[`crates/stemma-resolve/src/lib.rs`](../../crates/stemma-resolve/src/lib.rs)
— the lexical cascade over the index built by
[`stemma-ingest`](../../crates/stemma-ingest/src/lib.rs), the knowledge-graph
assists from [`stemma-kg`](../../crates/stemma-kg/src/lib.rs), and the dense
channel through [`stemma-embed`](../../crates/stemma-embed/src/lib.rs).
Collective disambiguation and the LM band are designed in
[04-knowledge-graph.md](04-knowledge-graph.md#designed-instance-layer-and-collective-disambiguation)
and [05-encoders-decoders.md](05-encoders-decoders.md); they are not built,
and nothing in this document depends on them.

The entry points are:

```rust
pub fn resolve_lexical(db: &StemmaDb, query: &str) -> Result<Trace>;
pub fn resolve(db: &StemmaDb, query: &str,
               embedder: Option<&dyn stemma_embed::Embedder>) -> Result<Trace>;
```

`resolve_lexical` is `resolve(db, query, None)`. The embedder is **optional
and fallible by type**: with no embedder configured, or no `vec_dense` table
in the store, or an embedding call that fails, resolution proceeds on the
lexical and knowledge channels and logs a warning. Degradation is the
designed behaviour, not an error path.

## Constants

Every tunable in the pipeline, with its source and its job:

| Constant | Value | Defined in | Role |
|---|---:|---|---|
| `MIN_SPAN_CHARS` | 3 | stemma-resolve | Spans shorter than this are never looked up |
| `MAX_SPAN_TOKENS` | 4 | stemma-resolve | Longest mention considered, in tokens |
| `PER_CHANNEL_LIMIT` | 8 | stemma-resolve | Candidates fetched per channel per span |
| `SELECT_THRESHOLD` | 0.35 | stemma-resolve | Fused score below which a candidate is traced but not selected |
| `TOP_K` | 5 | stemma-resolve | Max selected candidates per mention |
| `DENSE_MAX_SPANS` | 4 | stemma-resolve | Spans per query that get a dense KNN probe |
| `RRF_K` | 4.0 | stemma-resolve (`fuse`) | Reciprocal-rank-fusion damping constant |
| `EXACT_MAX_LEN` | 120 | stemma-ingest | Values longer than this are excluded from the exact channel |
| `DOC_MIN_LEN` | 200 | stemma-ingest | Values at least this long are classified `is_doc` |
| KG bonus | 0.04 / matched co-term | stemma-resolve | Coherence increment, capped at 0.9 |
| KG span nudge | ×1.08 | stemma-resolve | Selection preference for spans matching a KG entity |
| `STOPWORDS` | 29 words | stemma-resolve | Never a mention alone; allowed inside longer spans |

`RRF_K = 4.0` is far below the k = 60 of the original formulation [Cormack
et al. 2009]. That is deliberate and is discussed under
[fusion](#stage-5--reciprocal-rank-fusion): with only three channels and
eight results each, k = 60 flattens every rank difference into noise.

## Stage 0 — precondition

```rust
SELECT count(*) FROM sqlite_master WHERE name = 'lex_values'
```

No index, no resolution: `Error::IndexMissing`, surfaced by the server as
gRPC `FAILED_PRECONDITION` with the message *"lexical index missing — run
ingest first"*. Resolution never silently degrades to zero mentions because
of missing derived state; that failure mode is indistinguishable from a
genuine no-match and would be invisible in evaluation.

## Stage 1 — tokenization

```rust
fn tokenize(query: &str) -> Vec<Token>
```

Maximal runs of `char::is_alphanumeric()` become tokens; everything else is a
separator. Each token records its **byte** offsets into the query
(end-exclusive) and a `stopword` flag from a 29-word list:

> a, an, and, are, at, by, did, do, does, for, from, how, in, is, it, of,
> on, or, s, that, the, to, was, were, what, when, where, which, who, with

The list is intentionally tiny — question words, articles, prepositions,
copulas, and the possessive `s` that falls out of the tokenizer. It exists to
stop *"the"* becoming a mention, not to do linguistics. Stopwords are only
excluded from being a mention **on their own**; they participate freely
inside longer spans, because *"Bank of America"* is a mention and *"of"* is
not.

Because `is_alphanumeric` is Unicode-aware, non-Latin scripts tokenize
without special-casing. Because possessives split (`Chen's` → `Chen`, `s`),
the span `Chen's` is still enumerated — spans are reconstructed from the
original string between token boundaries, so interior punctuation is
preserved verbatim.

## Stage 2 — span enumeration

```rust
for i in 0..tokens.len() {
    for n in 1..=MAX_SPAN_TOKENS.min(tokens.len() - i) {
        // span text = query[tokens[i].start .. tokens[i+n-1].end]
    }
}
```

Every contiguous n-gram of up to 4 tokens, in `(start, length)` order. For a
query of *T* tokens this is

$$ S(T) = \sum_{i=0}^{T-1} \min(4,\; T-i) = 4T - 6 \quad (T \ge 4) $$

A five-token query yields 14 spans, an eight-token query 26. Both match the
observed traces exactly.

A span is marked `skipped` immediately if **all** its tokens are stopwords,
or if its text is shorter than `MIN_SPAN_CHARS = 3` characters. Skipped spans
are *kept in the trace* rather than discarded, so the console can grey them
out and a reader can see that the pipeline considered and dismissed them. All
other spans start with the provisional status `selected`, refined after
candidates are gathered.

There is no early segmentation decision here. Overlapping alternatives —
`Wei`, `Chen`, `Wei Chen`, `did Wei Chen` — all coexist and all get
candidates. The segmentation is decided at the end, by evidence, in
[stage 7](#stage-7--greedy-non-overlapping-selection). This is the
soft-span discipline: committing to boundaries before you know what the
boundaries match is how a linker loses recall it can never recover.

## Stage 3 — knowledge-graph-assisted mention detection

If the store has a compiled graph, every non-skipped span is tested against
the graph's entity vocabulary:

```sql
SELECT count(*) FROM kg_nodes WHERE kind = 'term' AND lower(label) = ?1
```

A hit sets `span.kg_alias = true`. `kind = 'term'` covers both single-word
TextRank terms and mined multi-word capitalized phrases (see
[04-knowledge-graph.md](04-knowledge-graph.md)), so this is where *"coastal
development permit"* gets recognized as one thing rather than three.

The flag does not change candidate generation at all. It changes **selection
priority** — a span the corpus itself has told us is an entity gets a ×1.08
nudge when spans compete for their byte range. The rationale: a compiled
phrase is independent evidence of mention-hood, structurally different from
and complementary to raw match strength. A four-token span that happens to
retrieve well is not the same kind of thing as a four-token span the corpus
uses as a unit.

This is the knowledge graph participating in *mention detection*, not just
in ranking — the classic entity-linking move of using an alias table to
propose spans [Hoffart et al. 2011], with the alias table compiled from the
user's own corpus instead of a fixed catalog.

**Honest scope note.** On the legal corpus the compiled term vocabulary is 88
nodes (48 single terms, 40 phrases), so `kg_alias` fires on common terms
(*facility*, *contract*, *payment*) and on the mined phrases (*California
Code of Regulations Title*, *Revenue and Taxation Code*) but not on
*coastal permit*. The mechanism is real; the vocabulary is currently too
small for it to fire often. Widening it is discussed in
[04-knowledge-graph.md](04-knowledge-graph.md#designed-instance-layer-and-collective-disambiguation).

## Stage 4 — candidate generation, three channels

Each non-skipped span runs three independent lexical retrieval channels, each
capped at `PER_CHANNEL_LIMIT = 8` results. **These three are never conditional
on each other**: the failure modes are disjoint, and a cascade that skips one
because another looked confident loses exactly the cases the skipped channel
was for [Cormack et al. 2009]. (The fourth channel, dense, *is* conditional —
see [stage 4b](#stage-4b--the-dense-channel-targeted) — because its cost
profile is different by two orders of magnitude.)

It is worth being explicit that the lexical channels are not a legacy
fallback waiting to be replaced by the dense one. BM25 over an inverted index
remains a strong baseline for exactly this problem: Sparkly, a TF/IDF blocker
built on Lucene, outperformed eight state-of-the-art entity-matching blockers
[Paulsen et al. 2023]. Names, codes and identifiers are lexical objects, and
a channel that matches them character-for-character is the right tool.

### Channel 1 — exact

```sql
SELECT src_table, src_column, src_rowid, value FROM lex_values
WHERE value_norm = lower(trim(?1)) AND length(value) <= 120
LIMIT 8
```

Served by the `lex_values_norm` B-tree index. Case-insensitive and
edge-whitespace-insensitive, since `value_norm` is `lower(trim(value))` and
the probe applies the same normalization to the span. `raw` score is 1.0 for
every hit; rank is result order.

The `length(value) <= EXACT_MAX_LEN` guard is what keeps the channel
meaningful: a mention does not "equal" an 800 KB regulation, and without the
guard a long value that happened to normalize to the span text would take the
0.9-floor scoring branch.

### Channel 2 — BM25 token search

```sql
SELECT v.src_table, v.src_column, v.src_rowid, v.value, bm25(lex_fts),
       v.is_doc, snippet(lex_fts, 0, '⟨', '⟩', '…', 10)
FROM lex_fts f JOIN lex_values v ON v.id = f.rowid
WHERE lex_fts MATCH ?1 ORDER BY bm25(lex_fts) LIMIT 8
```

The span is wrapped as an FTS5 phrase — `format!("\"{}\"", span.replace('"',
"\"\""))` — so query punctuation is treated as text, not as FTS5 operators,
and so the tokens must appear adjacently and in order. SQLite's `bm25()` is
lower-is-better; the pipeline stores `raw = -bm25` so that larger is better
uniformly across channels.

### Channel 3 — trigram fuzzy/substring search

Identical SQL against `lex_trigram`. The trigram tokenizer indexes
overlapping 3-character sequences, so this channel matches inside words and
across token boundaries — `Northgate` finds `Seattle - Northgate`, which
`unicode61` tokenization plus phrase matching would also find, but
`Northgat` or `North gate` would not.

The channel needs at least three characters, which is why `MIN_SPAN_CHARS`
is 3. Queries that the trigram tokenizer legitimately cannot express produce
a SQLite error, which is caught and treated as zero hits for that channel
rather than failing the resolution.

### Snippets

`snippet(fts, 0, '⟨', '⟩', '…', 10)` produces a 10-token window around the
best match with hit terms bracketed. It is **retained only when `is_doc`**:

```
…be advised that a ⟨coastal permit⟩ is required if the…
```

For a short value the value *is* the evidence; for a 2,660-character
regulation the value is not, and shipping it would be both useless and
enormous. The snippet is what `LexicalMatch.matched_text` carries for
documents.

## Stage 4b — the dense channel, targeted

The pipeline runs in three phases rather than one loop over spans, and the
reason is entirely about the dense channel's cost profile.

**Phase 1** gathers lexical hits for every live span (`gather_lexical_hits`),
keeping them as raw hits keyed by span id rather than fusing immediately.

**Phase 2** runs the dense channel — but only if an embedder is configured
*and* the store has a `vec_dense` table, and only on a chosen subset of
spans:

```rust
let mut targets: Vec<&Span> = spans.iter()
    .filter(|s| s.status != "skipped")
    .filter(|s| /* no exact hit in phase 1 */)
    .collect();
targets.sort_by_key(|s| std::cmp::Reverse(s.end - s.start));
targets.truncate(DENSE_MAX_SPANS);          // 4
```

Three selection rules, each with a reason:

- **Spans with an exact hit are excluded.** An exact normalized match already
  scores in [0.9, 1.0] and no dense neighbour can improve on it. Spending a
  KNN probe there buys nothing.
- **Longest spans first.** A longer span carries more semantic content, which
  is precisely what a dense encoder is for; a one-token span is better served
  by BM25.
- **At most `DENSE_MAX_SPANS = 4` per query**, because `vec0` KNN in
  sqlite-vec v0.1.6 is a **full scan of the vector table per probe**. There
  is no ANN index. On the 57,523-vector careg table each probe is a linear
  pass over 57,523 × 1024 floats — hundreds of milliseconds — so the cap is
  what keeps the dense channel affordable at all.

### The full-query span

Span enumeration caps n-grams at `MAX_SPAN_TOKENS = 4`, which is the right
width for lexical anchors and the wrong one for a mention that is semantic
all the way through: *"getting fired from a state job"* has no anchor at any
n-gram width, but the full phrase lands near the right documents in vector
space. So when an embedder is configured and the query is longer than the
n-gram cap, the whole query gets one extra span of its own. It flows through
the same phases as every other span, and **greedy mention selection
arbitrates**: a query with a strong lexical anchor is won by that anchor's
span (exact scores in [0.9, 1.0], comfortably above any calibrated cosine)
and the full-query span is marked `overlapped`; an anchor-free query is won
by the full-query span, which — because it covers the entire token range —
also suppresses the incidental substring matches that would otherwise
shatter the query into junk mentions ("fired from a" resolving into a mortar
regulation). No new selection rule was added; covering more bytes at a
higher score is already how the greedy cover decides.

The selected spans are embedded in **one batched call**, then probed:

```sql
SELECT src_table, src_column, src_rowid, distance FROM vec_dense
WHERE embedding MATCH ?1 AND k = ?2          -- k = PER_CHANNEL_LIMIT = 8
```

A failed embedding call logs `dense channel degraded` and leaves phase 1's
hits untouched.

### Query formatting is asymmetric

```rust
pub fn format_query(mention: &str) -> String {
    format!("Instruct: Given a search query, retrieve relevant passages \
             that answer the query\nQuery: {mention}")
}
```

Documents were embedded raw; the *mention* carries the retrieval instruction.
This is the Qwen3-Embedding-family convention [Zhang et al. 2025], and it is
in the code rather than in a config file because getting it wrong silently
costs most of the encoder's quality — everything still runs, the numbers are
just worse. See
[05-encoders-decoders.md](05-encoders-decoders.md#query-time-requirements).

### Distance to similarity

`vec_dense` is declared with the default L2 metric, and the vectors are unit
length, so the conversion is exact rather than approximate:

$$ \cos(a,b) = 1 - \frac{d_2(a,b)^2}{2} $$

which is what lands in `ChannelScore.raw` for the `dense` channel. Rank
within the KNN result is what feeds RRF; the cosine is carried for evidence —
and, unlike the lexical raws, it is also *used* (next section but one).

Because `vec_dense` stores only `(src_table, src_column, src_rowid)`, each
dense hit is joined back to `lex_values` for its `value` and `is_doc` flag. A
row present in the vector table but absent from the lexical index defaults to
`is_doc = true` with an empty value — the conservative choice, since it takes
the document scoring branch rather than being length-penalized against an
empty value.

**Phase 3** fuses the union of lexical and dense hits per span, applies the
knowledge-graph coherence bonus, and refines span status.

## Stage 5 — reciprocal rank fusion

Hits from all channels are grouped by `(src_table, src_column, src_rowid)` —
one candidate per *matched cell*, carrying one `ChannelScore { channel, rank,
raw }` per channel that found it.

### The fused base

With `K = RRF_K = 4` and `C` the set of channels that returned this candidate:

$$
\mathrm{rrf} = \sum_{c \in C} \frac{1}{K + \mathrm{rank}_c}
\qquad
\mathrm{base} = \min\!\left(\frac{\mathrm{rrf}}{3/K},\; 1\right)
= \min\!\left(\tfrac{4}{3}\,\mathrm{rrf},\; 1\right)
$$

Reciprocal rank fusion [Cormack et al. 2009] is used for the reason it is
always used: the channels produce scores on incomparable scales — a constant
1.0, a negated BM25 in the single digits, a negated BM25 over trigrams, a
cosine in [−1, 1] — and rank is the only quantity they share. RRF needs no
score normalization, no per-channel calibration, and no training data, and it
reliably beats the individual runs it fuses.

The denominator `3/K` normalizes so that **three channels at rank 0 gives
exactly `base = 1.0`**, with the `min(…, 1)` clamp absorbing anything beyond.
The constant is literally 3 in the code, and it did **not** change when the
dense channel was added — which has a real consequence.

Before the dense channel, a document was excluded from the exact channel and
so could reach at most two channels at rank 0:

$$ \mathrm{base}_{\max}^{\text{doc}} = \tfrac{4}{3}\left(\tfrac14 + \tfrac14\right) = \tfrac23
\quad\Longrightarrow\quad \mathrm{score}_{\max}^{\text{doc}} = 0.85 \times \tfrac23 = 0.567 $$

With `dense` available, a document can reach BM25, trigram *and* dense at
rank 0:

$$ \mathrm{base}_{\max}^{\text{doc}} = \tfrac{4}{3}\left(\tfrac14 + \tfrac14 + \tfrac14\right) = 1.0
\quad\Longrightarrow\quad \mathrm{score}_{\max}^{\text{doc}} = 0.85 $$

The document ceiling therefore rose from 0.567 to 0.85 the moment the fourth
channel landed, without any constant changing. The band ordering still holds —
0.85 is below the 0.9 exact floor — but the headroom is now one bonus step
wide, and the code comment above `base` (*"three channels at rank 0 → 1.0;
docs never have the exact channel, so their base tops out at 2/3"*) no longer
describes the system. Non-document values are affected the same way: a value
matched at rank 0 by all three non-exact channels can now reach
`1.0 × (0.4 + 0.6·affinity)`, up to 1.0, where before the ceiling without an
exact hit was 0.667. **A fuzzy match can now tie an exact one.** Re-deriving
the normalizer and the 0.85/0.9 constants against four channels is
outstanding work, and it is exactly the kind of change that is silent in
every test that does not assert on absolute scores.

**Why K = 4 and not 60.** The standard constant k = 60 was tuned for fusing
long TREC runs, where it damps the difference between rank 1 and rank 2 to
almost nothing so that agreement across systems dominates. Here each channel
returns at most 8 results; with k = 60, ranks 0 and 7 differ by
1/60 − 1/67 ≈ 0.0017, and every candidate scores essentially the same. K = 4
keeps rank informative across the actual range: rank 0 contributes 0.250,
rank 7 contributes 0.091, a 2.75× spread.

### Score assignment: three branches

```rust
let score = if has_exact {
    (0.9 + 0.1 * base).min(1.0)
} else if g.is_doc {
    (base * 0.85).min(0.85)
} else {
    let affinity = (span_len / value_len.max(span_len)).sqrt();
    (base * (0.4 + 0.6 * affinity)).min(1.0)
};
```

**Exact branch — `[0.9, 1.0]`.** An exact normalized match is definitionally
right about the value; the only open question is which of several exact
matches is meant. So the branch is a floor of 0.9 with the fused base
deciding the last tenth. This makes exact matches unconditionally outrank
everything else, which is the correct prior: if the user typed the stored
value, they meant the stored value.

**Document branch — `[0, 0.85]`.** A mention resolves *into* a document, so
length is not evidence against it. The base is scaled by 0.85 so that a
perfect document match still ranks below any exact match — a document
containing your phrase is weaker evidence than a cell equal to it.

**Value branch — length affinity.** For short stored values that are neither
exact matches nor documents:

$$
\mathrm{affinity} = \sqrt{\frac{L_{\text{span}}}{\max(L_{\text{value}},\, L_{\text{span}})}}
\qquad
\mathrm{score} = \mathrm{base}\cdot\left(0.4 + 0.6\,\mathrm{affinity}\right)
$$

Affinity is 1 when the span and the value are the same length and decays as
the value grows beyond the span. The square root softens the decay — a value
four times the span's length still scores at half affinity, not a quarter.
The `0.4 + 0.6·affinity` envelope keeps a floor: a very long value that
merely contains the span retains 40% of its fused base rather than
collapsing to zero.

The purpose is discrimination among near-miss values. Given the span
*"Seattle"*, both `offices.city = 'Seattle'` and `offices.name = 'Seattle -
Northgate'` match; the first is what the user said, the second contains it.
Length affinity is what separates them when neither is the exact channel's
job.

### The calibrated cosine floor

RRF deliberately discards raw scores, and for the lexical channels that is
pure gain — their raws are incomparable. The dense channel is different in
kind: its raw is a **cosine against a fixed encoder, which is absolute
evidence**, comparable across queries and across corpora in a way a BM25
value never is. A candidate found *only* by the dense channel pays RRF's
single-channel price (`base = ⅓` at rank 0, a doc score of ≈ 0.28 — below
the 0.35 selection threshold), which turns the semantic channel into one
that can never surface a mention on its own testimony. So after the branch
score, the best dense cosine on the candidate is calibrated onto the score
scale and applied as a floor:

```rust
let calibrated = (((best_cos - 0.30) / 0.30).clamp(0.0, 1.0)) * 0.78;
score = score.max(calibrated);
```

The window `[0.30, 0.60]` is the observed working range of
Qwen3-Embedding-0.6B under the asymmetric instruct format on this corpus —
cosines below ≈ 0.4 are topical noise, above ≈ 0.55 are strong semantic
matches — and the 0.78 ceiling keeps a perfect cosine below the 0.85
document ceiling and the 0.9 exact floor, preserving the band ordering:
*exact > lexically-corroborated document > dense-only semantic match*. The
constants are per-encoder by nature; when the embedder becomes swappable in
anger, the calibration window belongs beside the model identity in
`model_registry`, not in the code.

### Worked examples, from real traces

Mini corpus, query *"the Q3 numbers for the Seattle office"*, span
`Seattle`:

| candidate | channels (rank) | rrf | base | branch | score |
|---|---|---:|---:|---|---:|
| `offices.city #17` `'Seattle'` | exact 0, bm25 0, trigram 0 | 0.750 | 1.000 | exact | **1.000** |
| `offices.name #17` `'Seattle - Northgate'` | bm25 1, trigram 1 | 0.400 | 0.533 | value, affinity √(7/19)=0.607 | **0.408** |

Legal corpus, query *"appeals of coastal permit denials"*, span
`coastal permit`:

| candidate | channels (rank) | rrf | base | branch | score |
|---|---|---:|---:|---|---:|
| `regulations.text #28209` | bm25 0, trigram 0 | 0.500 | 0.667 | doc | **0.567** |
| `regulations.text #29055` | bm25 1, trigram 1 | 0.400 | 0.533 | doc | **0.453** |
| `regulations.text #29052` | bm25 2, trigram 2 | 0.333 | 0.444 | doc | **0.378** |

### Why documents need their own branch: the careg failure mode

This is the most consequential scoring decision in the pipeline, and it is
worth showing the arithmetic that forced it.

Take the same candidate — `regulations.text #28209`, a 2,660-character
regulation, matched at rank 0 in both FTS channels by the 14-character span
*"coastal permit"*. Its fused base is 0.667. **If the value branch's length
affinity were applied to it:**

$$
\mathrm{affinity} = \sqrt{\tfrac{14}{2660}} = 0.0726
\qquad
\mathrm{score} = 0.667 \times (0.4 + 0.6 \times 0.0726) = \mathbf{0.296}
$$

0.296 is below `SELECT_THRESHOLD = 0.35`. The span's best candidate is under
threshold, so the span's status becomes `weak`, and `weak` spans are never
selected as mentions. The same arithmetic holds for *every* span against
*every* document in the corpus, because document lengths dwarf mention
lengths by construction. **The entire corpus would return zero mentions for
every query** — not a degraded result, a total one, and one that looks
identical to "nothing in this database matches" rather than to a bug.

With the document branch: 0.667 × 0.85 = 0.567, comfortably above threshold.

The general principle: *length is evidence about the relationship between a
mention and a value only when that relationship is equality.* When the
relationship is containment — which is the only relationship a mention can
have with a document — length is evidence about the document's genre, and
penalizing it means preferring short documents over relevant ones. Document
retrieval solved this decades ago inside BM25 itself, whose length
normalization compares a document against the corpus mean rather than
against the query [Robertson & Zaragoza 2009]; the mistake here would be to
apply a *second*, harsher, query-relative penalty on top of the one BM25
already applied correctly.

The regression test is
`stemma_resolve::tests::document_corpus_resolution_works`, which builds a
three-document corpus in the shape of the failure and asserts that mentions
come back, the winner is a document, its snippet carries the `⟨⟩` markers,
and the topically correct document outranks the topically wrong one.

### Reachable score bands

The three branches partition the score range into bands that are, by
construction, ordered by evidence strength:

| Evidence | Reachable band |
|---|---|
| Exact normalized value match | 0.900 – 1.000 |
| Document, with KG coherence | up to 0.900 (hard cap) |
| Document, three channels (bm25 + trigram + dense) | up to 0.850 |
| Document, lexical only | 0.000 – 0.567 |
| Short value, fuzzy/token/dense match | 0.000 – 1.000, modulated by affinity |

A document can never *outrank* an exact match, because the coherence bonus is
capped at 0.9 and the exact floor is 0.9 — but it can now tie one, which is a
narrower guarantee than the design intends.

## Stage 6 — knowledge-graph coherence

Runs only when at least one candidate is a document and the store has a
compiled graph. This is the "GraphRAG-lite" assist: structure mined from the
corpus, used to break ties that lexical matching alone cannot.

1. Split the span into tokens of ≥ 3 characters, lowercased.
2. Find terms that co-occur with them in the compiled graph:

   ```sql
   SELECT DISTINCT n2.label FROM kg_nodes n1
   JOIN kg_edges e ON e.kind = 'cooccurs' AND (e.src = n1.id OR e.dst = n1.id)
   JOIN kg_nodes n2 ON n2.id = CASE WHEN e.src = n1.id THEN e.dst ELSE e.src END
   WHERE n1.kind = 'term' AND n1.label IN (…)
   LIMIT 4
   ```

3. Drop co-terms that are already in the span.
4. For each document candidate, count how many co-terms `c` also appear in
   that document (one FTS5 phrase probe against the candidate's `lex_fts`
   row each).
5. If `m > 0`: `score ← min(score + 0.04·m, 0.9)`, and push a
   `ChannelScore { channel: "kg", rank: 0, raw: m }`.
6. Re-sort by score.

The increment is small on purpose — 0.04 per co-term, at most four co-terms,
so at most +0.16 — because this is a coherence *tiebreaker*, not a retrieval
signal. It should reorder candidates that lexical matching already found
roughly equal; it should never promote a candidate that lexical matching
ranked poorly.

**A real reordering.** Legal corpus, query *"facility contract payment"*,
span `facility` (which is itself a `kg_alias`):

| candidate | channels | base | doc score | KG | final |
|---|---|---:|---:|---:|---:|
| `regulations #57316` | bm25 1, trigram 0 | 0.600 | 0.510 | +0.08 (m=2) | **0.590** |
| `regulations #42595` | bm25 0, trigram 2 | 0.556 | 0.472 | — | **0.472** |
| `regulations #52542` | bm25 2, trigram 1 | 0.533 | 0.453 | +0.04 (m=1) | **0.493** |

Both #57316 and #52542 overtake #42595, which had the better BM25 rank, on
the strength of containing terms the corpus's own co-occurrence graph
associates with *facility*. The `kg` channel appears in the trace and in the
gRPC evidence, so the reordering is inspectable rather than mysterious.

## Stage 7 — greedy non-overlapping selection

Spans still marked `selected` (that is: not skipped, with at least one
candidate, whose best candidate is at or above threshold) compete for
non-overlapping byte ranges.

**Ordering key.** Spans are sorted by

$$
\mathrm{key}(s) = \mathrm{score}(\text{best candidate of } s) \times
\begin{cases} 1.08 & \text{if } s.\mathrm{kg\_alias} \\ 1 & \text{otherwise}\end{cases}
$$

descending, with **longer spans first on ties** — a longer span is more
specific, and *"Wei Chen"* should beat *"Chen"* when both match equally well.

**Greedy assignment.** Walk that order; a span whose byte range `[start, end)`
intersects any already-taken range is marked `overlapped` and every one of
its candidates gets `reject_reason = "span_not_selected"`. Otherwise the span
becomes a mention, and within it:

- candidates at index `< TOP_K` with `score ≥ SELECT_THRESHOLD` → `selected`
- everything else → `reject_reason` of `"below_threshold"` or `"outranked"`

Finally, mentions are re-sorted into query order by byte offset, so the
response reads left to right regardless of the order in which spans won.

**Losing spans keep their candidates.** This is not incidental. In the test
`overlapped_spans_keep_near_misses`, the query *"what did Wei Chen ship"*
selects `Wei Chen` — and the losing sub-span `Chen` retains its candidates,
including the *other* Chen (Dana), marked `span_not_selected`. A
disambiguation UI, or a downstream consumer that disagrees with the
segmentation, needs that rival visible. Discarding it would make the
resolution unappealable.

## The trace contract

The public contract of the pipeline is not "the answer" — it is the answer
*plus everything that lost and why*.

```rust
pub struct Trace {
    pub query: String,
    pub tokens: Vec<Token>,   // every token, with stopword flags
    pub spans: Vec<Span>,     // every span enumerated, skipped ones included
    pub mentions: Vec<usize>, // indices into spans, in query order
    pub elapsed_ms: f64,
}
```

Five span statuses, each meaning a different kind of "no":

| Status | Meaning |
|---|---|
| `selected` | Became a mention |
| `overlapped` | Lost its byte range to a stronger span |
| `no_candidates` | Nothing in any channel matched |
| `weak` | Matched, but the best candidate is below threshold |
| `skipped` | Stopword-only, or shorter than `MIN_SPAN_CHARS` |

Three reject reasons, each meaning a different kind of loss:
`below_threshold`, `outranked`, `span_not_selected`.

`Resolve` returns the projection (`trace_to_proto`); `Explain` returns the
whole thing (`trace_to_explain_proto`). Both are served from the *same*
trace by the same code path in `stemma-server`, so `Explain` is never a
re-derivation that might disagree with `Resolve`. Since resolution is
deterministic and read-only, the console can also re-run `Explain` for a
query an agent already resolved and get a byte-identical trajectory — which
is exactly what [`ui/agent_backend.py`](../../ui/agent_backend.py) does to
visualize an agent's tool calls.

An example trace, `"appeals of coastal permit denials"` on the legal corpus
(via `cargo run -p stemma-resolve --example trace_dump`):

```
elapsed 750.8 ms · 5 tokens · 14 spans
  selected: 3   overlapped: 5   no_candidates: 5   skipped: 1
mentions: "appeals" (9 candidates), "coastal permit" (3), "denials" (8)
```

## Complexity

Let *T* be the query's token count, *S = 4T − 6* the span count, and *S'* the
non-skipped subset.

**SQL round trips per resolution:**

$$
2 + S' \cdot (\underbrace{1}_{\text{kg alias}} + \underbrace{1}_{\text{exact}} + \underbrace{2}_{\text{FTS}}) + \sum_{s \in S'_{\text{doc}}} \left(2 + 4\,|D_s|\right)
$$

where `|D_s| ≤ 32` is the number of document candidates for span *s*. With
the dense channel enabled, add **one HTTP round trip** to the embedding
endpoint (batched, all target spans in one request) and at most
`DENSE_MAX_SPANS = 4` KNN probes. The pipeline is therefore **linear in query
length and independent of corpus size in round-trip count** — corpus size
enters through the cost of each FTS5 `MATCH` and, much more sharply, through
each dense probe.

**What dominates.** Without the dense channel: FTS5 phrase matching over the
trigram index, two per span. With it: the dense probes, by a wide margin.
`vec0` KNN in sqlite-vec v0.1.6 has no approximate index — every probe reads
the entire vector table, so a single probe over 57,523 vectors at 1024
dimensions touches ~236 MB of float data. That single fact is why the channel
is capped at four spans and skipped wherever an exact hit already exists, and
it is the strongest argument for adding a partition key or a coarse quantized
pre-filter to `vec_dense`.

The knowledge-coherence probes are cheap individually (a point lookup joined
to a single-row FTS match) but there can be up to 4 × 32 = 128 of them per
span.

**Measured, on the legal corpus** (92,696 documents, 370,784 indexed cells,
4.2 GB store, debug build, warm page cache):

| Query | Tokens | Spans | Latency |
|---|---:|---:|---:|
| *appeals of coastal permit denials* | 5 | 14 | 751 ms |
| *bank insurance commission* | 3 | 6 | 728–768 ms |
| *facility contract payment* | 3 | 6 | 1,019 ms |
| *which sections govern appeals of coastal permit denials?* | 8 | 26 | 2,468–2,512 ms |

Roughly 60–95 ms per non-skipped span, scaling linearly with span count as
predicted. On the mini corpus (25 indexed cells, no documents) the same
pipeline takes 2.9 ms for a seven-token query.

Sub-second on a hundred-thousand-document corpus is usable but not fast, and
the shape of the cost points at the fix: spans are independent, so the
per-span channel queries parallelize perfectly. That requires a connection
pool, which requires replacing the server's per-database `Mutex<StemmaDb>`.
Both are unbuilt.

The measurements above were taken **without** the dense channel; the
latencies for a dense-enabled server are not yet recorded here.

**Memory** is bounded by the trace: `O(S · 4 · PER_CHANNEL_LIMIT)` candidate
records, at most 32 per span before grouping, so a few hundred small structs
for any realistic query. Values are truncated to 160 characters for
transport, so an 800 KB regulation never crosses the wire.

## Known limitations

These are real and current. None of them is hidden by the trace — every one
is visible as a span status or a missing candidate, which is the point of
tracing everything.

**Span length caps at four tokens.** `MAX_SPAN_TOKENS = 4` means *"California
Department of Fish and Wildlife"* (6 tokens) can never be a single span. It
will be resolved as fragments, and the fragments will compete for byte ranges
in a way that discards some of them. Raising the cap raises span count
linearly and therefore latency linearly; the real fix is to let the
knowledge graph's phrase vocabulary *propose* long spans directly rather than
enumerating all n-grams up to a larger bound.

**Two-character mentions are invisible.** `MIN_SPAN_CHARS = 3` is inherited
from the trigram index's minimum, but it silently kills real abbreviations:
*Q3*, *CA*, *US*, *H2*. In the mini-corpus trace of *"the Q3 numbers for the
Seattle office"*, the span `Q3` is marked `skipped` — and `Q3` is a stored
value in `reports.quarter`. The exact channel could serve short spans without
the trigram index; it is not currently consulted for them.

**Greedy selection is local.** Spans are assigned byte ranges one at a time
by descending score. A single high-scoring wrong long span can block two
correct short ones, and nothing reconsiders. The principled fix is joint
selection over segmentations, which is the same machinery collective
disambiguation needs; at query scale (2–4 mentions × ~10 candidates)
exhaustive joint scoring is microseconds [Hoffart et al. 2011].

**Mentions are scored independently.** There is no interaction between
mentions at all today. *"Chen's team"* resolves `Chen` and `team` as
unrelated spans; nothing checks that the selected Chen has an edge to any
team. This is the largest gap between what is built and what is designed, and
it is what [04-knowledge-graph.md](04-knowledge-graph.md#designed-instance-layer-and-collective-disambiguation)
is about.

**Boundary stopwords are not trimmed.** A span is skipped only when *all* its
tokens are stopwords, so *"appeals of"* and *"of coastal"* are enumerated,
retrieved for, and scored — three SQL queries each — before losing to a
better span. They cost latency and add noise to the trace.

**The exact channel's ranks are arbitrary.** `SELECT … LIMIT 8` with no
`ORDER BY` returns rows in whatever order the index walk produces, and those
positions become RRF ranks. When one span has several exact matches, their
relative fused scores are determined by storage order.

**Co-term selection is unordered.** The coherence query is documented in the
code as returning co-occurring terms "strongest first", but the SQL has no
`ORDER BY` — `LIMIT 4` takes an arbitrary four. Worse, the filter that drops
co-terms already present in the span runs *after* the `LIMIT`, so a span
whose first four graph neighbours are its own tokens gets no coherence signal
at all even when useful neighbours exist.

**Dense hits are reported as `LexicalMatch`.** `trace_to_proto` maps every
channel score through the same constructor, so a vec0 cosine arrives over the
wire tagged as a lexical match with `channel = "dense"`. The `SemanticMatch`
message exists and has the right fields (`model`, `similarity`); nothing
emits it. A consumer must string-match the channel name to know it is looking
at a similarity rather than a BM25 score, and the model identity is not on
the evidence at all.

**Candidates are cells, not records.** One user row matching in two columns
produces two candidates that are never merged. For the legal corpus this is
harmless (one text column matters); for a `people(first_name, last_name)`
schema it means the same person appears twice and consumes two of the five
`TOP_K` slots.

**Scores are not calibrated.** `Candidate.score` is documented in the proto
as *"Calibrated confidence in [0, 1]"*. It is a fused heuristic in [0, 1]
with sensible ordering properties; it is not a probability, and 0.567 does
not mean "57% likely correct". `SELECT_THRESHOLD = 0.35` is a tuned
constant, not a calibrated operating point. Calibration needs the evaluation
harness of [06-evaluation.md](06-evaluation.md) closed against labelled data.

**Corpus-wide document frequencies.** The knowledge compiler's term
statistics come from `fts5vocab`, which cannot partition by source table, so
in a multi-table store the document-frequency ceiling that filters corpus
stopwords is computed against the whole index. Documented in the compiler;
exact for the common single-document-table case.

**`ResolveOptions` are ignored.** `max_candidates_per_mention`, `allow_lm`
and `min_confidence` are accepted by the server and have no effect;
`TOP_K` and `SELECT_THRESHOLD` are compile-time constants. Only `source` and
`session` are honoured.

**Resolution is serialized per database.** `stemma-server` holds
`Mutex<StemmaDb>` per registered database because `rusqlite::Connection` is
not `Sync`. Concurrent requests to the same database queue behind each other
— and now they queue behind a synchronous HTTP call to the embedding endpoint
as well, since `OpenAiEmbedder::embed` is blocking `ureq` with a 60-second
timeout called from inside the lock. The Python client's default timeout was
raised from 10 s to 30 s to accommodate this.

**Fusion constants were not re-derived for four channels.** Documented above
under [reachable score bands](#reachable-score-bands): the `3/K` normalizer
is unchanged, so the document ceiling moved from 0.567 to 0.85 and a
non-exact value can now reach 1.0.

**One dense table per store.** `vec_dense` is a single fixed table name with
a single `model_registry` row, so a store holds one vector generation for one
`(table, column)` pair at a time. Comparing two encoder checkpoints means
re-staging and restarting, not querying both — which is safe (spaces are
never mixed) but is not the side-by-side A/B the registry design allows for.

**Dense promotion happens only at server startup**, and staging must be
written while the server is stopped. There is no online re-embed.

## References

- [Cormack et al. 2009] Gordon V. Cormack, Charles L. A. Clarke, Stefan
  Buettcher. "Reciprocal Rank Fusion Outperforms Condorcet and Individual
  Rank Learning Methods." SIGIR 2009.
- [Hoffart et al. 2011] Johannes Hoffart et al. "Robust Disambiguation of
  Named Entities in Text." EMNLP 2011.
- [Paulsen et al. 2023] Derek Paulsen, Yash Govind, AnHai Doan. "Sparkly: A
  Simple yet Surprisingly Strong TF/IDF Blocker for Entity Matching."
  PVLDB 16(6), 2023.
- [Robertson & Zaragoza 2009] Stephen Robertson, Hugo Zaragoza. "The
  Probabilistic Relevance Framework: BM25 and Beyond." *FnTIR* 3(4), 2009.

Full bibliography: [00-bibliography.md](00-bibliography.md).
