# The evaluation harness

This document designs the harness that turns
[06-evaluation.md](06-evaluation.md)'s protocol and metric definitions into
a runnable, repeatable measurement — the thing that runs on every
substantive change and reports whether resolution got better, worse, or
just different. 06 defines *what the numbers mean*; this document defines
*which experiments produce them*, why the design is grounded, and how a run
is graded.

Status: **designed, agreed in review; not yet built.** The consumer is
`crates/stemma-eval` plus a small amount of Python for synthetic query
generation.

## Context

Every scoring constant in the pipeline today was set by reasoning and
spot-checks: the dense calibration window (`[0.30, 0.60] → [0, 0.78]`), the
document ceiling (0.85) and exact floor (0.9), `COHERENCE_BOOST = 0.15`,
`ADJUDICATION_MARGIN = 0.08`, the selection threshold (0.35). The reasoning
is documented ([03-resolution.md](03-resolution.md)) and the spot-checks
were real, but nothing currently distinguishes "this constant is right" from
"this constant survived the queries we happened to try". Meanwhile the
pipeline has grown mechanisms — the dense channel, collective
disambiguation, the adjudication band — whose *individual* contribution has
never been isolated. The system's own thesis is resolution as a measured
artifact with honest evidence; its constants should meet the same bar.

## Motivation, and the shape the literature demands

The direct model is **GraphRAG-Bench** [Xiang 2025], which asked of
graph-augmented RAG exactly the question we must ask of stemma's mechanisms:
*when does the added structure actually help?* Its central finding
discipline-checks our design: across seven GraphRAG systems, graph structure
helped on multi-hop reasoning and synthesis tasks and **did not help — and
sometimes hurt — on simple fact retrieval**, a result only visible because
the benchmark tiered its tasks by the evidence topology required and
reported per tier. A blended leaderboard number would have hidden it.

Three further ideas carry over directly:

1. **Evaluate the pipeline, not just the endpoint.** GraphRAG-Bench scores
   graph construction (node/edge counts, degree, clustering), retrieval
   (evidence recall, context relevance), and generation (accuracy,
   faithfulness, coverage) as separate layers, and *correlates* them —
   denser graphs measurably bought better multi-hop recall. We adopt the
   layering and the correlation habit.
2. **Faithfulness is a metric, not a vibe.** Their generation layer checks
   answers against retrieved context. Our sharper analog: explicit-NIL
   behavior, measured on queries whose answers are genuinely absent.
3. **Cost on the same axis as quality.** Their prompt-inflation finding
   (graph pipelines bloating contexts to ~40k tokens and *introducing
   noise*) is the cautionary tale for every mechanism we add. Latency, probe
   counts and LM routing rates are reported next to recall, never in a
   separate appendix.

One deliberate divergence: GraphRAG-Bench leans on LLM-judged metrics
because free-text generation is its output. Stemma's output is **rowids**,
so the core harness is exact-match, deterministic, and cheap enough to run
on every commit. LLM judgment appears only in the (separately gated)
agent-grounding layer, and even there mechanical checks are preferred.

## What the older evaluation traditions add

Value linking sits at the junction of four fields with mature evaluation
literatures — entity linking, text-to-SQL, ad-hoc retrieval, and RAG — and
each contributes a hard-won lesson this harness adopts.

**Entity linking: matching modes must be explicit, and NIL is part of the
headline score.** GERBIL [Röder 2018] standardized EL evaluation after a
decade in which systems were incomparable partly because *annotation
matching* was underspecified — does a predicted span count when it overlaps
the gold span, or only when it equals it? We adopt its discipline twice:
mention-detection F is reported under both **strict** (byte-identical span)
and **weak** (token-overlap) matching, because greedy segmentation
legitimately shifts boundaries ("Chen" vs "Chen's") and the strict/weak gap
is itself a segmentation-quality signal; and both **micro** (per-mention)
and **macro** (per-query) aggregates are reported, since our queries carry
one to four mentions and micro-only reporting would over-weight multi-
mention queries. From TAC-KBP's decade of EL tracks [TAC-KBP 2013], whose
B³+ metric refused to score linking and NIL-handling separately: our
headline per-query credit is **conjunctive** — a query scores fully only
when its mentions are detected *and* linked to gold rowids, with NIL
queries scoring only on affirmed absence. Partial credit exists in the
diagnostic decomposition, never in the headline.

