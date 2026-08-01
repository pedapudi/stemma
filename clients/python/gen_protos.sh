#!/usr/bin/env bash
# Regenerates the checked-in Python stubs from proto/. Run after editing any
# .proto (the Rust twin is tools/regen_protos.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 -m grpc_tools.protoc -Iproto \
  --python_out=clients/python/stemmadb/_proto \
  --grpc_python_out=clients/python/stemmadb/_proto \
  proto/stemma/v1/resolve.proto proto/stemma/v1/embedder.proto
echo "regenerated clients/python/stemmadb/_proto"
