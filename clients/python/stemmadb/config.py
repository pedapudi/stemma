"""The stemma config file.

One JSON file describes a deployment; every process takes it via --config
and command-line flags override its fields. Configuration never comes from
environment variables.

    {
      "databases": { "legal": "eval/legal/data/legal.db" },
      "server":  { "listen": "127.0.0.1:50051",
                   "embedder": { "endpoint": "http://host:8081/v1",
                                 "model": "Qwen3-Embedding-0.6B" } },
      "console": { "listen": "127.0.0.1:8600",
                   "grpc": "127.0.0.1:50051",
                   "lm": { "endpoint": "http://host:8080/v1",
                           "model": "…", "api_key": "" } }
    }

Relative database paths are resolved against the config file's directory,
so the file means the same thing from any working directory.
"""

from __future__ import annotations

import json
import os
from typing import Any

DEFAULT_CONFIG = "config.json"


def load_config(path: str) -> dict[str, Any]:
    """Parse a stemma config file; database paths come back absolute."""
    with open(path) as f:
        cfg = json.load(f)
    base = os.path.dirname(os.path.abspath(path))
    cfg["databases"] = {
        name: p if os.path.isabs(p) else os.path.join(base, p)
        for name, p in (cfg.get("databases") or {}).items()
    }
    return cfg


def find_config(start: str) -> str | None:
    """Nearest config.json at or above `start` — the zero-flag default."""
    d = os.path.abspath(start)
    while True:
        candidate = os.path.join(d, DEFAULT_CONFIG)
        if os.path.exists(candidate):
            return candidate
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent
