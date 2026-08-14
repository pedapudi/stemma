#!/usr/bin/env python3
"""Smoke-test an explicitly configured language service without leaking identity."""

import argparse
import json
import sys
import urllib.error
import urllib.request


def request(url, payload=None, timeout=20):
    data = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/json"},
        method="GET" if data is None else "POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as response:
        return json.load(response)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--endpoint", required=True, help="base URL ending in /v1")
    parser.add_argument("--catalog-index", type=int, default=0)
    parser.add_argument("--timeout", type=int, default=20)
    args = parser.parse_args()
    base = args.endpoint.rstrip("/")

    catalog = request(f"{base}/models", timeout=args.timeout).get("data", [])
    if not 0 <= args.catalog_index < len(catalog):
        raise RuntimeError("catalog index is out of range")
    deployment = catalog[args.catalog_index].get("id")
    if not deployment:
        raise RuntimeError("catalog entry has no identifier")
    print("catalog: pass")

    body = {
        "model": deployment,
        "temperature": 0,
        "messages": [{"role": "user", "content": "Reply with exactly: ready"}],
    }
    reply = request(f"{base}/chat/completions", body, args.timeout)
    content = reply["choices"][0]["message"]["content"].strip().lower()
    if "ready" not in content:
        raise RuntimeError("completion did not return the expected marker")
    print("completion: pass")

    schema = {
        "type": "object",
        "properties": {"ready": {"type": "boolean"}},
        "required": ["ready"],
        "additionalProperties": False,
    }
    body["messages"] = [{"role": "user", "content": "Report readiness."}]
    body["response_format"] = {
        "type": "json_schema",
        "json_schema": {"name": "readiness", "strict": True, "schema": schema},
    }
    reply = request(f"{base}/chat/completions", body, args.timeout)
    result = json.loads(reply["choices"][0]["message"]["content"])
    if result != {"ready": True}:
        raise RuntimeError("structured completion failed its contract")
    print("structured output: pass")


if __name__ == "__main__":
    try:
        main()
    except (KeyError, ValueError, RuntimeError, urllib.error.URLError) as error:
        print(f"validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
