# Evaluation datasets

Frozen, versioned question sets consumed by the eval harness
([docs/design/07-eval-harness.md](../../docs/design/07-eval-harness.md)).

| File | Corpus | Contents |
|---|---|---|
| `legal-synth-v1.jsonl` | legal (`eval/legal/data/legal.db`) | synthetic questions, tiers L1/L2/L4 + NIL |
| `mini-l3-v1.jsonl` | mini (`eval/mini/data/mini.db`) | relational (L3) questions over declared joins |
| `legal-synth-v1-review.html` | — | the human-skim page for the review pass |

## Format

One JSON object per line. The **first line is a header record** — consumers
must skip any line with `"type": "header"` (equivalently: any line without a
`"question"` key). The header carries the dataset name, version, seed,
per-tier counts, generation stats, and the L3 slot note.

Question records:

```json
{
  "question": "...",
  "tier": "L1" | "L2" | "L3" | "L4" | "NIL",
  "corpus": "legal" | "mini",
  "targets": [
    {"table": "...", "column": "...", "rowids": [1, 2],
     "literal": "optional", "match_mode": "doc" | "value"}
  ],
  "nil": false,
  "provenance": {
    "generator": "eval/legal/gen_eval_set.py v1",
    "seed": 11,
    "attempts": {"lm_calls": 2, "candidates": 5},
    "verification": { ...the mechanical tier-check evidence... }
  }
}
```

- `corpus` names the config-file database (`databases` section) the targets
  resolve against.
- `match_mode: "doc"` — the gold row is a document (`is_doc` containment
  semantics); `"value"` — exact stored-value semantics, `literal` carries the
  stored value.
- L1 records carry `literal` = the verified lexical anchor phrase.
- L4 records carry two targets (one `regulations` row, one `sections` row);
  `provenance.verification.mode` records `no_anchor` or which side is
  anchored (`mixed:...`).
- NIL records have empty `targets` and `nil: true`; the verification block
  records the defining phrase, its corpus-wide hit counts (must be zero on
  both the exact-phrase and trigram channels), and the absence argument.

The legal set has **no L3 records yet**: the legal corpus has no join tables
(citation mining is in flight on a sibling branch). The header's `l3` field
documents the empty slot; `mini-l3-v1.jsonl` covers the relational tier
against the mini corpus until legal citation edges land.

## Ground truth is derived, tiers are verified

Questions are reverse-generated (sample a record, then ask the LM for an
oblique question about it), so the gold rowid is known **by construction**.
Tier membership is **verified mechanically** against the store's lexical
index (`legal.stemmadb`), never trusted from generation:

- **L1** — at least one content phrase of the question exact/trigram-hits the
  gold row, and the longest verbatim run is bounded (paraphrase enforced).
- **L2** — no content token of the question (resolver stopword list removed)
  produces an exact or trigram hit on any of the gold row's lex entries;
  colloquial register enforced by a banned-token list.
- **L4** — both gold rows pass the L2-style no-anchor check, or the pair is
  explicitly mixed (recorded which side is anchored). Never both anchored.
- **L3** — the gold tuple is derived by executing the recorded join path
  against the corpus (a pure function of the database).
- **NIL** — the defining topic phrase has zero exact-phrase and zero trigram
  hits corpus-wide, plus a recorded domain-level absence argument.

Every record's `provenance.verification` contains the evidence for its own
tier membership, so the checks are re-runnable and auditable.

## Freeze discipline

Per 07: **these sets are frozen after the human review pass.** Silent
regeneration is how an eval set drifts toward what the current system finds
easy — the labels can never be allowed to chase system output.

- A dataset file is immutable once merged. Fixes ship as a **new version**
  (`legal-synth-v2.jsonl`), never as an in-place edit.
- Regenerating a set is a **reviewed change**: the regeneration command
  (generator revision, config, `--seed`, counts) goes in the commit message,
  the new review HTML is part of the change, and the diff is reviewed like
  code.
- Baselines reference datasets by name+version; a version bump invalidates
  accepted baselines for the affected tiers, deliberately.
- The review page (`*-review.html`) is the artifact of the human skim that
  gates the freeze: every question was read against its gold snippet before
  the set was accepted.

## Regenerating

```sh
python3 eval/legal/gen_eval_set.py \
    --config config.json \
    --n-per-tier 50 --n-nil 25 --seed 11 \
    --out eval/datasets/legal-synth-v1.jsonl
```

The LM endpoint is read from `eval.lm` in the config file, falling back to
`console.lm`. Configuration is config-file + flags only; the generator reads
no environment variables. Corpus databases are opened strictly read-only.

Sampling is deterministic under `--seed` (stratified across CCR titles and
CFR title/parts); LM output is sampled at temperature 0.8, so regeneration
produces a *comparable* set, not a byte-identical one — which is exactly why
regeneration is a reviewed change.
