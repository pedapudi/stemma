# Evaluation

> The runnable harness built on this protocol — tiers, ablations, grading —
> is designed in [07-eval-harness.md](07-eval-harness.md).

stemma's output is a resolution artifact, not an answer, so it cannot be
scored by execution accuracy. This document specifies the evaluation
protocol: why the no-evidence setting is the only honest one, how ground
truth is derived from gold SQL instead of hand-labelled, why the metrics are
recall-weighted, what the corpora are for, and what each milestone gate has
to demonstrate.

Source: [`crates/stemma-eval/src/main.rs`](../../crates/stemma-eval/src/main.rs),
[`eval/`](../../eval).

## The no-evidence protocol

BIRD [J. Li 2023] ships, alongside each question and its gold SQL, a
human-written `evidence` string: a hint that names the column, spells out the
domain abbreviation, or gives the stored value the question refers to
obliquely. For example, a question about "eligible free rate" comes with
evidence defining it as `Free Meal Count / Enrollment`.

That evidence *is* the linking artifact. It is exactly what stemma is built
to produce, handed to the system for free.

Leaderboard numbers are therefore conditional on a pre-solved linking step,
and the conditional is expensive. Removing the hints costs CodeS-7B 11.93
points of execution accuracy (57.17 → 45.24), CodeS-3B 11.60 and CodeS-1B
12.00 [Nan 2026]; a second measurement puts the range at 8.35 to 20.86 points
across systems, with RSL-SQL/GPT-4o falling 65.78 → 54.50 and DAIL-SQL/GPT-4
falling 56.32 → 35.46 [Yun 2025]. Only 5 of 52 BIRD leaderboard methods report
no-evidence numbers at all [Nan 2026] — which is itself the argument for
making the no-evidence setting the default rather than the ablation.

[Yun 2025] also documents that BIRD's human evidence contains missing and
erroneous entries, and that automatically generated evidence sometimes
outperforms it. That matters for protocol design: **BIRD's human evidence is a
reference, not an oracle**, and the evidence-reconstruction metric below is
scored against it accordingly.

stemma's protocol is the setting that measures the thing:

1. **The resolver never sees `evidence`.** It sees only `question` and the
   database.
2. **Ground truth is derived from the gold SQL**, which is the ground truth
   the benchmark already ships and which nobody has to re-label.
3. **The human `evidence` string is retained as a reference**, used only for
   an evidence-reconstruction score — how much of what a human wrote by hand
   did the system recover automatically.

The derived-targets file carries `evidence` on every record with an explicit
comment that it is never shown to the resolver:

```rust
/// BIRD's human evidence, kept only as reference for the
/// evidence-reconstruction metric — never shown to the resolver.
evidence: String,
```

The protocol's virtue is that it needs no annotation budget and cannot drift
from the benchmark: derivation is a pure function of the gold SQL, so it is
reproducible, auditable, and updates automatically when BIRD does.

## Target derivation

```sh
bazel run //crates/stemma-eval -- derive \
  --questions eval/bird/data/dev/dev.json \
  --out /tmp/targets.json \
  [--db-id <name>]...
```

`derive` parses every gold SQL with `sqlparser` under the `SQLiteDialect` and
extracts two kinds of target.

### Schema targets

```rust
visit_relations(stmt, |rel| {
    tables.insert(strip_quotes(&rel.0.last().to_string()));
})
```

Every table the gold SQL references, unqualified and unquoted. A resolver
that returns candidates in tables the gold query never touches is linking to
the wrong part of the schema, and a resolver that misses a referenced table
has removed a join the query needs.

### Value targets

```rust
struct ValueTarget { column: String, op: String, literal: String, is_string: bool }
```

Collected by walking every expression in the statement:

| Pattern | Op recorded |
|---|---|
| `col = lit`, `lit = col` | `=` |
| `col != lit`, `col > lit`, `col >= lit`, `col < lit`, `col <= lit` | the operator |
| `col LIKE pat`, `col ILIKE pat` | `like` (always `is_string`) |
| `col IN (a, b, c)` | `in`, one target per list item |

Literals are `SingleQuotedString` / `DoubleQuotedString` (`is_string = true`)
or `Number` (`is_string = false`). Both operand orders are tried, so
`'Seattle' = T1.city` is captured as well as `T1.city = 'Seattle'`.

**Join keys are excluded by construction, not by a heuristic.** In
`a.id = b.a_id` both sides are identifiers, so `literal()` returns `None` for
each and no target is emitted. The regression test asserts exactly this:

```rust
derive_from_sql("SELECT * FROM a JOIN b ON a.id = b.a_id WHERE a.x = 'y'")
// → exactly one value target, on column "x"
```

