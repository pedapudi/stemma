# Corpora

stemma consumes any SQLite database as-is. The repo ships three
corpus setups, in increasing size.

## 1. Mini corpus (bundled, seconds)

[`eval/testdata/mini.sql`](../../eval/testdata/mini.sql) is a hand-built
office/people/teams/reports corpus designed so every mention class from the
README has a target: nickname, abbreviation, description, and association
(e.g. resolving *"Chen's team"* requires the `people → teams` foreign-key
hop; *"the crown's holdings"* needs the alias-ish `Crown Building` and the
`Holdings Research` team).

```sh
python3 - <<'EOF'
import sqlite3
conn = sqlite3.connect('mini.db')
conn.executescript(open('eval/testdata/mini.sql').read())
conn.close()
EOF
bazel run //crates/stemma-server -- --db mini=mini.db
```

## 2. California Code of Regulations (~200 MB, minutes)

57,523 sections of California regulatory text from the
Nemotron-Pretraining-Legal-v1 corpus — real-world scale for lexical and dense
retrieval, with genuinely oblique mention targets ("the Coastal Commission",
"Title 14 permits").

```sh
# 1. Obtain the source parquet (one file, ~40 MB):
#    Nemotron-Pretraining-Legal-California-Code-Of-Regulations/part_000000.parquet
#    into eval/careg/data/src/
# 2. Build the DB (requires python3 + pyarrow):
python3 eval/careg/build_careg_db.py
# -> eval/careg/data/careg.db: 57523 regulations, avg text 2660 chars, 189 MB
bazel run //crates/stemma-server -- --db careg=eval/careg/data/careg.db
```

Schema: `regulations(id, uuid, text, license, category)`. The `uuid` column
is preserved deliberately: pre-computed 1024-dim embeddings for exactly these
rows exist (keyed by uuid), so the dense channel can be loaded without
re-embedding, and multiple embedder checkpoints can be A/B-compared through
the model registry.

## 3. BIRD dev set (~2 GB, the evaluation benchmark)

BIRD databases are SQLite already; stemma consumes them directly and is
evaluated in the **no-evidence** setting (see
[architecture.md](../architecture.md#evaluation)).

```sh
eval/bird/fetch_bird.sh                       # downloads + unpacks
bazel run //crates/stemma-eval -- derive \
  --questions eval/bird/data/dev/dev.json \
  --out /tmp/targets.json
```

`derive` parses every gold SQL, extracting the tables it references and the
`column op literal` predicates — the ground truth a resolver must reconstruct.
Use `--db-id <name>` (repeatable) to restrict to a slice.

## Building your own corpus

Any SQLite database works. Guidelines that make resolution better and keep
the system honest:

1. **Ship a stock SQLite file.** No stemma-specific tables, no
   pre-processing — derived state belongs in the `.stemmadb` sidecar, which
   stemma builds itself.
2. **Keep stable identifiers.** `INTEGER PRIMARY KEY` rowids are what
   resolutions point at; if your source data has external IDs (uuids), keep
   them as a column so external artifacts (like pre-computed embeddings) can
   join back.
3. **Declare foreign keys.** The knowledge store compiles the FK graph into
   join paths; undeclared relationships cost you associative-mention
   resolution ("Chen's team") until declared metadata fills the gap.
4. **Prefer one text column per concept** (a `name`, a `title`, a `body`)
   over concatenated blobs — the indexes are built per column and evidence
   cites `(table, column, value)`.
5. **Don't pre-normalize.** Keep `'Seattle - Northgate'` as stored; mapping
   "the Seattle office" onto it is stemma's job, and normalizing away surface
   forms destroys the evidence trail.

A worked example of converting parquet source data is
[`eval/careg/build_careg_db.py`](../../eval/careg/build_careg_db.py); the
`stemmadb-corpus` skill in [`skills/`](../../skills) is a step-by-step recipe
for LLM agents doing the same for arbitrary data.
