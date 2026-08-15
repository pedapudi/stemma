# Running the server

stemma-server is the gRPC front door. It registers one or more user databases
at startup (creating each `.stemmadb` sidecar as needed) and serves the
Resolve API.

## Starting

```sh
bazel run //crates/stemma-server -- --config config.json
# or fully by flags:
bazel run //crates/stemma-server -- \
  --listen 127.0.0.1:50051 \
  --db mini=/path/to/mini.db \
  --db careg=eval/careg/data/careg.db
```

- `--config` — a stemma `config.json`; the server reads `databases` and
  `server.*` (listen address, embedder endpoint/model for the dense channel,
  lm endpoint/model for the adjudication band). Flags override the file,
  field by field. Relative database paths resolve against the file's
  directory. One file describes one deployment — the console and the MCP
  server read their own sections of the same file. Configuration never comes
  from environment variables.
- `--listen` — address to bind (default `127.0.0.1:50051`).
- `--db name=path` — repeatable. `name` is the logical handle clients put in
  `ResolveRequest.database`; the sidecar store is created as
  `<path minus extension>.stemmadb` next to the user DB.
- `--dense-search exact|usearch` — dense candidate source. The default is
  `exact`; the same setting is available as `server.dense_search`. The
  `usearch` value requires a build with the `usearch-sidecar` Cargo feature.
- `--embed-endpoint`, `--embed-model` — compatible embedding-service
  endpoint and served model identity for the dense channel. Without them,
  resolution uses lexical and graph evidence.
- `--lm-endpoint`, `--lm-model` — compatible language-service endpoint and
  served model identity for adjudication and parsing. Without them, resolution
  remains available while service-dependent bands are unavailable.
  Even when configured, the band runs only for requests that set
  `options.allow_lm`.

Startup logs confirm each registration, including the linked sqlite-vec
version:

```
INFO stemma_server: database registered name="careg" user_db=eval/careg/data/careg.db
     store=eval/careg/data/careg.stemmadb vec="v0.1.6"
INFO stemma_server: stemma-server starting listen=127.0.0.1:50051
```

Set `RUST_LOG=debug` for per-request logging.

## The Resolve API

Defined in [`proto/stemma/v1/resolve.proto`](../../proto/stemma/v1/resolve.proto)
(source of truth — the comments there are the API reference). Shape:

```
ResolveRequest  { query, database, options{allow_lm, source, session, ...} }
ResolveResponse { mentions[], outcome, clarification, episode_id }
Mention         { text, start, end, candidates[], nil }
Candidate       { table, rowid, column, value, score, evidence[] }
Evidence        { lexical | semantic | kg_path | probe | adjudication }
```

Semantics worth knowing:

- **Byte offsets**: `Mention.start/end` index into the request `query` bytes,
  end-exclusive.
- **Candidates are ranked, best first**, with heuristic fused scores in [0, 1].
  The scores are not probabilities. Consumers should inspect `outcome` and
  preserve supported alternatives.
- **`nil` vs empty**: `nil = true` means the pipeline affirmatively concluded
  no record matches; an empty candidate list without `nil` means it found
  nothing useful (weaker claim).
- **Every candidate carries channel evidence** with the matched text and native
  score. `Explain` additionally exposes coherence paths, adjudication marks,
  reach, and near-miss reasons. The structured `kg_path`, `probe`, and
  `adjudication` variants in `Candidate.evidence` remain reserved.
- **`options.allow_lm`** gates the language-service bands. With it off,
  resolution uses lexical, dense, and graph evidence. A configured embedding
  service may still receive query spans for the dense channel.
- **`source` and `session`** tag query history and provide provenance for
  session-scoped feedback.
- **`episode_id`** identifies the recorded evidence snapshot used by explicit
  feedback. It is empty when resolution succeeds but history persistence fails.
- **`max_candidates_per_mention` and `min_confidence`** are accepted request
  fields but are not applied by the current server.

`Explain` accepts the same request and returns the full trajectory. `Parse`
resolves first and invokes the bounded query proposer only when grounding is
settled. Its response distinguishes ambiguous grounding, missing evidence,
service unavailability, invalid proposals, and accepted parameterized SQL.

The feedback methods attach a typed judgment to a recorded episode:

