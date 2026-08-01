#!/usr/bin/env bash
# Type-checks and bundles the TypeScript UI to static/ui.js.
# Requires only the deno binary — no npm, no node_modules.
set -euo pipefail
cd "$(dirname "$0")"
deno check src/ui.ts
deno bundle --platform=browser --output=static/ui.js src/ui.ts
# cache-bust: stamp asset URLs with the bundle hash so browsers never serve
# a stale console after a deploy
HASH=$(cat static/ui.js static/ui.css | cksum | cut -d' ' -f1)
sed -i -E "s|/static/ui\.css(\?v=[0-9]+)?|/static/ui.css?v=${HASH}|; s|/static/ui\.js(\?v=[0-9]+)?|/static/ui.js?v=${HASH}|" static/index.html
echo "built static/ui.js (v=${HASH})"
