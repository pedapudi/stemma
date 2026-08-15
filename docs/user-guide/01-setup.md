# Setup

## Prerequisites

- **Linux or macOS**, with a C compiler (`gcc` or `clang`) — the build compiles
  SQLite and sqlite-vec from vendored/bundled sources.
- **Bazelisk** (recommended way to get Bazel; the repo pins the Bazel version
  in `.bazelversion`):

  ```sh
  curl -sL -o ~/.local/bin/bazelisk \
    https://github.com/bazelbuild/bazelisk/releases/latest/download/bazelisk-linux-amd64
  chmod +x ~/.local/bin/bazelisk
  ln -s ~/.local/bin/bazelisk ~/.local/bin/bazel
  ```

- **Rust** (optional but recommended): any recent stable toolchain via
  [rustup](https://rustup.rs). Bazel provisions its own hermetic Rust
  toolchain, so this is only needed for `cargo test` iteration and the proto
  regeneration tool.
- **Python 3 + pyarrow** (optional): only for corpus-building scripts that read
  parquet, e.g. `eval/careg/build_careg_db.py`.
- **grpcurl** (optional): to poke the server from the command line.

No GPU, no Docker, and no external database are required. Network access is
needed on first build (Bazel fetches the pinned toolchain and crates).

## Build and test

Bazel is the build system of record:

```sh
bazel test //...          # builds everything, runs all tests
bazel build //crates/stemma-server
```

The Cargo workspace doubles as the dependency manifest for Bazel
(crate_universe reads `Cargo.toml`/`Cargo.lock`), which keeps plain Cargo
fully usable for fast iteration:

```sh
cargo test                # same tests, faster edit-compile loop
```

Both must stay green; CI treats Bazel as authoritative.

First Bazel build takes a few minutes (toolchain + ~250 crate fetches); after
that, incremental builds are seconds.

### Verify the install

```sh
bazel test //crates/stemmadb:stemmadb_test --test_output=summary
```

The stemmadb tests prove the three load-bearing facts about your build: the
sqlite-vec extension is statically linked and registering (`vec_version()`
returns), FTS5 is present, and user databases attach read-only.

## Layout of a running system

```
your-data.db            # your SQLite database — never modified
your-data.stemmadb      # derived indexes plus retained history and feedback
```

Point the server at a database and the sidecar store is created automatically:

```sh
bazel run //crates/stemma-server -- --db mydb=/path/to/your-data.db
```

Continue with the [walkthrough](../walkthrough.md) or the
[concepts guide](02-concepts.md).

## Troubleshooting

- **`cfg_select!` / edition errors from Bazel builds** — the pinned Rust
  toolchain in `MODULE.bazel` (`rust.toolchain(versions = [...])`) is older
  than a dependency requires. Bump it to at least the version in the error and
  rebuild.
- **`unknown database` from the server** — the `--db name=path` flag names
  databases; the `database` field of ResolveRequest must match a registered
  name exactly.
- **Store version mismatch error on open** — the `.stemmadb` file was written
  by a newer schema version. Use a compatible server build, or preserve its
  query, chat, and feedback records before moving the file aside and
  re-ingesting. Older stores migrate in place when opened.
- **Slow first build** — expected; crate_universe fetches all workspace
  dependencies once. Subsequent builds hit the Bazel cache.
