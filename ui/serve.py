#!/usr/bin/env python3
"""Launches the stemma console.

    python ui/serve.py --db mini=/tmp/mini.db --grpc 127.0.0.1:50051

Register the same databases (same names) as the running stemma-server;
browsing reads the SQLite files directly, resolution goes over gRPC.
"""

import argparse

import uvicorn

from app import create_app


def main() -> None:
    ap = argparse.ArgumentParser(description="stemma console")
    ap.add_argument("--listen", default="127.0.0.1:8600", help="host:port to serve the UI on")
    ap.add_argument("--grpc", default="127.0.0.1:50051", help="stemma-server gRPC address")
    ap.add_argument(
        "--db",
        action="append",
        default=[],
        metavar="NAME=PATH",
        help="database registration, matching stemma-server (repeatable)",
    )
    ap.add_argument(
        "--lm-endpoint",
        default="",
        help="OpenAI-compatible base URL for the chat view, e.g. http://host:8000/v1 "
        "(vLLM, llama.cpp, LiteLLM, hosted compatibility endpoints). "
        "Bearer token via env LM_API_KEY.",
    )
    ap.add_argument("--lm-model", default="", help="model name for --lm-endpoint")
    args = ap.parse_args()

    dbs = {}
    for spec in args.db:
        name, _, path = spec.partition("=")
        if not name or not path:
            ap.error(f"--db expects name=path, got {spec!r}")
        dbs[name] = path
    if not dbs:
        ap.error("at least one --db name=path is required")

    lm_cfg = None
    if args.lm_endpoint and args.lm_model:
        from lm import LmConfig

        lm_cfg = LmConfig(args.lm_endpoint, args.lm_model)

    host, _, port = args.listen.partition(":")
    uvicorn.run(
        create_app(dbs, args.grpc, lm_cfg),
        host=host,
        port=int(port or 8600),
        log_level="info",
    )


if __name__ == "__main__":
    main()
