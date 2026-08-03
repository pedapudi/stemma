#!/usr/bin/env python3
"""Tests for mine_citations.py against a tiny synthetic corpus.

Usage:
    python3 eval/legal/test_mine_citations.py
"""

import os
import sqlite3
import tempfile
import unittest

import mine_citations as mc

REG = """California Code of Regulations

Title {title}. Testing
  Division 1. Fixtures

Cites: {cite}

§ {sec}. {heading}.

{body}"""

SEC = """Source:
Code of Federal Regulations
Title {title}{dash}Testing--Volume 1
CHAPTER I—FIXTURES
PART {part}—GENERAL

---

§ {sec}   {heading}.

{body}"""


def build_fixture(path):
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE regulations (
            id INTEGER PRIMARY KEY, uuid TEXT NOT NULL UNIQUE,
            text TEXT NOT NULL, license TEXT NOT NULL, category TEXT NOT NULL);
        CREATE TABLE sections (
            id INTEGER PRIMARY KEY, uuid TEXT NOT NULL UNIQUE,
            text TEXT NOT NULL, license TEXT NOT NULL, category TEXT NOT NULL);
        """
    )
    regs = [
        # 1: cites a sibling in its own title (relative form) and a federal part.
        REG.format(
            title=2, cite="2 CCR § 599.825", sec="599.825",
            heading="Resignation from State Service",
            body="An employee may resign as provided in Section 599.826. "
            "Records follow 29 CFR part 1602. See also § 599.825.",
        ),
        # 2: explicit CCR cite plus statute refs that must NOT be mined.
        REG.format(
            title=2, cite="2 CCR § 599.826", sec="599.826",
            heading="Withdrawal of Resignation",
            body="Withdrawal is governed by 2 CCR § 599.825 and Section 19996.1 "
            "of the Government Code. Labor Code Section 6404.5 does not apply. "
            "Sections 8263 and 8264 of the Education Code are unrelated. "
            "See 29 CFR 1602.14 for federal retention.",
        ),
        # 3: appendix-style Cites line, taken verbatim.
        REG.format(
            title=8, cite="8 CCR § 1710 App. A", sec="1710",
            heading="Appendix",
            body="Plates referenced in Section 1710 apply.",
        ),
        # 4: duplicate own ref (corpus artifact) — refs keeps the first row.
        REG.format(
            title=2, cite="2 CCR § 599.825", sec="599.825",
            heading="Resignation from State Service",
            body="Duplicate record.",
        ),
    ]
    secs = [
        # 1: em-dash title; relative § ref and a USC ref that must not be mined.
        SEC.format(
            title=29, dash="—", part=1602, sec="1602.14",
            heading="Preservation of records",
            body="Reports required by § 1602.7 shall be retained. Authority: "
            "section 709 of title 42, United States Code. See § 1602.14.",
        ),
        # 2: hyphen title variant; explicit CFR cite into another title; a
        # BUNDLED record — the second "§ 1602.9" heading is containment, not
        # citation, and must become a refs row rather than a citations row.
        SEC.format(
            title=29, dash="-", part=1602, sec="1602.7",
            heading="Employer information report",
            body="Filed as required under 42 CFR § 3.102 and part 1602 of this "
            "chapter. Section 1602.14 of this part governs retention.\n\n"
            "§ 1602.9   Bundled sibling section.\n\n"
            "Sibling body text mentioning § 1602.7 stays internal.",
        ),
    ]
    for i, text in enumerate(regs, 1):
        conn.execute(
            "INSERT INTO regulations VALUES (?, ?, ?, 'CC-BY', 'careg')",
            (i, f"reg-{i}", text),
        )
    for i, text in enumerate(secs, 1):
        conn.execute(
            "INSERT INTO sections VALUES (?, ?, ?, 'PD', 'ecfr')",
            (i, f"sec-{i}", text),
        )
    conn.commit()
    conn.close()


class MineCitationsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory(prefix="mine-citations-")
        cls.db = os.path.join(cls.dir.name, "fixture.db")
        build_fixture(cls.db)
        cls.stats = mc.mine(cls.db, quiet=True)
        cls.conn = sqlite3.connect(cls.db)

    @classmethod
    def tearDownClass(cls):
        cls.conn.close()
        cls.dir.cleanup()

    def refs(self):
        return dict(
            self.conn.execute("SELECT ref, row_id FROM refs WHERE table_name='regulations'")
        ), dict(
            self.conn.execute("SELECT ref, row_id FROM refs WHERE table_name='sections'")
        )

    def cited(self, src_table, src_id):
        return {
            r
            for (r,) in self.conn.execute(
                "SELECT ref FROM citations WHERE src_table=? AND src_id=?",
                (src_table, src_id),
            )
        }

    def test_own_refs_parse_including_appendix_and_hyphen_title(self):
        regs, secs = self.refs()
        self.assertEqual(
            regs,
            {"2 CCR § 599.825": 1, "2 CCR § 599.826": 2, "8 CCR § 1710 App. A": 3},
        )
        # Bundled record 2 contributes BOTH its headings as own refs.
        self.assertEqual(
            secs,
            {"29 CFR § 1602.14": 1, "29 CFR § 1602.7": 2, "29 CFR § 1602.9": 2},
        )

    def test_duplicate_own_ref_kept_once_first_row_wins(self):
        self.assertEqual(self.stats["dupes"], 1)
        row = self.conn.execute(
            "SELECT row_id FROM refs WHERE ref = '2 CCR § 599.825'"
        ).fetchone()
        self.assertEqual(row, (1,))

    def test_relative_and_explicit_citations_normalize_to_one_key_space(self):
        self.assertEqual(
            self.cited("regulations", 1),
            {"2 CCR § 599.826", "29 CFR part 1602"},  # self-cite § 599.825 dropped
        )
        self.assertIn("2 CCR § 599.825", self.cited("regulations", 2))
        self.assertIn("29 CFR § 1602.14", self.cited("regulations", 2))
        self.assertEqual(
            self.cited("sections", 2), {"42 CFR § 3.102", "29 CFR § 1602.14"}
        )

    def test_statute_and_usc_references_are_not_mined(self):
        all_refs = {r for (r,) in self.conn.execute("SELECT ref FROM citations")}
        for bogus in (
            "2 CCR § 19996.1",   # Government Code
            "2 CCR § 6404.5",    # Labor Code, name-first form
            "2 CCR § 8263",      # enumeration ending in Education Code
            "2 CCR § 8264",
            "29 CFR § 709",      # United States Code
        ):
            self.assertNotIn(bogus, all_refs)

    def test_self_citations_are_dropped(self):
        for table, row_id in (("regulations", 1), ("sections", 1)):
            ref = self.conn.execute(
                "SELECT ref FROM refs WHERE table_name=? AND row_id=?", (table, row_id)
            ).fetchone()[0]
            self.assertNotIn(ref, self.cited(table, row_id))

    def test_bundled_sibling_headings_are_containment_not_citation(self):
        # Record sections#2 bundles § 1602.7 and § 1602.9; neither its own
        # headings nor the sibling's internal back-reference are citations.
        self.assertEqual(
            self.cited("sections", 2), {"42 CFR § 3.102", "29 CFR § 1602.14"}
        )

    def test_resolved_is_a_valid_foreign_key_and_null_off_corpus(self):
        self.assertEqual(
            self.conn.execute("PRAGMA foreign_key_check(citations)").fetchall(), []
        )
        resolved = self.conn.execute(
            "SELECT count(*) FROM citations WHERE resolved IS NOT NULL"
        ).fetchone()[0]
        self.assertEqual(resolved, self.stats["resolved"])
        # off-corpus targets stay unresolved
        off = self.conn.execute(
            "SELECT resolved FROM citations WHERE ref = '42 CFR § 3.102'"
        ).fetchone()
        self.assertEqual(off, (None,))
        # in-corpus targets resolve to the right refs row
        row = self.conn.execute(
            "SELECT r.table_name, r.row_id FROM citations c JOIN refs r ON r.id = c.resolved "
            "WHERE c.src_table='sections' AND c.src_id=2 AND c.ref='29 CFR § 1602.14'"
        ).fetchone()
        self.assertEqual(row, ("sections", 1))

    def test_surrogate_ids_are_disjoint_from_corpus_id_space(self):
        lo = self.conn.execute("SELECT min(id) FROM refs").fetchone()[0]
        self.assertGreater(lo, mc.REFS_ID_BASE)
        lo = self.conn.execute("SELECT min(id) FROM citations").fetchone()[0]
        self.assertGreater(lo, mc.CITATIONS_ID_BASE)

    def test_rerun_is_idempotent_and_leaves_corpus_untouched(self):
        before = self.conn.execute(
            "SELECT count(*), sum(id) FROM regulations"
        ).fetchone()
        dump1 = list(
            self.conn.execute("SELECT * FROM refs ORDER BY id")
        ), list(self.conn.execute("SELECT * FROM citations ORDER BY id"))
        mc.mine(self.db, quiet=True)
        conn2 = sqlite3.connect(self.db)
        dump2 = list(
            conn2.execute("SELECT * FROM refs ORDER BY id")
        ), list(conn2.execute("SELECT * FROM citations ORDER BY id"))
        after = conn2.execute("SELECT count(*), sum(id) FROM regulations").fetchone()
        conn2.close()
        self.assertEqual(dump1, dump2)
        self.assertEqual(before, after)

    def test_range_and_dashed_section_numbers(self):
        self.assertEqual(mc.clean_num("599.825-599.830"), "599.825")  # range → start
        self.assertEqual(mc.clean_num("24360-1"), "24360-1")  # dashed CCR number
        self.assertEqual(mc.clean_num("240.13d-1"), "240.13d-1")
        self.assertEqual(mc.clean_num("1602.14."), "1602.14")
        self.assertIsNone(mc.clean_num("."))


if __name__ == "__main__":
    unittest.main()
