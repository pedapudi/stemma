"""stemma console — the optional web UI.

A thin FastAPI layer over the stemmadb client library: browsing and metadata
come from StoreBrowser (direct read-only SQLite), resolution and the query
trajectory come from StemmaClient (gRPC to stemma-server), and the
conversational layer drives any OpenAI-compatible LM through resolve/sql/schema
tools. Nothing in the core depends on this process.
"""

from __future__ import annotations

import os
import time

import grpc
from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from stemmadb import StemmaClient, StoreBrowser

STATIC = os.path.join(os.path.dirname(os.path.abspath(__file__)), "static")


class SqlRequest(BaseModel):
    sql: str


class ChatRequest(BaseModel):
    messages: list[dict]
    conversation: str = "default"


def create_app(
    dbs: dict[str, str],
    grpc_target: str,
    lm_cfg: tuple[str, str] | None = None,  # (endpoint, model)
) -> FastAPI:
    app = FastAPI(title="stemma console", docs_url=None, redoc_url=None)
    browsers = {name: StoreBrowser(path) for name, path in dbs.items()}
    client = StemmaClient(grpc_target)
    agent_chat = None
    if lm_cfg:
        from agent_backend import AgentChat

        agent_chat = AgentChat(dbs, grpc_target, lm_cfg[0], lm_cfg[1])

    def browser(name: str) -> StoreBrowser:
        b = browsers.get(name)
        if b is None:
            raise HTTPException(404, f"unknown database {name!r}")
        return b

    @app.get("/api/config")
    def config():
        return {
            "databases": sorted(dbs),
            "grpc": grpc_target,
            "lm": {"endpoint": lm_cfg[0], "model": lm_cfg[1]} if lm_cfg else None,
        }

    @app.get("/api/health")
    def health():
        t0 = time.monotonic()
        try:
            client.explain("", database=next(iter(sorted(dbs)), ""))
            ok = True
        except grpc.RpcError as e:
            ok = e.code() in (grpc.StatusCode.NOT_FOUND, grpc.StatusCode.FAILED_PRECONDITION)
        return {"grpc": ok, "latency_ms": round((time.monotonic() - t0) * 1e3, 1)}

    @app.get("/api/db/{name}/schema")
    def schema(name: str):
        return {
            "tables": [
                {
                    "name": t.name,
                    "row_count": t.row_count,
                    "columns": [vars(c) for c in t.columns],
                    "foreign_keys": [vars(fk) for fk in t.foreign_keys],
                }
                for t in browser(name).schema()
            ]
        }

    @app.get("/api/db/{name}/rows/{table}")
    def rows(name: str, table: str, limit: int = 50, after: int | None = None, q: str = ""):
        try:
            return browser(name).rows(table, limit=min(limit, 500), after=after, q=q)
        except ValueError as e:
            raise HTTPException(404, str(e)) from e

    @app.get("/api/db/{name}/graph")
    def graph(name: str):
        return browser(name).knowledge_graph()

    @app.get("/api/db/{name}/store")
    def store(name: str):
        meta = browser(name).store_meta()
        # KG stats ride along for the sidebar block.
        try:
            g = browser(name).knowledge_graph()
            meta["kg"] = {"layer": g["layer"], "nodes": len(g["nodes"]), "edges": len(g["edges"])}
        except Exception:
            meta["kg"] = None
        return meta

    @app.get("/api/db/{name}/examples")
    def examples(name: str):
        return {"examples": browser(name).examples()}

    @app.post("/api/db/{name}/sql")
    def sql(name: str, req: SqlRequest):
        t0 = time.monotonic()
        try:
            out = browser(name).query(req.sql)
            out["plan"] = browser(name).query_plan(req.sql)
        except ValueError as e:
            raise HTTPException(400, str(e)) from e
        except Exception as e:
            raise HTTPException(400, str(e)) from e
        out["elapsed_ms"] = round((time.monotonic() - t0) * 1e3, 1)
        return out

    @app.get("/api/db/{name}/resolve")
    def resolve(name: str, q: str):
        browser(name)  # 404 on unknown db before touching gRPC
        try:
            return client.explain_dict(q, database=name, source="console")
        except grpc.RpcError as e:
            raise HTTPException(502, f"stemma-server: {e.code().name}: {e.details()}") from e

    @app.post("/api/db/{name}/chat")
    async def chat(name: str, req: ChatRequest):
        if agent_chat is None:
            raise HTTPException(
                503,
                "no LM configured — start the console with --lm-endpoint/--lm-model "
                "(any OpenAI-compatible endpoint: vLLM, llama.cpp, LiteLLM, …)",
            )
        browser(name)
        text = next(
            (m.get("content", "") for m in reversed(req.messages) if m.get("role") == "user"),
            "",
        )
        if not text:
            raise HTTPException(400, "no user message")
        conv = req.conversation or "default"
        try:
            return await agent_chat.send(
                name, text,
                explain_fn=lambda q, database: client.explain_dict(
                    q, database=database, source="agent", session=f"{name}/{conv}"
                ),
                conv=conv,
            )
        except Exception as e:
            raise HTTPException(502, f"agent: {e}") from e

    @app.get("/api/db/{name}/chat")
    def chat_transcript(name: str, conversation: str = "default"):
        browser(name)
        if agent_chat is None:
            return {"messages": []}
        return {"messages": agent_chat.transcript(name, conversation)}

    @app.get("/api/db/{name}/chats")
    def chats(name: str):
        browser(name)
        if agent_chat is None:
            return {"conversations": []}
        return {"conversations": agent_chat.conversations(name)}

    @app.get("/api/db/{name}/history")
    def history(name: str, limit: int = 8):
        try:
            r = browser(name).query(
                "SELECT query, max(asked_at) AS at, max(source) AS src FROM query_log "
                f"GROUP BY query ORDER BY at DESC LIMIT {min(int(limit), 30)}"
            )
            return {"queries": [row[0] for row in r["rows"]],
                    "tagged": [{"query": row[0], "source": row[2]} for row in r["rows"]]}
        except Exception:
            return {"queries": []}

    @app.get("/")
    def index():
        return FileResponse(os.path.join(STATIC, "index.html"))

    app.mount("/static", StaticFiles(directory=STATIC), name="static")
    return app
