# Explicit grounding feedback

Stemma records deliberate judgments about an exact resolution or parsing
episode. Feedback lives in the `.stemmadb` sidecar. The attached user database
remains read-only.

## Episode identity

A non-empty Resolve, Explain, or Parse request returns `episode_id` when the
server records the episode successfully. The server stores the candidate
presentation order, clarification options, active indexed-corpus revision,
active vector-registry revision, and parse output. An empty identifier means
that resolution succeeded while episode persistence was unavailable. Feedback
cannot be submitted for that response.

```python
from stemmadb import StemmaClient

client = StemmaClient("127.0.0.1:50051")
result = client.resolve(
    "show work by Jordan",
    database="catalog",
    source="console",
    session="review-42",
)
print(result.episode_id)
```

## Categories

| Category | Meaning |
|---|---|
| `approved` | The displayed grounding or result is acceptable. |
| `rejected` | The result is wrong and no narrower cause is stated. |
| `wrong_meaning` | A span resolved to the wrong database meaning. |
| `missing_interpretation` | The intended meaning was absent from the choices. |
| `wrong_query_operation` | A parse used the wrong projection, filter, aggregation, ordering, or other operation. |
| `wrong_rows` | The parsed query returned the wrong records. |

`wrong_query_operation` and `wrong_rows` require a successful Parse episode. A
missing interpretation requires correction text.

## Approve or reject the whole episode

```python
event = client.submit_feedback(
    "catalog",
    result.episode_id,
    "approved",
    scope="database",
)
print(event.id)
```

The console places approval and rejection controls directly below the visual
resolution trajectory. The control submits the same typed event.

## Select a candidate

Candidate indices refer to the order in the recorded Explain trajectory. A
candidate target also requires its span identifier.

```python
trace = client.explain(
    "show work by Jordan",
    database="catalog",
    source="console",
    session="review-42",
)
span = next(item for item in trace.spans if len(item.candidates) > 1)
event = client.submit_feedback(
    "catalog",
    trace.episode_id,
    "wrong_meaning",
    scope="session",
    span_id=span.id,
    candidate_index=1,
    correction="the author record",
)
```

The server rejects a span or candidate that was absent from the recorded
episode. Candidate order is preserved even if a later resolution ranks the same
records differently.

## Answer a clarification

Clarification option indices refer to the recorded option order.

```python
trace = client.explain(
    "show work by Jordan",
    database="catalog",
    source="console",
    session="review-42",
)
question = trace.clarification
event = client.submit_feedback(
    "catalog",
    trace.episode_id,
    "approved",
    scope="session",
    span_id=question.span_id,
    clarification_option=0,
)
```

Feedback records the answer. The current resolver does not yet apply that answer
to continue a multi-turn parse.

## Scope

`session` associates the judgment with the `ResolveOptions.session` value from
the episode. The server rejects session scope when the original request had no
session. `database` makes the event eligible for future database-wide review.

Scope is provenance metadata in the current implementation. No runtime
preference or model update consumes it.

## Revision checks

Submission succeeds only while the episode's indexed-corpus and vector-registry
revisions remain active. This prevents a judgment about one candidate universe
from attaching silently to another. An expired episode remains in history for
inspection, while new feedback receives a failed-precondition response.

The revision check does not include a knowledge-graph recompile against
unchanged corpus receipts. Any later export that depends on graph roles or paths
must verify that evidence again.

## Inspection and deletion

```python
events = client.list_feedback("catalog", episode_id=result.episode_id)
for item in events.feedback:
    print(item.id, item.category, item.recorded_at)

deleted = client.delete_feedback("catalog", event.id)
```

Deletion is permanent. Deleting a query-history row inside the sidecar also
deletes its feedback through a foreign-key cascade. No automatic retention limit
is configured. Operators should set a retention policy appropriate for query and
correction text, which may contain sensitive information.

## Learning boundary

The event log provides evidence but does not constitute a training set. Silence,
abandonment, and the resolver's own choice create no feedback. A small,
unrepresentative gold SQL set can protect known cases during review, but cannot
calibrate usage-derived labels. Export to regression fixtures, preferences, or
representation training requires separate review and provenance rules described
in [Usage-guided grounding and representation
adaptation](../design/09-usage-guided-learning.md).
