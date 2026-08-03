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

# Raw target stats (all DBs, or slice with repeatable --db-id):
bazel run //crates/stemma-eval -- derive \
  --questions eval/bird/data/dev_20240627/dev.json \
  --out /tmp/targets.json \
  --db-id california_schools --db-id thrombosis_prediction

# Denotation-verified datasets (one JSONL per corpus, checked in):
bazel run //crates/stemma-eval -- dataset \
  --questions eval/bird/data/dev_20240627/dev.json \
  --db-root eval/bird/data/dev_20240627/dev_databases \
  --out-dir eval/datasets \
  --db-id california_schools

# The mechanism-ablation sweep (writes run JSON + self-contained HTML
# report; grades against eval/baseline/<corpus>.json when one exists):
bazel run //crates/stemma-eval -- run \
  --config config.json \
  --dataset eval/datasets/bird-california_schools.jsonl \
  --ablation lex --ablation +dense --ablation +kg --ablation +adj

# Grade an existing run (exit 1 + named failures on regression):
bazel run //crates/stemma-eval -- grade \
  --run eval/runs/<run-id>.json --baseline eval/baseline/<corpus>.json

# Accept a run as the new baseline (a reviewed change — commit the diff):
bazel run //crates/stemma-eval -- accept \
  --run eval/runs/<run-id>.json --out eval/baseline/<corpus>.json
```

`derive` output: per-question JSON (`tables`, `value_targets`, `evidence`,
`parse_error`) plus summary stats — question count, parse failures, and the
size of the string-value subset. Gold SQL that fails to parse is kept with
`parse_error` set, not dropped silently.

`dataset` additionally denotation-verifies every string target against the
database instance (the gold predicate must select rows; failures are
counted, never silently dropped), records the gold `(table, column, rowid
set)` per target, and assigns tiers mechanically (L1/L3/L4 for BIRD; L2 and
NIL come from the synthetic legal set, same JSONL shape). Ablations are
honest flag combinations of the shipping pipeline — `+coh` is folded into
`+kg` because collective disambiguation is not separately gateable; see the
status block of docs/design/07-eval-harness.md for all deviations.

The report template lives in `eval/report/` (deno-built, no npm;
`eval/report/build.sh` regenerates the checked-in `dist/template.html`
which the binary embeds). Rebuild it and re-run `cargo build` after
touching `report.ts`/`report.css`/`template.src.html`.

## Interpreting derive output

- `value_targets[].is_string == true` → the value-linking subset stemma
  milestone 2 targets first.
- `parse_error` non-null → sqlparser couldn't handle the gold SQL dialect;
  a few percent is normal, a large fraction means a harness bug — investigate
  before trusting numbers.
- Join-key equalities (`a.id = b.a_id`) are correctly excluded from value
  targets (column-to-column, not column-to-literal); tested in
  `crates/stemma-eval/src/derive.rs`.

## Extending the harness

The harness lives in `crates/stemma-eval`, split by concern:
`derive.rs` (SQL parsing + denotation verification + tiering),
`dataset.rs` (the JSONL format and its tolerant loader), `metrics.rs`
(per-query scoring, cell aggregation, calibration), `stats.rs` (paired
randomization, bootstrap CIs, randomised Tukey HSD — seeded,
deterministic), `runner.rs` (store prep, metered backend seams, run
files, baselines), `grade.rs` (the four implemented grading rules),
`report.rs` (template injection).

**Adding a metric**: compute it in `metrics::score_query` /
`metrics::aggregate`, thread it through `runner::CellReport`, and render
it in `eval/report/report.ts`. Keep definitions consistent with
docs/design/06-evaluation.md and 07-eval-harness.md.

**Adding an ablation column** (e.g. cross-encoder reranking when built):
add the flag combination in `runner::ablation` — it must be a real flag
of the shipping pipeline, never an eval-only code path — and its target
tiers in `runner::target_tiers` so containment grading knows what it is
allowed to move.

**Adding target types**: extend `collect_value_targets` (e.g. `BETWEEN`,
date functions). Every extension needs a unit test with a hand-written SQL
snippet — the existing tests are the pattern.

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
