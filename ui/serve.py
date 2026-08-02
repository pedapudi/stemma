#!/usr/bin/env python3
"""Launches the stemma console.

    python ui/serve.py --config config.json
    python ui/serve.py --db mini=/tmp/mini.db --grpc 127.0.0.1:50051

Configuration comes from the config file and command-line flags only
(flags override the file) — never from environment variables. With no
--config flag, the nearest config.json at or above the repository root
is used when present.

Register the same databases (same names) as the running stemma-server;
browsing reads the SQLite files directly, resolution goes over gRPC.
"""

import argparse
import os
import sys

import uvicorn

_REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(_REPO, "clients", "python"))

from stemmadb import find_config, load_config  # noqa: E402

from app import create_app  # noqa: E402


def main() -> None:
    ap = argparse.ArgumentParser(description="stemma console")
    ap.add_argument("--config", default=None,
                    help="stemma config.json (default: nearest config.json above the repo)")
    ap.add_argument("--listen", default=None, help="host:port to serve the UI on")
    ap.add_argument("--grpc", default=None, help="stemma-server gRPC address")
    ap.add_argument(
        "--db",
        action="append",
        default=[],
        metavar="NAME=PATH",
        help="database registration, matching stemma-server (repeatable)",
    )
    ap.add_argument(
        "--lm-endpoint",
        default=None,
        help="OpenAI-compatible base URL for the chat view, e.g. http://host:8000/v1 "
        "(vLLM, llama.cpp, LiteLLM, hosted compatibility endpoints)",
    )
    ap.add_argument("--lm-model", default=None, help="model name for --lm-endpoint")
    ap.add_argument("--lm-api-key", default=None, help="bearer token for --lm-endpoint")
    args = ap.parse_args()

    cfg_path = args.config or find_config(_REPO)
    cfg = load_config(cfg_path) if cfg_path else {}
    console = cfg.get("console") or {}
    lm = console.get("lm") or {}

    dbs = {}
    for spec in args.db:
        name, _, path = spec.partition("=")
        if not name or not path:
            ap.error(f"--db expects name=path, got {spec!r}")
        dbs[name] = path
    if not dbs:
        dbs = cfg.get("databases") or {}
    if not dbs:
        ap.error("at least one database is required (--db name=path or --config)")

    endpoint = args.lm_endpoint if args.lm_endpoint is not None else lm.get("endpoint", "")
    model = args.lm_model if args.lm_model is not None else lm.get("model", "")
    api_key = args.lm_api_key if args.lm_api_key is not None else lm.get("api_key", "")
    lm_cfg = (endpoint, model, api_key) if endpoint and model else None

    listen = args.listen or console.get("listen") or "127.0.0.1:8600"
    grpc = args.grpc or console.get("grpc") or "127.0.0.1:50051"

    host, _, port = listen.partition(":")
    uvicorn.run(
        create_app(dbs, grpc, lm_cfg),
        host=host,
        port=int(port or 8600),
        log_level="info",
    )


if __name__ == "__main__":
    main()
