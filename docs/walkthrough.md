# Walkthrough

End-to-end demo: build stemma, load two corpora, query the Resolve API over
gRPC, and look inside the `.stemmadb` store. Every command below was run
against the repo as of milestone 1; expected output is shown trimmed.

## 0. Build and test (~3 min first time)

```sh
bazel test //...
```

```
//crates/stemma-eval:stemma-eval_test    PASSED
//crates/stemmadb:stemmadb_test          PASSED
Executed 2 out of 2 tests: 2 tests pass.
```

The stemmadb tests are the important ones: they prove sqlite-vec is statically
linked, FTS5 is available, and user databases attach read-only.

## 1. Build the demo corpus

The bundled mini corpus covers every mention class from the README — the
"Seattle office" nickname, the "Chen's team" association, the "crown"
alias — in six small tables:

```sh
mkdir -p eval/mini/data
python3 - <<'EOF'
import sqlite3
conn = sqlite3.connect('eval/mini/data/mini.db')
conn.executescript(open('eval/testdata/mini.sql').read())
conn.close()
print("mini.db built")
EOF
```

## 2. Start the server

```sh
bazel build //crates/stemma-server
./bazel-bin/crates/stemma-server/stemma-server --db mini=eval/mini/data/mini.db
```

```
INFO stemma_server: database registered name="mini" user_db=eval/mini/data/mini.db
     store=eval/mini/data/mini.stemmadb vec="v0.1.6"
INFO stemma_server: stemma-server starting listen=127.0.0.1:50051
```

Two things happened before serving: `eval/mini/data/mini.stemmadb` was created (the
sidecar store, with its versioned bookkeeping schema), and `vec="v0.1.6"`
confirmed the vector extension is live in-process.

## 3. Query the Resolve API

In another terminal (grpcurl needs the proto since the server doesn't expose
reflection yet):

```sh
grpcurl -plaintext \
  -import-path proto -proto stemma/v1/resolve.proto \
  -d '{"query": "the Q3 numbers for the Seattle office", "database": "mini"}' \
  127.0.0.1:50051 stemma.v1.ResolveService/Resolve
```

```
{}
```

That empty response is the milestone-1 contract: the request was parsed, the
database was found, the store was touched — and zero mentions came back
because the resolution pipeline lands in milestone 2. The error path is
already real:

```sh
grpcurl -plaintext -import-path proto -proto stemma/v1/resolve.proto \
  -d '{"query": "x", "database": "nope"}' \
  127.0.0.1:50051 stemma.v1.ResolveService/Resolve
```

```
ERROR: Code: NotFound  Message: unknown database "nope"
```

When milestone 2 lands, the first query above is specified to return a
`Mention{text: "the Seattle office"}` whose top candidate is
`{table: "offices", rowid: 17, column: "name", value: "Seattle - Northgate"}`
with `LexicalMatch` evidence — that exact case is the acceptance test.

## 4. Look inside the store

The sidecar is a normal SQLite database — inspect it like one:

```sh
sqlite3 eval/mini/data/mini.stemmadb "PRAGMA user_version; .tables"
```

`PRAGMA user_version` prints `7`. The version-stamped store contains derived
indexes and bookkeeping alongside
`query_log`, `chat_log`, and `grounding_feedback`. The indexes can be rebuilt.
History and feedback cannot, so preserve them according to the deployment's
retention policy before replacing the store.

Meanwhile the user database was attached read-only the whole time — the server
physically cannot write to it.

## 5. Scale up: the regulations corpus

With the [CA Code of Regulations corpus built](user-guide/04-corpora.md)
(57,523 sections of real regulatory text):

```sh
./bazel-bin/crates/stemma-server/stemma-server \
  --db mini=eval/mini/data/mini.db \
  --db careg=eval/careg/data/careg.db
```

```sh
grpcurl -plaintext -import-path proto -proto stemma/v1/resolve.proto \
  -d '{"query": "coastal development permits", "database": "careg"}' \
  127.0.0.1:50051 stemma.v1.ResolveService/Resolve
```

Same contract, real-world scale — and the corpus whose `uuid` column joins to
pre-computed embeddings, so milestone 3's dense retrieval can be demonstrated
on it without an embedding run.

## 6. Derive evaluation targets (optional)

To see the evaluation harness work on BIRD (after `eval/bird/fetch_bird.sh`):

```sh
bazel run //crates/stemma-eval -- derive \
  --questions eval/bird/data/dev/dev.json --out /tmp/targets.json
```

It parses every gold SQL into the tables and `column op literal` predicates a
resolver must reconstruct — the metric every milestone reports against.

## Where to next

- [Concepts](user-guide/02-concepts.md) — what the pipeline will do with these
  corpora, stage by stage.
- [Architecture](architecture.md) — why it's built this way.
- [skills/](../skills) — task recipes for LLM agents working on or with
  stemmadb.
