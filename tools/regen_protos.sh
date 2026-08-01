#!/usr/bin/env bash
# Refresh crates/stemma-proto/src/gen from proto/. Run after editing any .proto.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo run -p proto-gen