**Text-to-SQL: single-instance execution creates silent false positives.**
Test-suite accuracy [Zhong 2020] showed that comparing query denotations on
one database wrongly accepts semantically different SQL whenever the
instance happens to make them agree (2.5% average, 8.1% worst-case on
Spider) — the fix was checking against many distilled instances. The
resolution analog: a candidate whose *value* equals the gold literal may
still be the wrong record — the same string in the wrong column, or in one
of several rows the predicate does not select. So targets are
**denotation-verified** at derivation time (the gold literal must select
the gold rows under the gold predicate on the actual instance), and
"points at" matching is scored at two strictnesses: **value-loose** (06's
normalized equality anywhere) and **column-strict** (equality in the gold
column). The gap between them is the measured coincidence rate — the exact
failure class Zhong showed single-instance execution hides.

**Ad-hoc retrieval: the lexical baseline is not a strawman, and deltas
need statistics.** BEIR [Thakur 2021] found BM25 a top-tier *zero-shot*
retriever — dense models that win in-domain routinely lose out of domain —
which is why the `lex` ablation is published in every matrix as a
first-class row, never as a foil, and why per-corpus numbers are never
averaged across corpora (BEIR's heterogeneity lesson: the average of
Spider-clean and BIRD-dirty describes neither). For grading, the IR
statistics literature is unambiguous: paired randomization or t-tests
agree with each other while Wilcoxon/sign tests agree with nothing
[Smucker 2007], and comparing many systems needs multiple-comparison
control [Carterette 2012], with the field's systematic reviews finding
chronic underreporting of effect sizes and power [Sakai 2016]. The
harness therefore attaches a **paired randomization test and a
bootstrapped confidence interval to every cell delta**, uses randomised
Tukey HSD when an ablation sweep compares more than two variants, and
treats the 1-point grading guard as a floor, not a substitute — a
significant 0.8-point regression on a 250-query tier still fails review.

**RAG evaluation: judged metrics need calibration against humans.** KILT
[Petroni 2021] made provenance conjunctive — its KILT-scores award the
answer point only when the gold provenance pages are also retrieved — and
that is precisely our layer-3 rule: an agent answer is credited only when
its citations ground to gold rowids in the same turn's tool results.
Where LLM judges eventually enter (auditing the synthetic set, scaling
layer-3 beyond mechanical checks), ARES [Saad-Falcon 2024] supplies the
sober pattern: judge outputs are corrected against a small human-labeled
set via prediction-powered inference [Angelopoulos 2023] to produce valid
confidence intervals, rather than trusted raw as reference-free scores
[Es 2024] are. The ~200-query human skim before freezing the legal set
doubles as that labeled anchor.

## Why this is grounded

Two properties keep the harness honest:

