#!/usr/bin/env python3
"""stemmadb MCP server — resolution as a first-class tool for any agent.

Exposes stemma over the Model Context Protocol (stdio transport):

  resolve(query, database)  — pin natural-language mentions to records; the
                              result carries both a compact summary sized for
                              model context AND the full trajectory (every
                              span, channel, candidate and near-miss) as
                              structured content, so MCP clients can render
                              resolution the way the stemma console does
  sql(query, database)      — read-only SELECT over user DB (src) + store (main)
  schema(database)          — tables, columns, declared relations
  knowledge_graph(database) — the compiled graph: schema + discovered joins +
                              profile layers, with provenance and centrality

Configuration comes from flags (or a config file the flags point at) —
never from environment variables:

  --config PATH   stemma config.json (databases + console.grpc)
  --grpc ADDR     stemma-server address (overrides the file)
  --db NAME=PATH  database registration, repeatable (overrides the file)
  --session ID    session tag recorded with each resolve in query_log

Run standalone:  python stemmadb_mcp.py --config ../../config.json
Typical use: launched over stdio by an MCP client (see agents/stemma_agent).
"""

from __future__ import annotations

import argparse
import os
import sys
from typing import Any

from mcp.server.fastmcp import FastMCP

_REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(_REPO, "clients", "python"))

# the stemmadb client library is the sibling package: pip install -e clients/python
from stemmadb import StemmaClient, StoreBrowser, find_config, load_config  # noqa: E402


def _compact(trace: dict[str, Any]) -> dict[str, Any]:
    """The model-facing digest: selected candidates only, values truncated."""
    mentions = []
    for i in trace.get("mentions", []):
        s = trace["spans"][i]
        mentions.append({
            "mention": s["text"],
            "candidates": [
                {
                    "ref": f"{c['table']}.{c['column']} #{c['rowid']}",
                    "value": (c.get("snippet") or c["value"])[:200],
                    "score": round(c["score"], 3),
                }
                for c in s["candidates"] if c["selected"]
            ],
        })
    return {
        "mentions": mentions,
        "near_misses_considered": [
            s["text"] for s in trace.get("spans", [])
            if s["status"] in ("weak", "overlapped") and s["candidates"]
        ][:8],
    }


def build_server(grpc: str, dbs: dict[str, str], session: str = "") -> FastMCP:
    """The MCP server, closed over its client and browsers — no globals."""
    mcp = FastMCP(
        "stemmadb",
        instructions=(
            "stemma resolves natural-language mentions to database records. "
            "Before referring to any entity, value, table or column, pin it with "
            "resolve; cite resolutions as table.column #rowid. Use sql (read-only) "
            "to fetch what resolve pointed at — never invent identifiers."
        ),
    )
    client = StemmaClient(grpc)
    browsers = {name: StoreBrowser(path) for name, path in dbs.items()}

    def browser(database: str) -> StoreBrowser:
        if database not in browsers:
            raise ValueError(f"unknown database {database!r}; available: {sorted(browsers)}")
        return browsers[database]

    @mcp.tool()
    def resolve(query: str, database: str) -> dict[str, Any]:
        """Resolve natural-language mentions in `query` to concrete records.

        Returns a compact digest for reasoning plus `trajectory` — the complete
        resolution trace (every span considered, per-channel scores, snippets,
        rejected near-misses with reasons) for clients that visualize resolution.
        """
        browser(database)  # validate name before the RPC
        trace = client.explain_dict(query, database=database, source="mcp", session=session)
        out = _compact(trace)
        out["trajectory"] = trace
        return out

    @mcp.tool()
    def sql(query: str, database: str) -> dict[str, Any]:
        """Run a read-only SELECT. Schema `src` is the user database (e.g.
        src.regulations); `main` is the stemma store. Never write.

        Prefer fetching the rows resolve pointed at (WHERE id IN (...));
        cells are clipped to 1500 chars, so SELECT the columns you need."""
        result = browser(database).query(query)

        def clip(v: Any) -> Any:
            if isinstance(v, str) and len(v) > 1500:
                return v[:1500] + f"… [+{len(v) - 1500} chars]"
            return v

        return {
            "columns": result["columns"],
            "rows": [[clip(v) for v in row] for row in result["rows"][:12]],
            "truncated": result.get("truncated", False) or len(result["rows"]) > 12,
        }

    @mcp.tool()
    def schema(database: str) -> dict[str, Any]:
        """Tables, columns and declared relations of the database."""
        return {
            "tables": [
                {
                    "name": t.name,
                    "approx_rows": t.row_count,
                    "columns": [c.name for c in t.columns],
                    "foreign_keys": [
                        f"{fk.from_column} -> {fk.to_table}.{fk.to_column or 'id'}"
                        for fk in t.foreign_keys
                    ],
                }
                for t in browser(database).schema()
            ]
        }

    @mcp.tool()
    def knowledge_graph(database: str) -> dict[str, Any]:
        """The compiled knowledge graph: schema layer, discovered joins
        (confidence-scored), characteristic terms and named entities with
        pagerank centrality. Useful for orienting in an unfamiliar corpus."""
        g = browser(database).knowledge_graph()
        # digest: full node/edge dump is for UIs, not model context
        terms = sorted(
            (n for n in g["nodes"] if n["kind"] == "term"),
            key=lambda n: n["props"].get("centrality", 0),
            reverse=True,
        )
        return {
            "layer": g["layer"],
            "tables": [
                {"name": n["label"], **n["props"]}
                for n in g["nodes"] if n["kind"] == "table"
            ],
            "characteristic_terms": [n["label"] for n in terms[:30]],
            "joins": [
                {"from": e["source"], "to": e["target"], "label": e["label"],
                 **{k: v for k, v in e["props"].items() if k in ("method", "confidence")}}
                for e in g["edges"] if e["kind"] in ("fk", "inferred_fk")
            ],
        }

    return mcp


def main() -> None:
    ap = argparse.ArgumentParser(description="stemmadb MCP server")
    ap.add_argument("--config", default=None,
                    help="stemma config.json (default: nearest config.json above the repo)")
    ap.add_argument("--grpc", default=None, help="stemma-server address")
    ap.add_argument("--db", action="append", default=[], metavar="NAME=PATH",
                    help="database registration, repeatable")
    ap.add_argument("--session", default="", help="session tag for query_log")
    args = ap.parse_args()

    cfg_path = args.config or find_config(_REPO)
    cfg = load_config(cfg_path) if cfg_path else {}

    dbs: dict[str, str] = {}
    for part in args.db:
        name, _, path = part.partition("=")
        if not name or not path:
            ap.error(f"--db expects name=path, got {part!r}")
        dbs[name] = path
    if not dbs:
        dbs = cfg.get("databases") or {}
    if not dbs:
        ap.error("at least one database is required (--db name=path or --config)")

    grpc = args.grpc or (cfg.get("console") or {}).get("grpc") or "127.0.0.1:50051"
    build_server(grpc, dbs, session=args.session).run()


if __name__ == "__main__":
    main()
