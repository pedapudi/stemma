---
name: stemmadb-eval
description: Run and extend stemma's evaluation harness — BIRD no-evidence target derivation, corpus metrics, and the measurement philosophy. Use when asked to evaluate resolution quality, run BIRD, derive targets, or add metrics.
---

# Evaluating stemma

## The protocol and why it's shaped this way

stemma evaluates on **BIRD dev in the no-evidence setting**. BIRD ships
human-written "evidence" hints that pre-solve entity/value linking — exactly
stemma's job — and SOTA text-to-SQL systems lose >10% execution accuracy
without them. So the question stemma measures is: *how much of the human
evidence can we reconstruct automatically?*

Ground truth is **derived from gold SQL**, never hand-labeled:
- **value targets** — every `column op literal` predicate (`=`, `!=`, `<`,
  `<=`, `>`, `>=`, `LIKE`, `IN`) in the gold SQL. String literals are the
  primary value-linking subset; numeric literals are tracked but need
  type-aware handling later.
- **schema targets** — every table the gold SQL references.
- BIRD's evidence field is carried along as the reference for
  evidence-reconstruction scoring, and must **never** be shown to the
  resolver.

Metrics are **recall-weighted** (F-beta with β>1): a missed record is
unrecoverable downstream, an extra candidate is noise. Report value-linking
recall directly — never end-to-end SQL execution accuracy, which measures a
different system.

The full protocol with citations is docs/design/06-evaluation.md; the
harness design (query tiers, mechanism ablations, the mechanism × tier
matrix, statistical grading) is docs/design/07-eval-harness.md — read both
before extending metrics, and keep new metrics consistent with their
definitions and with the shared bibliography (docs/design/00-bibliography.md).

## Running

```sh
# One-time, ~1-2 GB:
eval/bird/fetch_bird.sh

# Derive targets (all DBs, or slice with repeatable --db-id):
bazel run //crates/stemma-eval -- derive \
  --questions eval/bird/data/dev/dev.json \
  --out /tmp/targets.json \
  --db-id california_schools --db-id thrombosis_prediction
```

Output: per-question JSON (`tables`, `value_targets`, `evidence`,
`parse_error`) plus summary stats — question count, parse failures, and the
size of the string-value subset. Gold SQL that fails to parse is kept with
`parse_error` set, not dropped silently.

## Interpreting derive output

- `value_targets[].is_string == true` → the value-linking subset stemma
  milestone 2 targets first.
- `parse_error` non-null → sqlparser couldn't handle the gold SQL dialect;
  a few percent is normal, a large fraction means a harness bug — investigate
  before trusting numbers.
- Join-key equalities (`a.id = b.a_id`) are correctly excluded from value
  targets (column-to-column, not column-to-literal); tested in
  `crates/stemma-eval/src/main.rs`.

## Extending the harness

The harness lives in `crates/stemma-eval` (single `main.rs` today; split into
modules when adding a second subcommand).

**Adding a scoring subcommand** (the milestone-2 task): compare resolver
output (gRPC Resolve responses) against derived targets. A value target
`(column, '=', 'Seattle')` is *hit* if any returned candidate resolves to a
row where that column holds that literal — probe the BIRD SQLite DB directly
to check (`SELECT count(*) FROM t WHERE col = ?`). Report per-question and
corpus-level recall/precision/F-beta, sliced by string/numeric and by db_id.

**Adding target types**: extend `collect_value_targets` (e.g. `BETWEEN`,
date functions). Every extension needs a unit test with a hand-written SQL
snippet — the two existing tests are the pattern.

**Adding an eval corpus**: BIRD-format is just `[{question_id, db_id,
question, evidence, SQL}]` + a directory of SQLite DBs; anything mapped into
that shape (e.g. hand-authored questions over the careg corpus) reuses the
whole harness unchanged. Put fetch/prep under `eval/<name>/`, gitignore the
data, commit the scripts.

## Milestone acceptance gates (from the plan)

- M2: BM25+fuzzy value-linking recall on the BIRD slice; <50 ms CPU lexical
  path; "the Seattle office" resolves on the mini corpus with evidence.
- M3: measured recall lift of the dense channel over lexical-only.
- M4: associative mentions ("Chen's team") resolve via KG paths; evidence
  includes the path.
- M5: evidence-reconstruction delta with the LM band on, plus LM routing rate
  (most queries must not touch the LM).
