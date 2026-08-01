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

Configuration (environment):
  STEMMADB_GRPC  stemma-server address        (default 127.0.0.1:50051)
  STEMMADB_DBS   name=path[,name=path...]     (required — same names as the server)

Run standalone:  STEMMADB_DBS=legal=/path/legal.db python stemmadb_mcp.py
Typical use: launched over stdio by an MCP client (see agents/stemma_agent).
"""

from __future__ import annotations

import os
import sys
from typing import Any

from mcp.server.fastmcp import FastMCP

# the stemmadb client library is the sibling package: pip install -e clients/python
from stemmadb import StemmaClient, StoreBrowser

GRPC = os.environ.get("STEMMADB_GRPC", "127.0.0.1:50051")
_dbs_spec = os.environ.get("STEMMADB_DBS", "")
DBS: dict[str, str] = {}
for part in filter(None, _dbs_spec.split(",")):
    name, _, path = part.partition("=")
    if name and path:
        DBS[name] = path
if not DBS:
    print("STEMMADB_DBS is required (name=path[,name=path...])", file=sys.stderr)
    sys.exit(2)

mcp = FastMCP(
    "stemmadb",
    instructions=(
        "stemma resolves natural-language mentions to database records. "
        "Before referring to any entity, value, table or column, pin it with "
        "resolve; cite resolutions as table.column #rowid. Use sql (read-only) "
        "to fetch what resolve pointed at — never invent identifiers."
    ),
)

_client = StemmaClient(GRPC)
_browsers = {name: StoreBrowser(path) for name, path in DBS.items()}


def _browser(database: str) -> StoreBrowser:
    if database not in _browsers:
        raise ValueError(f"unknown database {database!r}; available: {sorted(_browsers)}")
    return _browsers[database]


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


@mcp.tool()
def resolve(query: str, database: str) -> dict[str, Any]:
    """Resolve natural-language mentions in `query` to concrete records.

    Returns a compact digest for reasoning plus `trajectory` — the complete
    resolution trace (every span considered, per-channel scores, snippets,
    rejected near-misses with reasons) for clients that visualize resolution.
    """
    _browser(database)  # validate name before the RPC
    trace = _client.explain_dict(query, database=database)
    out = _compact(trace)
    out["trajectory"] = trace
    return out


@mcp.tool()
def sql(query: str, database: str) -> dict[str, Any]:
    """Run a read-only SELECT. Schema `src` is the user database (e.g.
    src.regulations); `main` is the stemma store. Never write."""
    result = _browser(database).query(query)
    return {
        "columns": result["columns"],
        "rows": result["rows"][:12],
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
            for t in _browser(database).schema()
        ]
    }


@mcp.tool()
def knowledge_graph(database: str) -> dict[str, Any]:
    """The compiled knowledge graph: schema layer, discovered joins
    (confidence-scored), characteristic terms and named entities with
    pagerank centrality. Useful for orienting in an unfamiliar corpus."""
    g = _browser(database).knowledge_graph()
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


if __name__ == "__main__":
    mcp.run()
