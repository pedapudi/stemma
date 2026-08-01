#!/usr/bin/env bash
# Type-checks and bundles the TypeScript UI to static/ui.js.
# Requires only the deno binary — no npm, no node_modules.
set -euo pipefail
cd "$(dirname "$0")"
deno check src/ui.ts
deno bundle --platform=browser --output=static/ui.js src/ui.ts
echo "built static/ui.js"
