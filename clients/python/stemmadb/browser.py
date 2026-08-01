"""Read-only access to the two SQLite files of a stemma deployment.

Browsing is a storage-layer concern: the user database and the ``.stemmadb``
store are plain SQLite files, so navigation needs no server round-trip. Every
connection here is opened with ``mode=ro`` — this module cannot write.
"""

from __future__ import annotations

import os
import sqlite3
from dataclasses import dataclass, field
from typing import Any

_SELECT_PREFIXES = ("select", "with", "explain", "values", "pragma")


@dataclass
class ColumnInfo:
    name: str
    type: str
    pk: bool
    notnull: bool


@dataclass
class ForeignKey:
    from_column: str
    to_table: str
    to_column: str


@dataclass
class TableInfo:
    name: str
    columns: list[ColumnInfo] = field(default_factory=list)
    foreign_keys: list[ForeignKey] = field(default_factory=list)
    row_count: int = 0


class StoreBrowser:
    """Navigates one user DB + its ``.stemmadb`` sidecar, read-only."""

    def __init__(self, user_db: str, store: str | None = None):
        self.user_db = os.path.abspath(user_db)
        self.store = store or os.path.splitext(self.user_db)[0] + ".stemmadb"

    def _connect_user(self) -> sqlite3.Connection:
        return sqlite3.connect(f"file:{self.user_db}?mode=ro", uri=True)

    def _connect_store(self) -> sqlite3.Connection:
        conn = sqlite3.connect(f"file:{self.store}?mode=ro", uri=True)
        conn.execute(
            "ATTACH DATABASE ? AS src", (f"file:{self.user_db}?mode=ro",)
        )
        return conn

    # ---------- user data ----------

    def schema(self) -> list[TableInfo]:
        with self._connect_user() as conn:
            tables = [
                r[0]
                for r in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type='table' "
                    "AND name NOT LIKE 'sqlite_%' ORDER BY name"
                )
            ]
            out = []
            for t in tables:
                info = TableInfo(name=t)
                for cid, name, ctype, notnull, dflt, pk in conn.execute(
                    f'PRAGMA table_info("{t}")'
                ):
                    info.columns.append(
                        ColumnInfo(name=name, type=ctype, pk=bool(pk), notnull=bool(notnull))
                    )
                for row in conn.execute(f'PRAGMA foreign_key_list("{t}")'):
                    # id, seq, table, from, to, on_update, on_delete, match
                    info.foreign_keys.append(
                        ForeignKey(from_column=row[3], to_table=row[2], to_column=row[4] or "")
                    )
                info.row_count = conn.execute(f'SELECT count(*) FROM "{t}"').fetchone()[0]
                out.append(info)
            return out

    def rows(
        self, table: str, limit: int = 50, offset: int = 0
    ) -> dict[str, Any]:
        with self._connect_user() as conn:
            valid = {t.name for t in self.schema()}
            if table not in valid:
                raise ValueError(f"unknown table {table!r}")
            cur = conn.execute(
                f'SELECT rowid AS _rowid, * FROM "{table}" LIMIT ? OFFSET ?',
                (limit, offset),
            )
            columns = [d[0] for d in cur.description]
            rows = [[_display(v) for v in r] for r in cur.fetchall()]
            total = conn.execute(f'SELECT count(*) FROM "{table}"').fetchone()[0]
            return {"columns": columns, "rows": rows, "total": total, "offset": offset}

    # ---------- metadata / knowledge graph ----------

    def schema_graph(self) -> dict[str, Any]:
        """The schema layer of the knowledge graph: tables as nodes, declared
        foreign keys as edges. (The instance layer arrives with stemma-kg.)"""
        tables = self.schema()
        nodes = [
            {"id": t.name, "kind": "table", "rows": t.row_count,
             "columns": [c.name for c in t.columns]}
            for t in tables
        ]
        edges = [
            {"source": t.name, "target": fk.to_table,
             "label": f"{fk.from_column} → {fk.to_column or 'id'}"}
            for t in tables
            for fk in t.foreign_keys
        ]
        return {"layer": "schema", "nodes": nodes, "edges": edges}

    def store_meta(self) -> dict[str, Any]:
        if not os.path.exists(self.store):
            return {"exists": False}
        with self._connect_store() as conn:
            version = conn.execute("PRAGMA user_version").fetchone()[0]
            names = {
                r[0]
                for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
            }
            meta: dict[str, Any] = {
                "exists": True,
                "path": self.store,
                "schema_version": version,
                "size_bytes": os.path.getsize(self.store),
                "model_registry": [],
                "embed_queue": 0,
                "lexical": None,
            }
            if "model_registry" in names:
                cur = conn.execute("SELECT * FROM model_registry ORDER BY vector_table")
                cols = [d[0] for d in cur.description]
                meta["model_registry"] = [dict(zip(cols, r)) for r in cur.fetchall()]
            if "embed_queue" in names:
                meta["embed_queue"] = conn.execute(
                    "SELECT count(*) FROM embed_queue"
                ).fetchone()[0]
            if "lex_values" in names:
                values, tables, columns = conn.execute(
                    "SELECT count(*), count(DISTINCT src_table), "
                    "count(DISTINCT src_table || '.' || src_column) FROM lex_values"
                ).fetchone()
                meta["lexical"] = {"values": values, "tables": tables, "columns": columns}
            return meta

    # ---------- ad-hoc queries ----------

    def query(self, sql: str, limit: int = 200) -> dict[str, Any]:
        """Runs a read-only query against store (main) + user DB (src).

        Connections are mode=ro, so writes fail at the SQLite level too; the
        prefix check just produces a friendlier error earlier.
        """
        stripped = sql.strip().lower()
        if not stripped.startswith(_SELECT_PREFIXES):
            raise ValueError("read-only console: only SELECT/WITH/PRAGMA queries")
        conn = self._connect_store() if os.path.exists(self.store) else self._connect_user()
        try:
            cur = conn.execute(sql)
            columns = [d[0] for d in cur.description] if cur.description else []
            rows = [[_display(v) for v in r] for r in cur.fetchmany(limit)]
            truncated = cur.fetchone() is not None
            return {"columns": columns, "rows": rows, "truncated": truncated}
        finally:
            conn.close()


def _display(v: Any) -> Any:
    if isinstance(v, bytes):
        return f"<blob {len(v)}B>"
    if isinstance(v, str) and len(v) > 400:
        return v[:400] + "…"
    return v