| Method | Purpose |
|---|---|
| `SubmitFeedback` | Record approval, rejection, a grounding correction, or a parse failure category. |
| `ListFeedback` | Inspect every event in a database or one episode. |
| `DeleteFeedback` | Permanently remove one event. |

Feedback submission validates target indices and the active indexed-corpus and
vector-registry revisions. See [Explicit grounding
feedback](06-feedback.md) for category and retention rules.

## Calling it with grpcurl

The server does not (yet) expose gRPC reflection, so pass the proto:

```sh
grpcurl -plaintext \
  -import-path proto -proto stemma/v1/resolve.proto \
  -d '{"query": "the Q3 numbers for the Seattle office", "database": "mini"}' \
  127.0.0.1:50051 stemma.v1.ResolveService/Resolve
```

A registered database returns an outcome, its selected mentions, and an episode
identifier when history persistence succeeds. An unregistered database returns
`NotFound: unknown database "..."`.

## From Rust

`crates/stemma-proto` exports the generated client:

```rust
use stemma_proto::v1::resolve_service_client::ResolveServiceClient;
use stemma_proto::v1::ResolveRequest;

let mut client = ResolveServiceClient::connect("http://127.0.0.1:50051").await?;
let resp = client
    .resolve(ResolveRequest {
        query: "what did Chen's team ship".into(),
        database: "mini".into(),
        options: None,
    })
    .await?
    .into_inner();
```

## Optional approximate dense retrieval

Approximate retrieval is opt-in at build time and runtime. Build and start the
server with Cargo:

```sh
cargo run -p stemma-server --features usearch-sidecar -- \
  --dense-search usearch \
  --db catalog=/path/to/catalog.db
```

The configuration-file equivalent of the runtime flag is:

```json
{
  "server": {
    "dense_search": "usearch"
  }
}
```

The equivalent flag is `--dense-search usearch`. The server creates one
`<database>.stemmadb.usearch` directory beside each store. At startup it reuses
a valid index or builds one from `vec_dense`. It rebuilds after a background
embedding pass changes document vectors. `vector_sidecar_receipts` in SQLite
binds the active file to the corpus fingerprint, vector identity and generation,
shape, metric, and checksum.

A missing, stale, corrupt, or incompatible file emits a warning and uses exact
SQLite search. Approximate keys map back to SQLite vectors for exact rescoring.
If approximate evidence could produce a mention, the resolver repeats that
span's dense search through SQLite before fusion. Approximate margins therefore
cannot establish that no competing interpretation exists.

The server attempts to replace a corrupt file during startup rebuild. If
recovery repeatedly fails, stop the server, move only the affected
`.stemmadb.usearch` directory aside, and restart in `usearch` mode. After the
replacement validates, the moved directory can be deleted. Do not delete
`.stemmadb` as a substitute because that file also contains retained query,
chat, and feedback records.

Evaluate the sidecar near 100,000 vectors per frequently searched scope, after
exact scans materially affect latency. The
[`dense_shadow_compare.py`](../../tools/dense_shadow_compare.py) tool compares
paired approximate and exact exports. Deployment requires zero missed
ambiguity, zero guarded outcome changes, and a predeclared latency improvement.
Runtime export of paired observations is not automated yet; see the
[file contract](../../tools/dense_shadow_compare.md).

The current sidecar indexes document vectors only. Interpretation vectors keep
the exact path. Approximate search does not replace lexical indexes, graph
evidence, database probes, or SQLite as the vector authority.

Native sidecar support is not wired into the Bazel build. Bazel builds retain
the exact backend and reject `dense_search = "usearch"` at startup with a clear
feature-required error. Cargo is the supported sidecar build path until native
Bazel parity lands.

## Operational notes

- The server holds one connection per database behind a mutex — adequate for
  research use; a connection pool arrives with the pipeline milestones.
- The user DB is attached `mode=ro`; the server cannot mutate it even if
  compromised by a bug.
- `.stemmadb` files use WAL mode; you may see `-wal`/`-shm` companions while
  the server runs.
- Exact SQLite vector search is the default. See
  [the vector-sidecar design](../design/03-resolution.md#optional-approximate-vector-sidecar)
  for the approximate path's internal contract and limitations.