That property is why the extractor is written as *column-versus-literal*
rather than as a WHERE-clause walk. A WHERE walk would have to special-case
join conditions; this formulation never sees them.

**Targets are collected statement-wide**, not only from `WHERE`. A literal
comparison inside `HAVING`, a `CASE`, or a correlated subquery is still a
value link the resolver would have to make, so capturing it is correct — but
note that the crate's doc comment says "WHERE-class clauses", which describes
the common case rather than the implementation.

### What `derive` reports

```
questions:                <n>
gold-SQL parse failures:  <n>
total value targets:      <n>
questions with >=1 string value target (value-linking subset): <n> (<pct>%)
```

The last line defines **the value-linking subset**: questions whose gold SQL
compares a column against a string literal. That is the slice stemma targets
first, because a string literal in a predicate is precisely a stored value
that the question named obliquely. Numeric and date literals need type-aware
handling — *"in 2023"* linking to a date column is a different problem from
*"the Seattle office"* linking to a stored name — and are derived but not yet
targeted.

Questions whose gold SQL fails to parse are kept with a `parse_error` rather
than dropped, so the denominator is always the full question set and coverage
loss is visible instead of silent.

### Limitations of derived targets

Stated plainly, because a derived ground truth has failure modes a
hand-labelled one does not:

- **Column names lose their table qualification.** `column_name()` takes the
  *last* part of a compound identifier, so `T1.city` becomes `city`. When two
  tables in the same database both have a `city` column, a value target does
  not say which was meant. Scoring must either accept either table or resolve
  the alias chain, which the current derivation does not do.
- **`LIKE` patterns keep their wildcards.** The target literal for
  `name LIKE '%Chen%'` is `%Chen%`, not `Chen`. Scoring must strip wildcards
  before comparing against a resolved value.
- **Gold SQL is one correct query, not the only one.** A resolver that links
  to a legitimately different-but-equivalent record is scored wrong. Derived
  targets are a lower bound on correctness.
- **Gold SQL sometimes encodes the linking rather than the intent.** Where
  the gold query hard-codes a value the question only implies, the target is
  right; where it works around a data quirk, the target may be an artefact.

None of these is a reason to hand-label instead. They are reasons to read a
score as a comparable number across systems and versions rather than as
absolute truth.

## Metrics

**Everything is recall-weighted.** The asymmetry is structural, not a tuning
preference: a record stemma fails to surface is *unrecoverable downstream* —
no query generator can join to a table it was never told about — while an
extra candidate is *noise that the next stage filters*. The output is a
candidate set for exactly this reason. Precision-first metrics would reward
the wrong behaviour: premature commitment, which is the failure mode the
whole design is built to avoid.

### Proposed metric set

*Designed; the scoring subcommand is not built.*

**Value-linking recall@k.** Over the value-linking subset: the fraction of
string value targets for which some candidate within the top *k* of some
mention points at the target value. "Points at" means the candidate's stored
value equals the literal after normalization, or — for `is_doc` candidates —
the document contains it. Reported at k = 1, 5, and *unbounded* (the full
traced candidate set), because the three numbers separate three different
failures:

| k=1 | k=5 | unbounded | Diagnosis |
|---|---|---|---|
| ✗ | ✓ | ✓ | Ranking problem |
| ✗ | ✗ | ✓ | Threshold or `TOP_K` problem |
| ✗ | ✗ | ✗ | Retrieval problem — a channel is missing the record |

This is the practical payoff of tracing near-misses: a system that returns
only its answer cannot distinguish these, and they have completely different
fixes.

**Schema-linking recall.** The fraction of gold-referenced tables covered by
the tables of the selected candidates. Reported per question and
macro-averaged.

**Candidate-set cost.** Mean selected candidates per mention and mean
mentions per query — the noise the downstream consumer pays for the recall.
Reported alongside recall, never traded against it in a single number.

**Recall-weighted F.** Where one number is needed, F<sub>β</sub> with β = 2,
which weights recall four times precision. Stating β explicitly is the point;
an unqualified "F1" would silently encode a 1:1 trade this system rejects.

**Evidence reconstruction.** Token-level overlap between the resolution's
evidence (matched values, cited columns, KG paths) and BIRD's human
`evidence` string. Not a headline metric — the human strings are prose and
overlap is a crude proxy — but it directly answers "how much of the hint did
we recover", which is the question the no-evidence protocol exists to ask.

