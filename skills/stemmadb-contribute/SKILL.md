---
name: stemmadb-contribute
description: Contribute code to stemma/stemmadb — repo invariants, dual Cargo/Bazel build rules, change recipes (deps, crates, protos, extensions), and testing conventions. Use before writing or reviewing any code change in this repo.
---

# Contributing to stemma

## Read first

- `docs/architecture.md` — the design and its reasoning; changes that fight it
  need a documented reason.
- The milestone plan at the bottom of that file — land work in milestone
  order; don't build milestone-5 machinery while milestone-2 gaps exist.

## Architectural invariants (violations are review-blockers)

1. **The user database is read-only.** Attached `mode=ro` as schema `src`.
   No code path may write to it; probes are SELECTs.
2. **All derived state is disposable** and lives in the `.stemmadb` store.
   Any feature must survive "delete the store, re-ingest".
3. **SQLite stays stock.** New capability enters as loadable-extension code
   statically registered via `sqlite3_auto_extension` (see
   `third_party/sqlite_vec` + `stemmadb::register_extensions`) — never as
   patches to SQLite, never as runtime `.so` loading.
4. **Subsystem SQL stays behind its trait.** Graph SQL only inside the
   SQLite `KnowledgeStore` backend (stemma-kg); index/queue SQL in
   stemmadb/stemma-ingest; the resolution pipeline (stemma-resolve) programs
   against traits and must compile without knowing SQL exists.
5. **Models are pluggable backends.** Embedders behind the `Embedder` trait
   (stemma-embed), LMs behind the LM backend trait (stemma-lm, primary
   backend = OpenAI-compatible protocol). No model-vendor SDK types outside
   those crates. The LM is never a retrieval mechanism — expansion and
   select-among-k adjudication only.
6. **Vector-space hygiene.** Every vec0 table has a `model_registry` row;
   model change = new table + blue-green backfill, never in-place mixing.
7. **Recall-first, evidence always.** Pipeline stages return ranked candidate
   sets, not early hard commits; every candidate carries at least one
   `Evidence`. An affirmative "no match" is `nil = true`, distinct from
   "found nothing".
8. **The store schema is versioned.** Any change to store tables bumps
   `STORE_SCHEMA_VERSION` in `crates/stemmadb/src/lib.rs`; mismatch is a
   hard error by design (re-ingest is cheap).

## Build system: one manifest, two builds

Cargo is the *dependency manifest*; Bazel is the *build of record*. Both must
stay green:

```sh
cargo test          # fast iteration
bazel test //...    # authoritative
```

Bazel reads `Cargo.toml`/`Cargo.lock` through crate_universe
(`MODULE.bazel`), but **BUILD files do not update themselves** — that's the
contributor's job. Recipes:

### Add a third-party dependency
```sh
cargo add -p <crate> <dep> [--features ...]   # updates Cargo.toml + Cargo.lock
```
Then add `"@crates//:<dep>"` to the `deps` of the consuming target in
`crates/<crate>/BUILD.bazel`. Target names in `@crates` use the crate's real
name (hyphens preserved, e.g. `@crates//:tonic-prost`). Bazel picks up the
lockfile change automatically.

### Add a workspace crate
1. `crates/<name>/{Cargo.toml,src/lib.rs}` (workspace members glob
   `crates/*` — no root Cargo.toml edit needed).
2. Add `"//crates/<name>:Cargo.toml"` to the `manifests` list in
   `MODULE.bazel` (`crate.from_cargo`).
3. Write `crates/<name>/BUILD.bazel` (`rust_library` + `rust_test`; copy an
   existing one, edition `2021`, `visibility = ["//visibility:public"]`).
4. `cargo check && bazel test //...`.

### Change a proto
1. Edit files under `proto/` (source of truth — document semantics in
   comments there, they are the API reference).
2. `tools/regen_protos.sh` (runs the cargo-based generator; needs rustup).
3. Commit the regenerated `crates/stemma-proto/src/gen/*.rs` **with** the
   proto change; Bazel compiles the checked-in generated code.
Breaking wire changes need a `stemma.v2` package, not edits to v1 semantics.

### Add a C extension (sqlite-vec pattern)
1. Vendor sources under `third_party/<name>/` with a `cc_library` BUILD
   (copts to silence vendor warnings; depend on
   `//third_party/sqlite:sqlite3_headers`).
2. Reference it from `crates/stemmadb/build.rs` (cargo path) **and** the
   stemmadb `BUILD.bazel` deps (Bazel path) — the dual-build cost of vendored
   C, kept in exactly one crate.
3. Register in `stemmadb::register_extensions` and add a test proving it
   loads (the `vec_version()` test is the pattern).

### Bump the Rust toolchain
Bazel: `rust.toolchain(versions = [...])` in `MODULE.bazel`. Keep >= the
version new dependencies need; local cargo uses whatever rustup provides.

## Testing conventions

- Unit tests live with the code (`#[cfg(test)]`), run under both builds.
  `rust_test(crate = ":lib")` in BUILD wires them into Bazel.
- Anything touching SQLite uses `StemmaDb::open_in_memory()` or a temp-dir
  pair; tests must not leave files outside temp dirs.
- Every architectural invariant that *can* be asserted cheaply is a test
  (read-only attach, store versioning, extension registration are the
  existing examples — extend the pattern).
- Golden tests for the Resolve API use the mini corpus
  (`eval/testdata/mini.sql`) — it is designed so each mention class (nickname,
  abbreviation, description, association) has a known correct resolution;
  extend it rather than inventing parallel fixtures.
- Eval changes: see the `stemmadb-eval` skill; new target extraction logic
  requires a unit test with hand-written SQL.

## Style

- Rust 2021, rustfmt defaults. Comments explain constraints code can't show,
  in the file's existing voice; no changelog-style comments.
- Error handling: `thiserror` enums in library crates, `anyhow` at binary
  edges. SQLite errors bubble through the crate's `Error`, not `unwrap()`.
- Logging via `tracing`, structured fields (`tracing::info!(name, ...)`), no
  `println!` outside CLI output paths.
- Commit messages: imperative summary line, body explains what and why. No
  attribution trailers or generated-by tags of any kind.

## Landing checklist

- [ ] `cargo test` and `bazel test //...` both green
- [ ] BUILD files updated for any dep/crate/proto change
- [ ] Generated proto code regenerated and committed if protos changed
- [ ] `STORE_SCHEMA_VERSION` bumped if store tables changed
- [ ] New invariants tested; mini corpus extended if a new mention class
- [ ] Docs touched if behavior visible in docs changed
      (`docs/`, crate READMEs, or the relevant skill)
