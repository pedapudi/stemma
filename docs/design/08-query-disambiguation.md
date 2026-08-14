# Grounding-first semantic parsing

Status: **grounding outcomes, record clarifications, and the first validated
semantic-parsing slice are built; broader query coverage is staged**.

## Destination

Stemma is intended to parse a natural-language request into a safe, read-only
SQLite query. Grounding remains the first investment because a syntactically
valid query over the wrong record, column, or join is still wrong.

The design has two authoritative representations:

```text
resolution trace → grounded SQLite syntax tree
```

The trace owns spans, candidates, paths, evidence, alternatives, and
clarification constraints. The syntax tree owns executable query structure.
Grounding provenance is a small sidecar of references from syntax locations
back into the trace. There is no intermediate `GroundedQuery`, custom semantic
representation, or copied evidence graph.

## Why the trace is the grounding contract

Resolution answers which database objects the words may denote and why. It
must preserve losing readings because premature commitment makes a missed
interpretation unrecoverable. Stable references for candidates and verified
paths are added only when parser fixtures require them.

A second grounding object would copy selected candidates, create
synchronization rules, and force every client to understand two accounts of
the same evidence. A “grounded query” is therefore a property of a trace whose
material grounding ambiguity has been settled, not another public type.

The trace distinguishes five query-level outcomes:

| Outcome | Meaning |
|---|---|
| `resolved` | One material grounding survives. |
| `equivalent` | Several readings survive but produce the same verified record set for this database revision. |
| `ambiguous` | Supported readings can materially change the result. |
| `unknown` | The intended record or schema concept may be outside the catalog or candidate set. |
| `unanswerable` | Parsing or registered data proves the request cannot be expressed safely. |

Grounding alone normally emits the first four. General answerability belongs
to parsing because it depends on the requested operation, not only its
referents.

## Grounding completion gate

Parser coverage expands only after grounding evaluation demonstrates:

- at least 95% precision for material ambiguity;
- fewer than 1% confident consequential grounding errors;
- at least 90% of answerable ambiguities resolved within two clarification
  turns;
- no regression in absence precision or established evaluation tiers;
- calibrated reliability and risk–coverage reports for decision thresholds;
- latency within the existing per-tier budgets.

The grounding corpus must cover competing records, same values in different
columns and roles, competing tables and join paths, NIL, multiple ambiguous
mentions, revision-scoped equivalence, incomplete replies, contradictions,
and rejection of every offered option.

Clarification remains deterministic. Each question partitions viable trace
readings, cites their evidence, and asks about one localized difference. One
question is returned at a time. Stateless continuation carries the original
query, database revision, and accepted grounding constraints; persistence is
added only if multi-turn measurements show that this is insufficient.

## Proposal service

After grounding is settled, an optional language service proposes a small,
fixed number of parameterized read-only queries. It receives only the user
request, relevant trace candidates and rivals, verified schema paths, column
types, accepted constraints, and required clock or timezone context.

The response is schema-constrained and contains SQL, typed parameters,
grounding references, and categorized assumptions. Temperature, request size,
proposal count, output size, and timeout are bounded. A malformed response or
a set of mechanically invalid proposals permits one correction request with
structured failure codes; there is no open-ended repair loop.

The service is a proposer, never a correctness oracle. If it is unavailable,
resolution and grounding clarification remain available and parsing reports a
distinct availability failure. Endpoint and deployment selection are explicit
configuration and are never hard-coded.

## Grounded SQLite syntax tree

Every proposed statement is parsed with the SQLite dialect before any database
connection is used. A compact internal result associates the syntax tree and
parameters with provenance references:

```text
ParsedQuery
  syntax_tree
  parameters[]
  grounding_uses[]
  assumptions[]
  validation_receipt
```

Evidence stays in the trace. A grounding use names a syntax location, table,
column, trace span, trace candidate, and optional verified path. The public
response returns canonical parameterized SQL and provenance; it does not
publish a redundant serialized syntax tree.

Deterministic validation rejects proposals that:

- contain anything other than one read-only query;
- name a table, column, value, or join unsupported by schema and trace
  evidence;
- rely on an unresolved grounding alternative;
- violate type, aggregate, grouping, window, or set-operation rules;
- contain a disconnected or unverified join;
- interpolate a user-derived value rather than binding it;
- depend on an unstated temporal, unit, or business assumption;
- exceed configured bounds on depth, joins, subqueries, or parameters.

The first slice stops after deterministic syntax and grounding validation.
A later slice may add a bounded read-only execution probe with a row cap,
time limit, progress handler, and result truncation. SQLite continues to own
query planning and optimization.

## Parse-time ambiguity

Several valid syntax trees may remain after grounding. Normalize and compare
them structurally, then localize the smallest material difference: source,
join, projection, predicate, aggregation, grouping, temporal interpretation,
ordering, or result granularity. Deterministic templates turn that difference
into one clarification.

Trees are equivalent only when normalization or verified denotation proves it
for the current database revision. Accidental agreement on one instance is
not permanent semantic equivalence.

## Coverage order

Each step ships only with fixtures, validation, and end-to-end execution:

1. projection, filters, ordering, limits, and offsets;
2. aggregation, distinctness, grouping, and aggregate filters;
3. multi-table joins;
4. Boolean composition, ranges, membership, patterns, and nulls;
5. temporal intervals, calendars, and timezones;
6. arithmetic, units, and derived measures;
7. subqueries and set operations;
8. common table expressions and windows;
9. corrections and compositional follow-ups.

The target is the complete safe, read-only SQLite query surface.

## Evaluation layers

The default suite is hermetic: scripted proposals, deterministic vector
fixtures, real syntax parsing, deterministic validation, and real SQLite
execution. It requires no network service.

A separately invoked live suite performs capability checks, then evaluates
fixed acceptance fixtures. It records availability, an anonymous deployment
fingerprint, date, latency, structured-response validity, syntax parse rate,
first-pass validation, correction success, grounding violations, execution
accuracy, and normalized-tree stability. It never prints deployment
identifiers. A small representative sample runs three times; comparison uses
normalized trees and denotations rather than response text.

Correctness comes from gold traces and queries, deterministic validation, and
controlled database denotations. Live output never supplies gold labels.
Semantic retrieval has a separate pinned-vector benchmark; the proposal
service is not assumed to provide embeddings.

## Simplicity constraints

Reject a change that introduces a second grounding object, custom semantic
representation, duplicate syntax tree, parallel parser, copied evidence,
unvalidated identifiers, literal interpolation, clause-by-clause service
calls, more than one repair, a separate optimizer, or a feature without an
execution fixture. Reject traits, registries, factories, switches, and
persistence introduced for only one implementation.

Proposal, validation, execution, and protocol projection remain separate
small functions. Runtime code is compact, typed, and direct. Documentation
may be detailed; speculative runtime abstraction is not.

## Research basis

The ambiguity taxonomy and evaluation requirements draw on ambiguity sets
[Min 2020], staged clarification [Lee 2023], localized interaction [Yao 2019],
database ambiguity and unanswerability [Dong 2025], multi-faceted schema-level
evaluation [Sarwar 2026], specific-question utility [Rahmani 2024], and NIL
type separation [Zhu 2023]. Full citations are in
[the bibliography](00-bibliography.md#i-query-ambiguity-and-clarification).
