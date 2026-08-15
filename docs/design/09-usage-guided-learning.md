# Usage-guided grounding and representation adaptation

Status: **typed feedback collection is implemented. Preference derivation,
training-example export, adaptive retrieval, and representation training are
designed research work.**

## Purpose

Stemma can improve from use because each judgment can be attached to the exact
resolution evidence that a person saw. The distinctive opportunity is to combine
four views of the same failure:

1. the resolution trace identifies the query span, candidate order, channel
   scores, and selected reading;
2. the knowledge graph records schema roles, paths, relations, and provenance;
3. the encoder exposes record geometry that Ambit can measure without labels;
4. the parser records the validated SQL operation and returned grounding uses.

Most feedback-learning work starts with a predicted query and a binary result.
Interactive semantic parsing shows that binary feedback can reduce annotation
load [Iyer 2017], while free-form corrections can repair inaccurate SQL
interpretations [Elgohary 2020]. Stemma can retain finer provenance because its
feedback event points into a trace and a graph-supported candidate set.

## Implemented event contract

Every non-empty resolution or parsing request may produce an opaque
`episode_id`. `query_log` stores the evidence revision, candidate presentation
order, clarification partitions, and parse output for that episode.
`grounding_feedback` stores one deliberate user judgment:

- `approved`;
- `rejected`;
- `wrong_meaning`;
- `missing_interpretation`;
- `wrong_query_operation`;
- `wrong_rows`.

The event may identify a span, candidate, or offered clarification option. A
correction can state a missing meaning. Scope is either one session or the
database. The server validates all selectors against the recorded episode and
rejects an event after the indexed-corpus or vector-registry revision changes.
The receipt does not cover a graph recompile against unchanged corpus inputs,
so graph-sensitive export must verify the graph again.

The service never updates an event in place. Users can list events and
permanently delete an event. No runtime component currently learns from the
log.

## Why a judgment is not yet a label

Approval means that a displayed result helped one user in one context. Rejection
without a reason does not reveal whether grounding, query structure, data
freshness, or presentation failed. Even clicks and reformulations, which are
plentiful interaction signals, are informative and systematically biased
[Joachims 2007].

The derivation pipeline therefore preserves four evidence grades:

| Grade | Evidence | Permitted use |
|---|---|---|
| observed | one explicit event | aggregate diagnostics |
| repeated | consistent events for the same supported reading and scope | bounded near-tie reorder experiment |
| reviewed | a person confirms the intended candidate or corrected SQL | regression fixture and export candidate |
| verified | reviewed correction passes schema, trace, and denotation checks | training and acceptance evaluation |

Silence, abandonment, automatic selection, and tool invocation never create an
event. Conflicting events remain visible. A derived artifact records every source
event and disappears when any governing deletion policy requires it.

## Calibration-free online improvement

The first adaptive behavior should be a bounded preference over candidates that
already have current evidence. A preference may reorder candidates inside an
existing near tie. It cannot add a candidate, cross an unsupported schema role,
or convert `unknown` into `resolved`.

The gate is count-and-contradiction based:

- require repeated explicit judgments within one declared scope;
- retain approvals and rejections separately;
- apply only when the active indexed-corpus and vector-registry revisions match;
- expose the preference and its event count in the trace;
- withdraw the preference after contradiction or source-event deletion.

This rule makes no probability claim. A representative labeled stream could
later support a calibrated estimator for its declared population. Until then,
evaluation reports exact event counts, conflict rates, and bounded behavior
changes. The small gold SQL set remains a regression and adversarial set. It
cannot establish how often a usage-derived preference will be correct in
deployment.

## Ambiguity hotspots from graph and geometry

Similarity crowding is only one source of ambiguity. The knowledge graph adds
structural hotspots:

- one phrase reaches several columns with different semantic roles;
- two entities share a surface value and connect to different fact tables;
- several join paths connect the same mentions;
- a high-frequency graph phrase or term reaches several schema roles;
- an inferred relation conflicts with declared foreign-key structure;
- graph evidence supports several candidates with materially different reach.

Ambit contributes per-record collision and neighborhood diagnostics. These
locate places where the encoder may fail to separate records. Indexed
interpretation identity, graph paths, schema roles, and database probes then
separate repeated interpretations, connected records, role competitors, and
pairs without graph support. A useful hotspot report joins these views:

```text
span and candidate confusion
  + graph role or path divergence
  + encoder collision diagnostics
  + observed feedback frequency and consequence
```

The report should appear inside the existing reasoning trajectory. It extends
the candidate fork with graph paths, role differences, collision diagnostics,
and feedback counts. The current console already places whole-episode approval
and rejection controls under that trajectory. Candidate-specific controls and
hotspot annotations remain unimplemented.

