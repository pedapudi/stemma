# Grounding-first semantic parsing

Status: **resolution outcomes, deterministic grounding clarification, validated
read-only parsing, and trace-linked explicit feedback are implemented. Complete
SQLite coverage, query-structure clarification, and learned adaptation remain
research work.**

## Ambition

Stemma aims to turn a natural-language request into a safe, executable SQLite
query while preserving every database-supported interpretation that could
materially change the answer. “Fully disambiguated” means that one of four
conditions is explicit:

- one interpretation remains;
- several interpretations have verified equivalent denotations for the active
  database revision;
- a localized question separates the remaining interpretations;
- the system reports that its evidence cannot support an answer.

This is a completeness claim over interpretations supported by the registered
database, lexical index, vector index, and knowledge graph. It is never a claim
that the system has enumerated every meaning a person could imagine. Work on
ambiguous question answering likewise represents a question as a set of
plausible answers or rewrites rather than forcing one answer [Min 2020].

## Authoritative representations

The runtime has two authoritative representations:

```text
resolution trace → grounded SQLite syntax tree
```

The trace owns spans, candidate order, evidence, graph paths, alternatives,
outcomes, and grounding clarification. The SQLite syntax tree owns executable
query structure. `GroundingUse` references connect syntax locations to trace
spans and candidates.

A separate semantic intermediate representation would duplicate evidence and
create synchronization rules. Stemma therefore has no public `GroundedQuery`
type. A query is grounded when its trace has no unresolved material grounding
alternative and deterministic validation accepts the proposed SQLite tree.

## Resolution outcomes

| Outcome | Meaning |
|---|---|
| `resolved` | One material grounding survives. |
| `equivalent` | Several readings survive and verified denotation shows the same result for the active database revision. |
| `ambiguous` | Supported readings can materially change the result. |
| `unknown` | The intended record or schema concept may be outside the candidate set. |
| `unanswerable` | Parsing or registered data proves that no safe expression is available. |

The current trace derives `resolved`, `ambiguous`, and `unknown` from grounding
evidence. `equivalent` and `unanswerable` require denotation or parse evidence
that the resolver does not yet carry.

## Decision policy without held-out calibration

Stemma does not assume a representative labeled calibration set. A small set of
gold queries can reveal regressions and concrete counterexamples. It cannot
justify a population-level probability or a universal confidence threshold when
the usage distribution is unknown.

The default decision policy is therefore evidence-conservative:

1. Exact database matches, verified graph paths, schema constraints, and bounded
   database probes establish support.
2. Score margins and encoder similarity order supported candidates. They do not
   become probabilities by convention.
3. A close material alternative produces `ambiguous` even when one candidate is
   ranked first.
4. Missing support produces `unknown`.
5. A clarification partitions recorded candidates. Its value is measured by the
   alternatives it eliminates and the consequences it separates.

Entropy and expected information gain require a probability distribution over
the alternatives [Shannon 1948]. Candidate scores do not supply that
distribution. Stemma therefore proposes set reduction and worst-case
consequence reduction for clarification ranking:

$$
U(q) = \min_a \bigl(|V| - |V_a|\bigr),
\qquad
U_D(q) = \min_a \bigl(D(V) - D(V_a)\bigr),
$$

where $V$ is the set of viable interpretations and $V_a$ is the subset retained
by answer $a$. $D(S)$ is the maximum verified pairwise denotation divergence
inside set $S$. The first quantity rewards a question whose weakest supported
answer still removes alternatives. The second rewards a question whose weakest
supported answer still removes consequential ambiguity. Neither quantity
requires calibrated probabilities.

These equations specify a research ranking rule; the current planner selects
the first ambiguous mention. A complete question must also permit rejection of
every offered interpretation. That response produces `unknown` and receives no
set-reduction credit.

If a representative labeled stream later exists, reliability diagrams,
selective-risk curves, and information gain become additional measurements.
They do not replace the evidence rules. Distribution shift can invalidate
post-hoc calibration [Ovadia 2019]. Active sampling can also bias the labeled
population [Zhao 2021; Zhan 2022].

## What a small gold SQL set can establish

A small, unrepresentative gold set has four valid roles:

- immutable regression fixtures for known operations and ambiguities;
- adversarial tests that make each supported alternative change the result;
- metamorphic tests for paraphrase, irrelevant-clause, row-order, and harmless
  schema changes;
- anchors for checking whether usage-derived changes forget established cases.

The set does not estimate deployment accuracy. Reports keep controlled cases,
gold SQL cases, and observed usage cases separate. Execution agreement on one
database instance is insufficient because different SQL can agree accidentally;
distilled test suites reduce that false acceptance [Zhong 2020].

## Grounding clarification

The implemented clarification planner chooses one ambiguous mention and asks a
deterministic question about a relation, attribute, or record distinction. Each
option stores candidate indices from the presented trace. This follows the
interactive semantic-parsing pattern of localizing an uncertain component and
requesting targeted input [Yao 2019]. Specific questions are more useful than
generic requests for clarification [Rahmani 2024].

The public protocol returns one question. A feedback event can identify one
offered option or one candidate. Multi-turn continuation that carries accepted
constraints into a new trace remains unimplemented.

## Trace-linked explicit feedback

