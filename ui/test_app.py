#!/usr/bin/env python3
"""Tests for the console's SQL and history endpoints against a tiny
synthetic user database and store. No stemma-server is needed: these
endpoints read SQLite directly.

Usage (with fastapi and httpx installed, and clients/python importable):
    python3 ui/test_app.py
"""

import os
import sqlite3
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "clients", "python"))

from fastapi.testclient import TestClient  # noqa: E402

from app import create_app  # noqa: E402


class ConsoleSqlTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        user = os.path.join(self.tmp.name, "shop.db")
        with sqlite3.connect(user) as c:
            c.executescript(
                "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT);"
                "INSERT INTO items VALUES (1, 'kettle'), (2, 'teapot');"
            )
        store = os.path.join(self.tmp.name, "shop.stemmadb")
        with sqlite3.connect(store) as c:
            c.executescript(
                "CREATE TABLE query_log(id INTEGER PRIMARY KEY, query TEXT NOT NULL,"
                " asked_at TEXT NOT NULL, source TEXT NOT NULL DEFAULT '');"
                # The latest ask of 'kettle' came from the console; an older
                # one from mcp, which sorts after 'console' alphabetically.
                "INSERT INTO query_log (query, asked_at, source) VALUES"
                " ('kettle', '2026-01-01 10:00:00', 'mcp'),"
                " ('kettle', '2026-01-02 10:00:00', 'console'),"
                " ('teapot', '2026-01-01 09:00:00', 'agent');"
            )
        self.client = TestClient(create_app({"shop": user}, "127.0.0.1:1"))

    def tearDown(self):
        self.tmp.cleanup()

    def test_sql_on_an_unknown_database_is_404(self):
        r = self.client.post("/api/db/nope/sql", json={"sql": "SELECT 1"})
        self.assertEqual(r.status_code, 404, r.text)

    def test_sql_select_returns_rows_and_plan(self):
        r = self.client.post(
            "/api/db/shop/sql", json={"sql": "SELECT name FROM src.items ORDER BY id"}
        )
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(r.json()["rows"], [["kettle"], ["teapot"]])
        self.assertTrue(r.json()["plan"])

    def test_sql_accepts_explain_statements(self):
        for sql in (
            "EXPLAIN QUERY PLAN SELECT * FROM src.items",
            "EXPLAIN SELECT * FROM src.items",
        ):
            r = self.client.post("/api/db/shop/sql", json={"sql": sql})
            self.assertEqual(r.status_code, 200, f"{sql}: {r.text}")
            self.assertTrue(r.json()["rows"], sql)

    def test_history_reports_the_latest_source(self):
        r = self.client.get("/api/db/shop/history")
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(
            r.json()["tagged"],
            [
                {"query": "kettle", "source": "console"},
                {"query": "teapot", "source": "agent"},
            ],
        )


if __name__ == "__main__":
    unittest.main()
