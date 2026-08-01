"""stemma console — the optional web UI.

A thin FastAPI layer over the stemmadb client library: browsing and metadata
come from StoreBrowser (direct read-only SQLite), resolution and the query
trajectory come from StemmaClient (gRPC to stemma-server). Nothing in the
core depends on this process; run it only when you want the console.
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


def create_app(dbs: dict[str, str], grpc_target: str) -> FastAPI:
    app = FastAPI(title="stemma console", docs_url=None, redoc_url=None)
    browsers = {name: StoreBrowser(path) for name, path in dbs.items()}
    client = StemmaClient(grpc_target)

    def browser(name: str) -> StoreBrowser:
        b = browsers.get(name)
        if b is None:
            raise HTTPException(404, f"unknown database {name!r}")
        return b

    @app.get("/api/config")
    def config():
        return {"databases": sorted(dbs), "grpc": grpc_target}

    @app.get("/api/health")
    def health():
        t0 = time.monotonic()
        try:
            client.explain("", database=next(iter(sorted(dbs)), ""))
            ok = True
        except grpc.RpcError as e:
            # NOT_FOUND means the server answered; anything transport-shaped
            # means it didn't.
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
    def rows(name: str, table: str, limit: int = 50, offset: int = 0):
        try:
            return browser(name).rows(table, limit=min(limit, 500), offset=max(offset, 0))
        except ValueError as e:
            raise HTTPException(404, str(e)) from e

    @app.get("/api/db/{name}/graph")
    def graph(name: str):
        return browser(name).schema_graph()

    @app.get("/api/db/{name}/store")
    def store(name: str):
        return browser(name).store_meta()

    @app.post("/api/db/{name}/sql")
    def sql(name: str, req: SqlRequest):
        t0 = time.monotonic()
        try:
            out = browser(name).query(req.sql)
        except ValueError as e:
            raise HTTPException(400, str(e)) from e
        except Exception as e:  # sqlite errors -> readable message
            raise HTTPException(400, str(e)) from e
        out["elapsed_ms"] = round((time.monotonic() - t0) * 1e3, 1)
        return out

    @app.get("/api/db/{name}/resolve")
    def resolve(name: str, q: str):
        browser(name)  # 404 on unknown db before touching gRPC
        try:
            return client.explain_dict(q, database=name)
        except grpc.RpcError as e:
            raise HTTPException(502, f"stemma-server: {e.code().name}: {e.details()}") from e

    @app.get("/")
    def index():
        return FileResponse(os.path.join(STATIC, "index.html"))

    app.mount("/static", StaticFiles(directory=STATIC), name="static")
    return app