## Offline Ambit decision gate

Ambit remains an offline evaluation dependency until it proves incremental
value. The implemented
[`ambit_retrieval_study.py`](../../tools/ambit_retrieval_study.py) harness
compares six bounded retrieval policies on identical exported candidate pools:

| Policy | Expansion signal |
|---|---|
| existing | current retrieval bound |
| deeper fixed | rank only |
| score margin | fused-score proximity |
| graph directed | verified relation or role competition |
| Ambit directed | per-record collision or crowded-neighborhood diagnostic |
| combined | bounded union of graph and Ambit candidates |

The harness reports record-as-query and free-form queries separately because
corpus geometry directly represents the first population and only proxies the
second. It computes gold survival, supported-alternative recall, added candidate
count, ambiguity localization, false commitment, and measured latency. The input
contract binds each run to database and vector fingerprints. See
[`tools/ambit_retrieval_study.md`](../../tools/ambit_retrieval_study.md) for the
JSON Lines contract and commands.

The harness does not produce candidate evidence, integrate with runtime
retrieval, or establish that the sample represents future queries. A useful
study still needs reviewed inputs that carry graph-support and Ambit-collision
diagnostics. Analyses may stratify those inputs by collision count,
neighborhood size, graph relation, and query kind.

Runtime integration requires Ambit-directed expansion to beat score-margin and
graph-directed expansion at the same candidate and latency bounds. A negative or
inconclusive result keeps Ambit in diagnosis and training-data selection.

## Exporting reviewed examples

A separate exporter may turn verified events into three kinds of examples:

1. query-to-record positives from approved candidate selections;
2. reviewed hard negatives from role competitors with graph evidence;
3. query-to-SQL examples from reviewed parse corrections.

The exporter includes indexed-corpus and vector-registry revisions, trace
selectors, graph paths, event identifiers, scope, and review status. It excludes
raw events, conflicts, stale revisions, and examples whose target is no longer
present. A deletion tombstone removes exported derivatives before another
training run.

Train, validation, and evaluation splits group by database, entity family,
session, and time. This prevents paraphrases or repeated judgments about the same
entity from crossing split boundaries. Actively selected examples are tracked as
a distinct population because active sampling changes the label distribution
[Zhao 2021; Zhan 2022].

## Representation adaptation

Representation training is the last repair. The order of comparison is:

1. normalization, deduplication, and alias repair;
2. bounded score-margin expansion;
3. graph-directed expansion and reranking;
4. a small learned reranker over frozen embeddings;
5. parameter-efficient encoder adaptation;
6. broader encoder training.

Ambit can weight examples by measured collision pressure and mine confusable
neighbors. Database and graph evidence supply the false-negative guard. A close
pair is safe to repel only when reviewed evidence and distinct schema roles say
that the records remain different for the task. Repeated indexed
interpretations and database-verified co-answer records become positives or
protected neighbors. Supervised contrastive objectives pull same-class examples
together and separate different classes [Khosla 2020]. Incorrect pair
construction can therefore damage the space directly.

The first encoder experiment should use low-rank adaptation (LoRA) while
freezing the base weights [Hu 2022]. This bounds the number of trainable
parameters and makes rollback simple. It does not remove the risk of forgetting
established retrieval behavior. Evaluation must retain frozen regression and
out-of-scope retrieval populations because learned systems can lose previous
behavior when trained on a new task [Kirkpatrick 2017].

Every trained generation is registered beside the previous vector index. The
evaluation matrix compares retrieval, ambiguity preservation, false commitment,
and established regression fixtures before promotion. Geometry improvement alone
cannot license a swap because more isotropic spaces can still retrieve worse on
the target task.

## Research claim

The academic problem is evidence-preserving interactive semantic parsing under
catalog ambiguity and sparse, biased feedback. Existing work studies interactive
parse correction [Yao 2019; Elgohary 2020], learning from binary user feedback
[Iyer 2017], ambiguous question sets [Min 2020], and embedding geometry. The
cited work does not establish the combined claim below. Stemma can test it by
connecting the levels through stable trace selectors and a database-derived
knowledge graph:

> Database-verified, graph-described alternative sets and geometry diagnostics
> can turn sparse user judgments into safer clarification, retrieval repair, and
> reviewed training examples without treating unrepresentative feedback as
> calibrated truth.

The hypothesis is falsifiable. Each adaptation must beat simpler repairs on fixed
bounds, preserve known cases, and publish negative results.

## Simplicity constraints

The event log remains the only source of usage evidence. Consumers derive
preferences, fixtures, reports, or training examples outside request handling.
Runtime code does not acquire a training framework, experiment registry, Ambit
dependency, or hidden user profile. Each production behavior requires a measured
acceptance gate and a trace-visible explanation.