Each non-empty Resolve, Explain, or Parse request attempts to record an opaque
episode identifier in `query_log`. A successful write returns that identifier.
Resolution still succeeds with an empty identifier if persistence fails. A
recorded episode stores:

- the request kind and history attribution;
- indexed-corpus and vector-registry revisions;
- candidate identities in presentation order;
- clarification option partitions;
- parse status, accepted output, and validation failures when parsing ran.

`SubmitFeedback` records a typed event in `grounding_feedback`. Categories
distinguish approval, unexplained rejection, wrong meaning, missing
interpretation, wrong query operation, and wrong returned rows. A target may
identify a span, candidate, or offered clarification option. The server rejects
unknown selectors, incompatible categories, and episodes whose indexed-corpus
or vector-registry revision is no longer active. The revision check does not
cover a knowledge-graph recompile against unchanged corpus receipts. Any
graph-sensitive derived artifact must verify graph evidence again.
`ListFeedback` and `DeleteFeedback` provide inspection and permanent deletion.

An event is evidence about one displayed episode. It is not automatically a
preference, correctness label, or training example. Research on implicit search
feedback finds that user actions are informative and biased, which makes direct
absolute labels unsafe [Joachims 2007]. Stemma records deliberate controls and
still keeps the derivation boundary explicit.

## Encoder geometry and graph structure

Ambit measures where an encoder places records too close together for a stated
noise budget. The indexed interpretation key and knowledge graph show whether a
close pair repeats one interpretation, crosses schema roles, follows a database
relation, or lacks graph support. These signals answer different questions:

| Signal | Question |
|---|---|
| encoder crowding | Which records may be hard to retrieve or rank apart? |
| interpretation identity, graph relation, and provenance | Do the records repeat one indexed meaning, occupy different roles, or connect through the database? |
| observed feedback | Did the displayed interpretation help for this request? |
| gold SQL or reviewed correction | Which interpretation and operation are accepted for a known case? |

An offline study must compare existing bounded retrieval, deeper fixed
retrieval, score-margin expansion, graph-directed expansion, Ambit-directed
expansion, and their bounded combination on the same queries. Runtime Ambit
integration is justified only if per-record diagnostics improve gold survival
or ambiguity localization beyond the simpler score and graph rules. Geometry
does not weaken declared database relations or same-interpretation identity. It
weakens embedding evidence when database structure keeps close records distinct.

## Proposal and deterministic validation

After grounding is settled, an optional language service proposes at most three
parameterized read-only queries. It receives the request, relevant candidates,
verified schema paths, accepted constraints, and required temporal context. A
malformed or entirely invalid response permits one bounded correction request.

Every proposal is parsed with the SQLite dialect. Validation rejects statements
that write data, contain multiple statements, interpolate user-derived values,
name unsupported schema objects, use unverified joins, mismatch parameter types,
or depend on unresolved grounding. One surviving proposal is returned as
canonical SQL with typed parameters and grounding references. Multiple surviving
proposals currently produce an explicit invalid-proposal result. Query-structure
alternatives and localized parse clarification remain unimplemented.

## Evaluation contract

The hermetic suite uses scripted proposal responses, pinned vectors, real SQLite
parsing, deterministic validation, and controlled denotations. A separate live
suite checks configured service capability and stability. Live output never
supplies gold labels.

Evaluation reports these populations separately:

- controlled ambiguity fixtures;
- a small immutable gold SQL set;
- reviewed usage feedback;
- record-as-query retrieval cases;
- free-form paraphrase cases.

Primary safety measures include:

- **false commitment rate**: the share of requests resolved to a consequentially
  wrong or unsupported interpretation;
- **supported-alternative recall**: the share of database-supported material
  readings that remain in the candidate set;
- **gold candidate survival**: whether a known accepted reading survives each
  retrieval and selection boundary;
- **clarification localization**: whether the question addresses the recorded
  difference that changes the result;
- execution correctness and latency.

Every learned or geometry-directed change must beat a simpler bounded repair on
the same population. It must preserve the immutable regression set and absence
behavior.

## Simplicity constraints

Runtime code remains compact, typed, and direct. Stemma rejects a second
grounding object, a custom semantic representation, copied evidence graphs,
unvalidated identifiers, literal interpolation, clause-by-clause service calls,
open-ended repair, a parallel query optimizer, and abstractions with only one
implementation. Code review treats runtime verbosity as a defect: extra layers,
duplicated fields, and one-use abstractions require a tested invariant or must be
removed. Detailed research logic belongs in evaluation tools and design documents
until measured evidence licenses a production seam.

## Research basis

The ambiguity taxonomy draws on ambiguity sets [Min 2020], staged clarification
[Lee 2023], interactive semantic parsing [Yao 2019], conversational ambiguity and
unanswerability [Dong 2025; Sarwar 2026], targeted-question usefulness [Rahmani
2024], and NIL separation [Zhu 2023]. The feedback policy draws on semantic-parse
correction [Elgohary 2020], learning from user feedback [Iyer 2017], and the bias
of implicit interaction signals [Joachims 2007]. Full records appear in the
bibliography sections on [query ambiguity](00-bibliography.md#query-ambiguity-and-clarification)
and [user feedback](00-bibliography.md#user-feedback-and-adaptation).