**Ground truth is derived, never hand-labeled by us.** For BIRD, every
literal in a gold query's WHERE/HAVING clause names the (column, value,
rows) a human meant — targets fall out of parsing SQL the benchmark's own
authors wrote, under the no-evidence protocol
([06-evaluation.md](06-evaluation.md#the-no-evidence-protocol)). For the
legal corpus, targets are constructed in reverse (sample a record → generate
an oblique question about it), so the gold rowid is known *by construction*.
In both cases the labels cannot drift toward what the system finds easy,
because we never label answers by looking at system output.

**Tier membership is verified mechanically, not trusted from generation.**
The input under evaluation is always the full natural-language question —
span detection and segmentation are inside the system under test, because
that is where real failures happen (a segmentation failure sent "fired from
a" into a mortar regulation; no phrase-level probe would have caught it).
That makes query realism a dataset property to protect: a "semantic tier"
query that secretly has a lexical anchor measures nothing. So tier
assignment is checked by machine: an L2 candidate qualifies only if none of
its content tokens produce an exact or trigram hit on the gold row; an L3
candidate only if its gold tuple actually traverses a join path. Queries
that fail verification are regenerated or discarded.

## The three layers

### Layer 1 — resolution (the core)

Input: an NL question. System under test: `resolve_full` over a registered
corpus, with a **mechanism ablation** selected per run:

| Ablation | What runs |
|---|---|
| `lex` | exact + bm25 + trigram + RRF only |
| `+dense` | … + targeted dense channel, whole-query span, cosine floor |
| `+kg` | … + kg mention detection + term coherence |
| `+coh` | … + collective disambiguation |
| `+adj` | … + LM adjudication band (`allow_lm`) |

Ablations are cumulative in the pipeline's own order, and each is a flag
combination the server already supports (absent embedder = no dense; absent
KG = no coherence; `allow_lm` gates adjudication). No eval-only code paths
in the pipeline — the harness must measure the shipping system.

Queries carry a **tier**, named for the mechanism the query is constructed
to require:

| Tier | Requires | Example shape |
|---|---|---|
| L1 | lexical anchor | "the Q3 numbers for the Seattle office" |
| L2 | semantic resolution (zero lexical overlap, verified) | "getting fired from a state job" |
| L3 | relational coherence (multi-mention, join-path decides) | "what did Chen's Billing team ship" |
| L4 | cross-record co-answer (≥2 gold rows, often ≥2 tables) | "overdraft fees on checking accounts" |
| NIL | honest absence (answer verifiably not in corpus) | "who inspects restaurant kitchens" (retail food code is statute, not CCR) |

### Layer 2 — construction

Per corpus, per run: KG statistics (nodes, edges, degree distribution,
inferred-join precision where declared FKs exist to check against), dense
coverage (fraction of documents embedded, queue backlog), and index sizes.
These are not goals; they exist to be **correlated with layer-1 lift** —
"coherence lift scales with join-edge density" is a testable claim, and if
it fails, the KG construction budget is misallocated.

### Layer 3 — agent grounding (separately gated)

On recorded transcripts: resolve-before-claim compliance (the agent may not
assert corpus absence without a resolve call in the trail), citation
grounding (every cited `table.column #rowid` must appear in a tool result in
the same turn), and no-padding (an honest-absence answer contains no
general-knowledge substitution). The first cut ships three regression cases
transcribed verbatim from live failures (2026-08-02: absence-without-resolve;
LIKE-scan context flooding; general-knowledge padding), asserted
mechanically against the trail structure. LLM judgment is a later, optional
refinement — the mechanical checks caught all three real failures.

## What is measured, and why

All layer-1 metrics follow 06's definitions and its recall-weighted stance
(a missed record is unrecoverable downstream; extra candidates are
filterable noise). Per (tier × ablation) cell:

- **Mention-detection F** (β = 2), under both strict and weak span
  matching, micro- and macro-aggregated [Röder 2018]: do selected spans
  cover the tokens the gold SQL filters on? Catches segmentation failures
  independently of retrieval; the strict/weak gap measures boundary drift.
- **Candidate recall@k** (k = 1, 5, ∞) and **MRR** of the gold row, with
  the k-pattern diagnosis table from 06 (ranking vs threshold vs retrieval
  failure), each at value-loose and column-strict matching [Zhong 2020].
- **Grounded-query rate**: the conjunctive headline — mentions detected
  *and* gold rowids linked (NIL affirmed, for the NIL tier), in the
  B³+/KILT lineage [TAC-KBP 2013; Petroni 2021]. One number per (tier ×
  ablation) cell that cannot be gamed by partial success.
- **NIL-precision / NIL-recall** on the NIL tier: NIL-precision is the
  fraction of no-mention (or below-threshold) outcomes that are correct
  absences; NIL-recall is the fraction of absent-answer queries that did
  *not* produce a confident wrong mention. A valid-but-wrong resolution is
  the worst failure the system's positioning names; it gets its own number.
- **Calibration curve**: P(gold ∈ selected | score bucket), 10 buckets.
  The scores claim absolute meaning — bands at 0.35/0.85/0.9 — and this is
  the direct test of that claim. Feeds the ambit-informed calibration work:
  if the curve is corpus-dependent, per-corpus calibration is justified by
  measurement, not taste.
- **Cost**: median/p95 latency, dense probes per query, adjudication
  routing rate and LM round-trip time, selected candidates per mention.
  Every mechanism's lift is quoted *with* its cost or not at all.

The **primary artifact of a run is the mechanism × tier matrix** — recall@5
per cell with deltas against the previous accepted run — not any single
number. Expected shape, stated in advance so deviations are findings:
`+dense` lifts L2 and should not move L1; `+coh` lifts L3 and should not
move L1/L2; `+adj` buys points on ties wherever they occur, at bounded
routing cost. A mechanism moving a tier it has no business moving is a
regression *even if the movement is upward* — it means the mechanism fires
where its evidence is not real, and the next corpus will pay for it.

## How a run is graded

A run passes when:

1. **No cell regresses** against the accepted baseline (stored in-repo as
   a small JSON, updated deliberately and reviewed like code). "Regresses"
   means: a drop exceeding 1 point of recall@5, **or** any drop a paired
   randomization test marks significant at α = 0.05 [Smucker 2007] — the
   1-point guard is a floor, not a license. Cell deltas carry bootstrapped
   confidence intervals; sweeps comparing more than two variants use
   randomised Tukey HSD for multiple-comparison control [Carterette 2012].
2. **Tier-mechanism containment holds**: off-target cells move < 1 point in
   either direction (see above — upward drift off-target is also a fail).
3. **NIL-precision does not drop**; any new confident-wrong on the NIL set
   is surfaced as a named case in the report, not just a rate.
4. **Cost envelopes hold**: p95 latency per tier and adjudication routing
   rate stay within declared budgets (budgets live next to the baseline
   JSON; changing them is a reviewed decision).
5. Layer-3 regression cases stay green.

The report is one Markdown file per run (the matrix, deltas, named
failures with their trajectories linked), written where the console can
serve it later — the eval dashboard is downstream product work, the file
format should not block on it.

## Datasets

- **BIRD dev slice**: 5–8 databases spanning clean and dirty schemas,
  questions verbatim, evidence field withheld (the no-evidence protocol),
  targets derived per 06. Primarily L1/L3/L4; BIRD questions are rarely L2.
- **Legal synthetic**: ~200 questions, ~50 per tier plus ~25 NIL,
  reverse-generated by the LM against sampled records with mechanical tier
  verification (above) and a human skim in the console before freezing.
  Frozen after review — the set is versioned, and regenerating it is a
  reviewed change, because silent regeneration is how eval sets drift
  toward what the current system finds easy.
- **Mini**: the existing golden tests already pin exact behaviors (the Chen
  case, overlap semantics); they join the harness unchanged as the
  correctness floor.

## What is deliberately out of scope

Execution accuracy (06's argument stands: it measures the downstream
generator, not stemma). Cross-encoder reranking and mention expansion are
*not* in the first matrix — they land as new ablation columns when built,
which is the point of the design: the matrix is the standing instrument new
mechanisms report into, in the same units as everything before them.

## References

- [Xiang 2025] Yilin Xiang et al. "When to use Graphs in RAG: A
  Comprehensive Analysis for Graph Retrieval-Augmented Generation"
  (GraphRAG-Bench). arXiv:2506.05690.
- [Röder 2018] Michael Röder, Ricardo Usbeck, Axel-Cyrille Ngonga Ngomo.
  "GERBIL — Benchmarking Named Entity Recognition and Linking
  Consistently." Semantic Web 9(5), 2018.
- [TAC-KBP 2013] "TAC KBP Entity Linking Task Description v1.0" (B³+
  scoring, NIL clustering). NIST TAC, 2013.
- [Zhong 2020] Ruiqi Zhong, Tao Yu, Dan Klein. "Semantic Evaluation for
  Text-to-SQL with Distilled Test Suites." EMNLP 2020. arXiv:2010.02840.
- [Thakur 2021] Nandan Thakur, Nils Reimers, Andreas Rücklé, Abhishek
  Srivastava, Iryna Gurevych. "BEIR: A Heterogeneous Benchmark for
  Zero-shot Evaluation of Information Retrieval Models." NeurIPS 2021
  Datasets & Benchmarks. arXiv:2104.08663.
- [Petroni 2021] Fabio Petroni et al. "KILT: a Benchmark for Knowledge
  Intensive Language Tasks." NAACL 2021. arXiv:2009.02252.
- [Saad-Falcon 2024] Jon Saad-Falcon, Omar Khattab, Christopher Potts,
  Matei Zaharia. "ARES: An Automated Evaluation Framework for
  Retrieval-Augmented Generation Systems." NAACL 2024. arXiv:2311.09476.
- [Es 2024] Shahul Es, Jithin James, Luis Espinosa-Anke, Steven Schockaert.
  "RAGAs: Automated Evaluation of Retrieval Augmented Generation."
  EACL 2024 Demos. arXiv:2309.15217.
- [Angelopoulos 2023] Anastasios N. Angelopoulos, Stephen Bates, Clara
  Fannjiang, Michael I. Jordan, Tijana Zrnic. "Prediction-Powered
  Inference." Science 382(6671), 2023. arXiv:2301.09633.
- [Smucker 2007] Mark D. Smucker, James Allan, Ben Carterette. "A
  Comparison of Statistical Significance Tests for Information Retrieval
  Evaluation." CIKM 2007.
- [Carterette 2012] Ben Carterette. "Multiple Testing in Statistical
  Analysis of Systems-Based Information Retrieval Experiments." ACM TOIS
  30(1), 2012.
- [Sakai 2016] Tetsuya Sakai. "Statistical Significance, Power, and Sample
  Sizes: A Systematic Review of SIGIR and TOIS, 2006–2015." SIGIR 2016.
- BIRD, DIVER, SEED: see [00-bibliography.md](00-bibliography.md).
