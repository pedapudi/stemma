#!/usr/bin/env bash
# Downloads the BIRD dev set (databases are SQLite files) into eval/bird/data.
# ~1–2 GB unzipped; resumable.
set -euo pipefail

DEST="$(cd "$(dirname "$0")" && pwd)/data"
mkdir -p "$DEST"

URL="https://bird-bench.oss-cn-beijing.aliyuncs.com/dev.zip"

echo "downloading BIRD dev set to $DEST ..."
curl -L --fail --continue-at - -o "$DEST/dev.zip" "$URL"
unzip -oq "$DEST/dev.zip" -d "$DEST"

# Some releases nest the databases in a second zip.
nested="$(find "$DEST" -maxdepth 2 -name 'dev_databases.zip' | head -1 || true)"
if [ -n "$nested" ]; then
  unzip -oq "$nested" -d "$(dirname "$nested")"
fi

echo "done. dev.json:"
find "$DEST" -maxdepth 2 -name 'dev.json' -o -maxdepth 2 -name '*.json' | head -5
