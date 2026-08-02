---
name: stemmadb-setup
description: Stand up stemmadb from a bare machine to a served corpus — install toolchain, build, create/register databases, verify over gRPC. Use when asked to set up, install, configure, deploy, or run stemma/stemmadb.
---

# Setting up stemmadb

Goal state: `bazel test //...` green, stemma-server running, at least one user
database registered with its `.stemmadb` sidecar created, and a Resolve call
answering over gRPC.

## 1. Toolchain

Check before installing — only Bazelisk is strictly required:

```sh
which bazel || {
  mkdir -p ~/.local/bin
  curl -sL -o ~/.local/bin/bazelisk \
    https://github.com/bazelbuild/bazelisk/releases/latest/download/bazelisk-linux-amd64
  chmod +x ~/.local/bin/bazelisk && ln -sf ~/.local/bin/bazelisk ~/.local/bin/bazel
}
```

The repo's `.bazelversion` pins Bazel; bazelisk fetches it automatically. A C
compiler must exist (`cc --version`). Rust via rustup is optional (fast
`cargo test` iteration; the Bazel build is hermetic without it).

## 2. Build and verify

```sh
bazel test //...
```

Expect all tests passing. The critical suite is
`//crates/stemmadb:stemmadb_test` — it verifies sqlite-vec is statically
linked (`vec_version()` works), FTS5 exists, and user DBs attach read-only.

Failure modes:
- Rustc feature errors (e.g. `cfg_select!` unstable): the pinned toolchain in
  `MODULE.bazel` is too old for a dependency — raise
  `rust.toolchain(versions = ["..."])` and rebuild.
- Network errors: first build fetches the Rust toolchain and all crates;
  retry once connectivity is confirmed.

## 3. Provide a user database

Any stock SQLite file works. Do NOT add stemma-specific tables to it — all
derived state goes in the sidecar automatically. For a quick corpus, use the
bundled test data:

```sh
mkdir -p eval/mini/data
python3 -c "
import sqlite3
c = sqlite3.connect('eval/mini/data/mini.db')
c.executescript(open('eval/testdata/mini.sql').read())
c.close()"
```

For real data, follow the `stemmadb-corpus` skill.

## 4. Run the server

```sh
bazel build //crates/stemma-server
./bazel-bin/crates/stemma-server/stemma-server \
  --listen 127.0.0.1:50051 \
  --db mini=eval/mini/data/mini.db &
```

Verify from the logs (both lines must appear):
- `database registered name="mini" ... vec="v0.1.6"` — sidecar created,
  vector extension live;
- `stemma-server starting listen=...`.

`--db name=path` is repeatable for multiple databases. The sidecar is created
as `<path minus extension>.stemmadb` next to the user DB; ensure that
directory is writable. `RUST_LOG=debug` enables per-request logs.

## 5. Verify over gRPC

No reflection is registered yet — pass the proto to grpcurl:

```sh
grpcurl -plaintext \
  -import-path proto -proto stemma/v1/resolve.proto \
  -d '{"query": "the Q3 numbers for the Seattle office", "database": "mini"}' \
  127.0.0.1:50051 stemma.v1.ResolveService/Resolve
```

Correct responses by state of the codebase:
- Milestone 1 (skeleton): `{}` for a registered DB — success looks empty.
- Unknown DB name: `NotFound: unknown database "..."` — proves routing works.
- Milestone 2+: mentions with candidates and evidence.

If grpcurl is missing, install from
`https://github.com/fullstorydev/grpcurl/releases` (single binary), or use the
Rust client (`stemma_proto::v1::resolve_service_client::ResolveServiceClient`).

## 6. Configuration facts (current state)

- All configuration is CLI flags on stemma-server; there is no config file yet.
- The store schema is versioned (`PRAGMA user_version`). A
  `StoreVersionMismatch` error means the `.stemmadb` predates the current
  schema: delete the `.stemmadb` file (always safe — derived state) and rerun.
- Embedder/LM backends are not wired yet (milestones 3/5); nothing to
  configure for them today. When they land, they are separate processes: TEI
  (gRPC) for embedding, any OpenAI-compatible endpoint for the LM.

## Invariants you must not violate while setting up

1. Never write to the user database or point stemma at a DB you'd mind having
   locked read-only during serving.
2. Never hand-edit `.stemmadb` contents; delete-and-rebuild instead.
3. Keep the user DB and its `.stemmadb` adjacent (same directory) — tools
   assume the pairing.
