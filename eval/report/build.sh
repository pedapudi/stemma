#!/usr/bin/env bash
# Type-checks and bundles the report renderer, then inlines CSS + JS into
# dist/template.html — the single-file template stemma-eval embeds at
# compile time and injects each run's JSON into.
# Requires only the deno binary — no npm, no node_modules (mirrors ui/build.sh).
set -euo pipefail
cd "$(dirname "$0")"

deno check report.ts
mkdir -p dist
deno bundle --platform=browser --output=dist/report.js report.ts

python3 - <<'PY'
src = open("template.src.html").read()
css = open("report.css").read()
js = open("dist/report.js").read()
out = src.replace("/*__STEMMA_CSS__*/", css).replace("/*__STEMMA_JS__*/", js)
assert "__STEMMA_RUN_DATA__" in out, "template lost its data placeholder"
open("dist/template.html", "w").write(out)
print(f"built dist/template.html ({len(out)} bytes)")
PY