**Latency.** Median and p95 per query, reported with corpus size. The
measured baseline today is 60–95 ms per non-skipped span on a
92,696-document corpus; sub-second on small corpora, seconds on large ones.
See [03-resolution.md](03-resolution.md#complexity).

### What is not measured

Execution accuracy. stemma does not generate SQL, and scoring it by the
downstream success of a generator it does not control would measure the
generator. The relationship to execution accuracy is the *argument* for the
work, established by the published error analyses; it is not stemma's metric.

## Corpora

Three, in increasing size, each for a different job.

### Mini — correctness

[`eval/testdata/mini.sql`](../../eval/testdata/mini.sql): six tables
(`offices`, `people`, `teams`, `team_members`, `reports`, `shipments`) with
six declared foreign keys, hand-built so that every mention class from the
README has a target — nickname (*the Seattle office* → `'Seattle -
Northgate'`), abbreviation, description, and association (*Chen's team*
needs the `people → teams` hop; *the crown's holdings* needs `Crown Building`
and `Holdings Research`).

It is the golden-test corpus, loaded directly by
`stemma-resolve`'s and `stemma-kg`'s unit tests via `include_str!`. Five
resolution tests assert specific behaviours on it: exact city match scores
≥ 0.9; `Wei Chen` wins its byte range while the losing `Chen` span keeps the
rival Dana Chen as a visible near-miss; `Northgate` is found through the
trigram channel; proto conversion preserves byte offsets and evidence;
Explain preserves rejected candidates.

Correctness lives here because assertions on a five-row corpus are exact and
fast. Nothing about scale is learned from it.

### Legal — scale and document-shaped data

[`eval/legal/build_legal_db.py`](../../eval/legal/build_legal_db.py) merges
two Nemotron legal subsets into **one** user database with two tables:

| Table | Source | Rows | Avg text |
|---|---|---:|---:|
| `regulations` | California Code of Regulations | 57,523 | 2,660 chars |
| `sections` | eCFR (federal) | 35,173 | 16,151 chars |

789 MB user database; the compiled store is 4.2 GB. Both tables share the
schema `(id INTEGER PRIMARY KEY, uuid TEXT UNIQUE, text, license, category)`.

**One database, two tables, deliberately.** The lexical index, the knowledge
graph and resolution all span tables, so state and federal regulation resolve
side by side and a query can land in either. It is also the smallest
realistic multi-table document corpus, which is what exposed the corpus-wide
`fts5vocab` document-frequency limitation described in
[04-knowledge-graph.md](04-knowledge-graph.md#step-1--candidate-shortlist-a-df-ceiling-plus-burstiness).

The corpus's job is everything the mini corpus cannot test: document-shaped
values (the `is_doc` branch and the careg failure mode of
[03-resolution.md](03-resolution.md#why-documents-need-their-own-branch-the-careg-failure-mode)),
term and phrase mining at real vocabulary size, index build time, and
resolution latency at scale. It has **no labelled targets** — it is a
robustness and performance corpus, not an accuracy one.

**`uuid` is preserved on purpose.** Pre-computed 1024-dimension embeddings
for exactly these rows exist, keyed by uuid — verified at 57,523 / 57,523,
100% coverage of the `regulations` table — so the dense channel can be loaded
without an embedding run and multiple encoder checkpoints can be A/B-compared
through the model registry.
[`eval/legal/load_vectors.py`](../../eval/legal/load_vectors.py) is the
loader; see
[05-encoders-decoders.md](05-encoders-decoders.md#the-integration). The
single-corpus builders
[`eval/careg/build_careg_db.py`](../../eval/careg/build_careg_db.py) and
[`eval/ecfr/build_ecfr_db.py`](../../eval/ecfr/build_ecfr_db.py) produce the
same schema for one source each.

### BIRD — the benchmark

[`eval/bird/fetch_bird.sh`](../../eval/bird/fetch_bird.sh) downloads and
unpacks the dev set (~1–2 GB). BIRD's databases *are* SQLite, so stemma
consumes them with no conversion — the same `--db name=path` registration as
any other corpus. This is not a coincidence of convenience; it is why BIRD
was chosen over benchmarks that would need a loader, since a loader is a
place for the evaluation to diverge from production behaviour.

KaggleDBQA [C.-H. Lee 2021] is the designated later stress test, for
abbreviation-heavy schemas where column names are opaque and the linking
problem is at its hardest.

## Corpus construction guidelines

The rules in [`docs/user-guide/04-corpora.md`](../user-guide/04-corpora.md)
are evaluation rules as much as usability ones, and the reasoning is worth
stating here:

- **Ship a stock SQLite file, with no stemma-specific tables.** Derived state
  belongs in the sidecar. A corpus that arrives pre-indexed measures the
  indexer's absence.
- **Keep stable identifiers.** Resolutions point at rowids; external
  artefacts (pre-computed embeddings) join on uuids.
- **Declare foreign keys.** Undeclared relationships cost associative-mention
  resolution until inclusion mining recovers them — and inclusion mining is
  measurably good but not free, so a corpus that declares its FKs measures
  resolution rather than measuring join discovery.
- **One text column per concept.** Indexes are built per column and evidence
  cites `(table, column, value)`; a concatenated blob destroys the citation.
- **Do not pre-normalize.** Keeping `'Seattle - Northgate'` as stored *is*
  the task. Normalizing surface forms away deletes the evidence trail and
  makes the benchmark easier than the world.

## Milestone gates

What each milestone has to demonstrate. Gates 1–2 and the knowledge-graph
work are met; the rest are the plan.

**Gate 1 — storage.** sqlite-vec statically linked and reporting a version;
FTS5 present; user databases attached read-only with a test proving writes
fail; store schema version-stamped. *Met.*

**Gate 2 — lexical resolution.** The README case resolves end to end: *"the
Q3 numbers for the Seattle office"* produces a `Seattle` mention whose top
candidate is `offices` rowid 17 with `LexicalMatch` evidence, scoring ≥ 0.9.
Overlapping spans keep their near-misses with reject reasons. Document
corpora produce mentions with marked snippets and the topically correct
document ranked first. *Met — five tests in
[`crates/stemma-resolve/src/lib.rs`](../../crates/stemma-resolve/src/lib.rs).*

**Gate — knowledge graph.** Schema and profile layers compile; every edge
carries provenance; corpus stopwords are excluded by the DF ceiling while
topical terms survive; every term node carries a TextRank score and every
node a centrality; undeclared joins are discovered with confidence;
incremental recompilation touches only dirty tables and converges to the same
graph as a full recompile. *Met — four tests in
[`crates/stemma-kg/src/lib.rs`](../../crates/stemma-kg/src/lib.rs).*

**Gate 3 — dense channel.** *Partially met.* A registered vector table with a
model-registry row exists, and the channel contributes candidates
([03-resolution.md](03-resolution.md#stage-4b--the-dense-channel-targeted)).
Still outstanding, and all of it measurable: the embed queue drains for a
corpus with no pre-computed vectors; the dense channel demonstrably
contributes candidates the lexical channels miss, shown as a
recall@unbounded improvement on questions with no lexical overlap between
mention and stored value; the fusion constants are re-derived for four
channels and the score bands re-verified; a re-embed completes with no window
in which two vector spaces are queried together *and* no window in which
there is no index at all.

The careg corpus makes the first of those unusually cheap to run: four
encoder generations over the same 57,523 rowids, each at 100% uuid coverage
([05-encoders-decoders.md](05-encoders-decoders.md#the-integration)), so a
dense-recall comparison across checkpoints needs staging and restarting, not
embedding. It also comes with a measured geometric baseline
([05-encoders-decoders.md](05-encoders-decoders.md#the-legal-corpus-measured)),
so a dense-channel change can be reported as *both* a retrieval delta and a
crowding delta.

**Gate 4 — collective disambiguation.** A query with two interdependent
mentions (*Chen's team*) resolves the correct pair where independent scoring
does not, with a `KgPath` evidence message naming the connecting path. Joint
scoring stays within a latency budget that is a small fraction of candidate
generation.

**Gate 5 — LM band.** Mention expansion measurably improves recall on the
oblique-mention subset; constrained adjudication improves precision on the
ambiguous band without reducing recall; explicit NIL is returned rather than
a forced choice when no candidate is right. Every LM decision carries an
`Adjudication` evidence message with the model identity.

**Gate — scoring harness.** `stemma-eval` gains the subcommand that runs a
resolver over derived targets and reports the metric set above. Until this
exists, every claim in this document about stemma's accuracy is unmeasured —
which is why no such claim appears anywhere in this document set.

## References

- [C.-H. Lee 2021] Chia-Hsuan Lee, Oleksandr Polozov, Matthew Richardson.
  "KaggleDBQA: Realistic Evaluation of Text-to-SQL Parsers." ACL-IJCNLP 2021.
- [Lei 2025] Fangyu Lei et al. "Spider 2.0: Evaluating Language Models
  on Real-World Enterprise Text-to-SQL Workflows." ICLR 2025 (Oral).
- [J. Li 2023] Jinyang Li et al. "Can LLM Already Serve as a Database
  Interface? A Big Bench for Large-Scale Database Grounded Text-to-SQLs."
  NeurIPS 2023 (BIRD).
- [Nan 2026] Yafeng Nan et al. "DIVER: A Robust Text-to-SQL System
  with Dynamic Interactive Value Linking and Evidence Reasoning."
  arXiv:2602.12064.
- [Yun 2025] Janghyeon Yun, Sang-goo Lee. "SEED: Enhancing Text-to-SQL
  Performance and Practical Usability Through Automatic Evidence Generation."
  IEEE ICDEW 2025. arXiv:2506.07423.

Full bibliography: [00-bibliography.md](00-bibliography.md).
