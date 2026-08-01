# Running the server

stemma-server is the gRPC front door. It registers one or more user databases
at startup (creating each `.stemmadb` sidecar as needed) and serves the
Resolve API.

## Starting

```sh
bazel run //crates/stemma-server -- \
  --listen 127.0.0.1:50051 \
  --db mini=/path/to/mini.db \
  --db careg=eval/careg/data/careg.db
```

- `--listen` — address to bind (default `127.0.0.1:50051`).
- `--db name=path` — repeatable. `name` is the logical handle clients put in
  `ResolveRequest.database`; the sidecar store is created as
  `<path minus extension>.stemmadb` next to the user DB.

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
ResolveRequest  { query, database, options{max_candidates_per_mention, allow_lm, min_confidence} }
ResolveResponse { mentions[], rewritten_query }
Mention         { text, start, end, candidates[], nil }
Candidate       { table, rowid, column, value, score, evidence[] }
Evidence        { lexical | semantic | kg_path | probe | adjudication }
```

Semantics worth knowing:

- **Byte offsets**: `Mention.start/end` index into the request `query` bytes,
  end-exclusive.
- **Candidates are ranked, best first**, with calibrated scores in [0, 1].
  Consumers should treat the response as a candidate set, not a verdict.
- **`nil` vs empty**: `nil = true` means the pipeline affirmatively concluded
  no record matches; an empty candidate list without `nil` means it found
  nothing useful (weaker claim).
- **Every candidate carries evidence** — which channel matched what text, KG
  paths to co-mentioned entities, verification probes that ran, and the LM's
  rationale if adjudication happened.
- **`options.allow_lm`** gates the LM band entirely; with it off, resolution
  is purely lexical/dense/KG and fully local.

## Calling it with grpcurl

The server does not (yet) expose gRPC reflection, so pass the proto:

```sh
grpcurl -plaintext \
  -import-path proto -proto stemma/v1/resolve.proto \
  -d '{"query": "the Q3 numbers for the Seattle office", "database": "mini"}' \
  127.0.0.1:50051 stemma.v1.ResolveService/Resolve
```

Current skeleton behavior: a registered database returns `{}` (no mentions —
the pipeline lands in milestone 2); an unregistered database returns
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

## Operational notes

- The server holds one connection per database behind a mutex — adequate for
  research use; a connection pool arrives with the pipeline milestones.
- The user DB is attached `mode=ro`; the server cannot mutate it even if
  compromised by a bug.
- `.stemmadb` files use WAL mode; you may see `-wal`/`-shm` companions while
  the server runs.
