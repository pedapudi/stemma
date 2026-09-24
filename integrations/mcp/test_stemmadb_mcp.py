#!/usr/bin/env python3
"""Tests for the MCP server's sql tool against a tiny synthetic database.
No stemma-server is needed: sql reads SQLite directly.

Usage (with the mcp package installed):
    python3 integrations/mcp/test_stemmadb_mcp.py
"""

import asyncio
import os
import sqlite3
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from stemmadb_mcp import build_server  # noqa: E402


class SqlToolTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.user = os.path.join(self.tmp.name, "docs.db")
        with sqlite3.connect(self.user) as c:
            c.execute("CREATE TABLE docs(id INTEGER PRIMARY KEY, body TEXT)")
            c.execute("INSERT INTO docs VALUES (1, ?)", ("x" * 1000 + "y" * 1000,))
        self.server = build_server("127.0.0.1:1", {"docs": self.user})

    def tearDown(self):
        self.tmp.cleanup()

    def sql(self, query):
        result = asyncio.run(
            self.server.call_tool("sql", {"query": query, "database": "docs"})
        )
        # FastMCP returns (content blocks, structured result); a dict-typed
        # tool's structured result is the dict itself.
        return result[1] if isinstance(result, tuple) else result

    def test_cells_are_clipped_at_the_documented_1500_chars(self):
        out = self.sql("SELECT body FROM docs")
        cell = out["rows"][0][0]
        self.assertTrue(cell.startswith("x" * 1000 + "y" * 500), cell[:80])
        self.assertIn("[+500 chars]", cell)


if __name__ == "__main__":
    unittest.main()
