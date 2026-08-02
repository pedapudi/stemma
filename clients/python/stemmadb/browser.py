"""Read-only access to the two SQLite files of a stemma deployment.

Browsing is a storage-layer concern: the user database and the ``.stemmadb``
store are plain SQLite files, so navigation needs no server round-trip. Every
connection here is opened with ``mode=ro`` — this module cannot write.
"""

from __future__ import annotations

import json
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
                # max(rowid) is O(1) via the pk index; count(*) is a full scan
                # that big tables cannot afford on every schema fetch.
                info.row_count = conn.execute(
                    f'SELECT coalesce(max(rowid), 0) FROM "{t}"'
                ).fetchone()[0]
                out.append(info)
            return out

    def rows(
        self,
        table: str,
        limit: int = 50,
        after: int | None = None,
        q: str = "",
    ) -> dict[str, Any]:
        """Keyset-paginated rows: `after` is the last rowid of the previous
        page (OFFSET degrades linearly on big tables; keyset does not).
        `q` filters by substring across text columns, served by the store's
        trigram index when available."""
        with self._connect_user() as conn:
            valid = {t.name for t in self.schema()}
            if table not in valid:
                raise ValueError(f"unknown table {table!r}")
            params: list[Any] = []
            where = ["rowid > ?"] if after is not None else []
            if after is not None:
                params.append(after)
            if q:
                ids = self._filter_rowids(table, q, limit * 20)
                if ids is not None:
                    if not ids:
                        return {"columns": [], "rows": [], "has_more": False}
                    where.append(f"rowid IN ({','.join(str(i) for i in ids)})")
                else:  # no store index — LIKE scan over text columns
                    text_cols = [
                        r[1]
                        for r in conn.execute(f'PRAGMA table_info("{table}")')
                        if "TEXT" in (r[2] or "").upper() or "CHAR" in (r[2] or "").upper()
                    ]
                    if text_cols:
                        like = " OR ".join(f'"{c}" LIKE ?' for c in text_cols)
                        where.append(f"({like})")
                        params.extend([f"%{q}%"] * len(text_cols))
            sql = f'SELECT rowid AS _rowid, * FROM "{table}"'
            if where:
                sql += " WHERE " + " AND ".join(where)
            sql += " ORDER BY rowid LIMIT ?"
            params.append(limit + 1)
            cur = conn.execute(sql, params)
            columns = [d[0] for d in cur.description]
            rows = [[_display(v) for v in r] for r in cur.fetchall()]
            has_more = len(rows) > limit
            return {"columns": columns, "rows": rows[:limit], "has_more": has_more}

    def _filter_rowids(self, table: str, q: str, limit: int) -> list[int] | None:
        """Rowids matching a substring filter via the store's trigram index;
        None when the store or index is unavailable (caller falls back)."""
        if len(q) < 3 or not os.path.exists(self.store):
            return None
        with self._connect_store() as conn:
            names = {
                r[0]
                for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
            }
            if "lex_trigram" not in names:
                return None
            fts_q = '"' + q.replace('"', '""') + '"'
            try:
                return [
                    r[0]
                    for r in conn.execute(
                        "SELECT DISTINCT v.src_rowid FROM lex_trigram f "
                        "JOIN lex_values v ON v.id = f.rowid "
                        "WHERE lex_trigram MATCH ? AND v.src_table = ? "
                        "ORDER BY v.src_rowid LIMIT ?",
                        (fts_q, table, limit),
                    )
                ]
            except Exception:
                return None

    # ---------- metadata / knowledge graph ----------

    def schema_graph(self) -> dict[str, Any]:
        """Fallback graph when the compiled knowledge store is absent: tables
        as nodes, declared foreign keys as edges."""
        tables = self.schema()
        nodes = [
            {"key": f"table:{t.name}", "kind": "table", "label": t.name,
             "props": {"rows": t.row_count, "columns": [c.name for c in t.columns]}}
            for t in tables
        ]
        edges = [
            {"source": f"table:{t.name}", "target": f"table:{fk.to_table}",
             "kind": "fk", "label": f"{fk.from_column} → {fk.to_column or 'id'}",
             "props": {"method": "declared", "confidence": 1.0}}
            for t in tables
            for fk in t.foreign_keys
        ]
        return {"layer": "schema-only", "nodes": nodes, "edges": edges}

    def knowledge_graph(self) -> dict[str, Any]:
        """The compiled knowledge graph from the store (schema + discovered
        relations + profile layers), falling back to the declared schema when
        the store hasn't been compiled yet."""
        if not os.path.exists(self.store):
            return self.schema_graph()
        with self._connect_store() as conn:
            names = {
                r[0]
                for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
            }
            if "kg_nodes" not in names:
                return self.schema_graph()
            nodes = []
            key_by_id: dict[int, str] = {}
            for nid, key, kind, label, props in conn.execute(
                "SELECT id, key, kind, label, props FROM kg_nodes"
            ):
                key_by_id[nid] = key
                nodes.append({
                    "key": key, "kind": kind, "label": label,
                    "props": json.loads(props or "{}"),
                })
            if not nodes:
                return self.schema_graph()
            edges = [
                {
                    "source": key_by_id.get(src, ""),
                    "target": key_by_id.get(dst, ""),
                    "kind": kind, "label": label,
                    "props": json.loads(props or "{}"),
                }
                for src, dst, kind, label, props in conn.execute(
                    "SELECT src, dst, kind, label, props FROM kg_edges"
                )
            ]
            return {"layer": "compiled", "nodes": nodes, "edges": edges}

    def examples(self, limit: int = 6) -> list[str]:
        """Query suggestions mined from the knowledge graph: co-occurring
        characteristic terms (document corpora) and frequent values."""
        if not os.path.exists(self.store):
            return []
        out: list[str] = []
        with self._connect_store() as conn:
            names = {
                r[0]
                for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
            }
            if "kg_edges" in names:
                # Strongest term pairs make natural two-word queries.
                for a, b in conn.execute(
                    "SELECT n1.label, n2.label FROM kg_edges e "
                    "JOIN kg_nodes n1 ON n1.id = e.src "
                    "JOIN kg_nodes n2 ON n2.id = e.dst "
                    "WHERE e.kind = 'cooccurs' "
                    "ORDER BY json_extract(e.props, '$.docs') DESC LIMIT ?",
                    (max(2, limit // 2),),
                ):
                    out.append(f"{a} {b}")
                for (label,) in conn.execute(
                    "SELECT label FROM kg_nodes WHERE kind = 'value' "
                    "ORDER BY json_extract(props, '$.count') DESC LIMIT ?",
                    (limit,),
                ):
                    if len(label) > 2 and label not in out:
                        out.append(label)
        return out[:limit]

    def query_plan(self, sql: str) -> list[dict[str, Any]]:
        """SQLite's EXPLAIN QUERY PLAN as a depth-annotated tree, in source
        order."""
        stripped = sql.strip().lower()
        if not stripped.startswith(_SELECT_PREFIXES):
            raise ValueError("read-only console: only SELECT/WITH/PRAGMA queries")
        conn = self._connect_store() if os.path.exists(self.store) else self._connect_user()
        try:
            rows = conn.execute(f"EXPLAIN QUERY PLAN {sql}").fetchall()
        finally:
            conn.close()
        depth: dict[int, int] = {0: 0}
        out = []
        for node_id, parent, _notused, detail in rows:
            d = depth.get(parent, 0) + (1 if parent != 0 else 0)
            depth[node_id] = d
            out.append({"id": node_id, "parent": parent, "depth": d, "detail": detail})
        return out

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
                # v4 stores track item status; the panel number is the
                # pending backlog, not the lifetime ledger. Older stores
                # (no status column) fall back to the raw row count.
                cols = {r[1] for r in conn.execute("PRAGMA table_info(embed_queue)")}
                if "status" in cols:
                    meta["embed_queue"] = conn.execute(
                        "SELECT count(*) FROM embed_queue WHERE status = 'pending'"
                    ).fetchone()[0]
                else:
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
