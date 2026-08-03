#!/usr/bin/env python3
"""Mines citation structure out of the legal corpus into relational tables.

The corpus is two document tables (regulations = California Code of
Regulations, sections = eCFR). Each document *is* a citable unit — its header
carries its own canonical citation — and each body *cites* other units in
prose. This script surfaces both facts as tables so stemma's knowledge graph
can compile the citation network as ordinary joins:

  refs       — the canonical citations a record IS. `ref` is the join key
               ("2 CCR § 599.825", "40 CFR § 180.1"); (table_name, row_id)
               points back at the corpus row. A regulations row is a single
               CCR section (one refs row, from its "Cites:" header line); an
               eCFR sections row is a *bundle* of consecutive CFR sections
               (one refs row per "§ N" heading it contains), so citations
               into the middle of a bundle still join.
  citations  — one row per cross-reference mined from body text, normalized
               into the same canonical form. `ref` is the raw normalized
               target; `resolved` is a REAL foreign key to refs(id), NULL
               when the target is outside the corpus.

Foreign-key design: SQLite requires the parent side of a foreign key to be a
PRIMARY KEY or UNIQUE column. `refs.ref` is UNIQUE, so `citations.ref` COULD
reference it directly — but many mined citations point at sections the corpus
does not contain (repealed CCR sections, CFR titles outside the subset), and
a constraint that is mostly violated is worse than none. Instead `citations`
keeps the raw target in `ref` and carries `resolved INTEGER REFERENCES
refs(id)`, populated only when the target exists; NULL child values satisfy
SQLite's FK semantics, so the declared constraint is honest and enforceable.

Surrogate ids: refs ids start at 10,000,001 and citations ids at 20,000,001.
The corpus tables use dense 1..N ids, and stemma's inclusion-dependency
mining tests raw set containment between integer columns — dense surrogate
ranges on the derived tables would be accidentally contained in (and contain)
the corpus id spaces, manufacturing spurious join candidates. Disjoint ranges
keep the discovered joins the semantic ones: refs.row_id and citations.src_id
against the corpus tables, and the declared citations.resolved -> refs.id.

Idempotent: drops and recreates only its own two tables; regulations and
sections are never written. Re-running converges to the same state.

Usage:
    python3 eval/legal/mine_citations.py path/to/legal.db
    python3 eval/legal/mine_citations.py path/to/legal.db --reindex-store path/to/legal.stemmadb

--reindex-store clears the sidecar's lexical index tables (only those) so the
next stemma-server start re-ingests the new tables; the server otherwise
skips ingest when an index is already populated. Run it only with the server
stopped. Configuration is by flags only — no environment variables.
"""

import argparse
import re
import sqlite3

REFS_ID_BASE = 10_000_000
CITATIONS_ID_BASE = 20_000_000

SCHEMA = """
CREATE TABLE refs (
    id         INTEGER PRIMARY KEY,
    table_name TEXT NOT NULL,
    row_id     INTEGER NOT NULL,
    ref        TEXT NOT NULL UNIQUE
);
CREATE INDEX refs_row ON refs(table_name, row_id);

CREATE TABLE citations (
    id        INTEGER PRIMARY KEY,
    src_table TEXT NOT NULL,
    src_id    INTEGER NOT NULL,
    ref       TEXT NOT NULL,
    resolved  INTEGER REFERENCES refs(id),
    UNIQUE (src_table, src_id, ref)
);
CREATE INDEX citations_ref ON citations(ref);
CREATE INDEX citations_resolved ON citations(resolved);
CREATE INDEX citations_src ON citations(src_table, src_id);
"""

# ---- own-ref (header) parsing ------------------------------------------------

# regulations: the header ends with a "Cites: 2 CCR § 599.825" line naming the
# record's own citation. The line is already canonical (appendix and plate
# records read "14 CCR § 1670 App. B", "8 CCR PLATE A-1"); it is taken
# verbatim so coverage is not lost to exotic-but-well-formed cites.
RE_CITES_LINE = re.compile(r"^Cites:\s*(.+?)\s*$", re.M)

# sections: the header (before the "---" separator) names the CFR title; the
# body carries one "§ 180.1   Heading." line per bundled section. Both em dash
# and plain hyphen occur after the title number ("Title 21-Food and Drugs").
RE_CFR_TITLE = re.compile(r"^Title (\d+)\s*[—–-]", re.M)
RE_CFR_HEADING = re.compile(r"^§§?\s*([0-9][^\s]*)\s{2,}\S", re.M)


def parse_refs(table, text):
    """(list of canonical own refs, body text). The list is empty when the
    record defies the format (odd accounting-style parts, empty bodies)."""
    if table == "regulations":
        m = RE_CITES_LINE.search(text[:2000])
        if not m:
            return [], text
        return [m.group(1)], text[m.end():]
    head, sep, body = text.partition("\n---\n")
    if not sep:
        return [], text
    body = body.lstrip("\n")
    tm = RE_CFR_TITLE.search(head)
    if not tm:
        return [], body
    refs = []
    for hm in RE_CFR_HEADING.finditer(body):
        num = clean_num(hm.group(1))
        if num:
            ref = f"{tm.group(1)} CFR § {num}"
            if ref not in refs:
                refs.append(ref)
    return refs, body


# ---- cross-reference (body) mining -------------------------------------------
#
# Conservative by construction: explicit code-qualified forms first, then
# relative "§ X" / "Section X" forms attributed to the record's own title,
# guarded against the statute citations legal prose is full of ("Section
# 11346.2 of the Government Code", "section 553 of title 5, United States
# Code"). Precision over recall: verbose forms ("Title 2, California Code of
# Regulations, section 599.825") and title-only references are left unmined.

RE_EXPLICIT_CCR = re.compile(
    r"\b(\d{1,2})\s+C\.?C\.?R\.?\s*(?:§§?|[Ss]ec(?:tion)?s?\.?)\s*([0-9][\w.\-]*)"
)
RE_EXPLICIT_CFR = re.compile(
    r"\b(\d{1,2})\s+C\.?F\.?R\.?,?\s*"
    r"(?:(?P<part>[Pp]arts?)\s+|§§?\s*|[Ss]ec(?:tion)?s?\.?\s*)?"
    r"([0-9][\w.\-]*)"
)
RE_RELATIVE = re.compile(r"(?:§§?|\b[Ss]ections?|\b[Ss]ecs?\.)\s*([0-9][\w.\-]*)")

# A relative section number preceded by a code/act/title name is a statute or
# cross-code citation, not a reference into this record's own title.
RE_PRE_REJECT = re.compile(
    r"(?:Code|Act|U\.?\s?S\.?\s?C\.?|Stats?\.|Statutes|C\.?C\.?R\.?|C\.?F\.?R\.?"
    r"|[Tt]itle\s+\d{1,3})\s*,?\s*$"
)
# "... of the Government Code", "... of title 44": the target lives in another
# code entirely. "of this chapter/part/title" and friends stay internal. The
# enumeration tail makes "Sections 8263 and 8264 of the Education Code" reject
# its first member too; ", Labor Code" is the suffixed statute form.
RE_POST_REJECT = re.compile(
    r"\s*(?:\([^()\s]{1,15}\)\s*)*"
    r"(?:(?:[,;]\s*(?:and\s+|or\s+)?|\s+(?:and|or|through|to)\s+)(?:[Ss]ections?\s+|§§?\s*)?"
    r"[0-9][\w.\-]*\s*(?:\([^()\s]{1,15}\)\s*)*)*"
    r"(?:,?\s*of\s+(?!(?:this|these)\b)"
    r"|,\s+(?=[A-Z][a-z]+(?:\s+[A-Za-z&]+){0,5}\s+Code\b))"
)

RE_NUM_OK = re.compile(r"[0-9][0-9A-Za-z]*(?:[.\-][0-9A-Za-z.]+)*")


def clean_num(tok):
    """Normalizes a section-number token; None when it is not one."""
    tok = tok.rstrip(".,;:)]")
    if "-" in tok:
        # "599.825-599.830" is a range (take its start); "24360-1" is a
        # dashed CCR section number (keep whole).
        a, _, b = tok.partition("-")
        if "." in a and "." in b:
            tok = a
    if len(tok) > 20 or not RE_NUM_OK.fullmatch(tok):
        return None
    return tok


def mine_body(table, own_refs, own_title, body):
    """Canonical cross-reference targets mined from one record's body.
    `own_refs` (the record's own canonical citations) are suppressed: a
    bundled record's section headings and self-references are containment,
    not citation."""
    found = set()
    spent = []  # spans consumed by explicit matches

    for m in RE_EXPLICIT_CCR.finditer(body):
        num = clean_num(m.group(2))
        if num:
            found.add(f"{m.group(1)} CCR § {num}")
            spent.append(m.span())
    for m in RE_EXPLICIT_CFR.finditer(body):
        num = clean_num(m.group(3))
        if not num:
            continue
        if m.group("part"):
            found.add(f"{m.group(1)} CFR part {num}")
        else:
            found.add(f"{m.group(1)} CFR § {num}")
        spent.append(m.span())

    if own_title is not None:
        code = "CCR" if table == "regulations" else "CFR"
        for m in RE_RELATIVE.finditer(body):
            if any(s < m.end() and m.start() < e for s, e in spent):
                continue  # inside an explicit match
            if RE_PRE_REJECT.search(body[max(0, m.start() - 40):m.start()]):
                continue
            if RE_POST_REJECT.match(body[m.end():m.end() + 60]):
                continue
            num = clean_num(m.group(1))
            if num:
                found.add(f"{own_title} {code} § {num}")

    found.difference_update(own_refs)
    return found


RE_OWN_TITLE = re.compile(r"^(\d{1,3}) (?:CCR|CFR)\b")


def own_title_of(refs):
    if not refs:
        return None
    m = RE_OWN_TITLE.match(refs[0])
    return m.group(1) if m else None


# ---- driver ------------------------------------------------------------------

CORPUS_TABLES = ("regulations", "sections")


def mine(db_path, quiet=False):
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA foreign_keys = ON")
    for t in CORPUS_TABLES:
        n = conn.execute(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?", (t,)
        ).fetchone()[0]
        if n == 0:
            raise SystemExit(f"{db_path}: no {t} table — not a legal corpus db")

    # Idempotence: these two tables are this script's whole footprint.
    conn.executescript(
        "DROP TABLE IF EXISTS citations;\nDROP TABLE IF EXISTS refs;\n" + SCHEMA
    )

    stats = {}
    own = {}  # (table, row_id) -> own refs, for self-citation suppression
    ref_rows = []
    seen_refs = set()
    dupes = 0
    for table in CORPUS_TABLES:
        total = parsed = kept = 0
        for row_id, text in conn.execute(f"SELECT id, text FROM {table}"):
            total += 1
            refs, _ = parse_refs(table, text)
            if not refs:
                continue
            own[(table, row_id)] = set(refs)
            parsed += 1
            for ref in refs:
                if ref in seen_refs:
                    dupes += 1  # corpus duplicate; first row keeps the key
                    continue
                seen_refs.add(ref)
                kept += 1
                ref_rows.append((REFS_ID_BASE + len(ref_rows) + 1, table, row_id, ref))
        stats[table] = {"rows": total, "own_ref": parsed, "refs": kept}
    conn.executemany("INSERT INTO refs VALUES (?, ?, ?, ?)", ref_rows)

    cit_id = CITATIONS_ID_BASE
    for table in CORPUS_TABLES:
        mined = 0
        batch = []
        for row_id, text in conn.execute(f"SELECT id, text FROM {table}"):
            own_refs = own.get((table, row_id), set())
            _, body = parse_refs(table, text)
            targets = mine_body(table, own_refs, own_title_of(sorted(own_refs)), body)
            for ref in sorted(targets):
                cit_id += 1
                batch.append((cit_id, table, row_id, ref))
                mined += 1
        conn.executemany(
            "INSERT INTO citations (id, src_table, src_id, ref) VALUES (?, ?, ?, ?)",
            batch,
        )
        stats[table]["citations"] = mined

    conn.execute(
        "UPDATE citations SET resolved = (SELECT r.id FROM refs r WHERE r.ref = citations.ref)"
    )
    resolved = conn.execute(
        "SELECT count(*) FROM citations WHERE resolved IS NOT NULL"
    ).fetchone()[0]
    violations = conn.execute("PRAGMA foreign_key_check(citations)").fetchall()
    if violations:
        raise SystemExit(f"foreign_key_check failed: {violations[:5]}")
    conn.commit()

    total_cit = sum(stats[t]["citations"] for t in CORPUS_TABLES)
    if not quiet:
        for t in CORPUS_TABLES:
            s = stats[t]
            print(
                f"{t}: {s['own_ref']}/{s['rows']} records with a parsed own ref "
                f"({100.0 * s['own_ref'] / s['rows']:.1f}%), "
                f"{s['refs']} refs, {s['citations']} citations mined"
            )
        print(f"duplicate own refs skipped: {dupes}")
        if total_cit:
            print(
                f"citations resolved into the corpus: {resolved}/{total_cit} "
                f"({100.0 * resolved / total_cit:.1f}%)"
            )
    conn.close()
    stats["resolved"] = resolved
    stats["dupes"] = dupes
    return stats


def reindex_store(store_path, quiet=False):
    """Clears the sidecar's lexical index so the next server start rebuilds
    it over the new tables. Touches lex_* only; vectors, knowledge graph and
    history are untouched (the graph recompiles itself via fingerprints)."""
    conn = sqlite3.connect(store_path)
    for t in ("lex_vocab", "lex_fts", "lex_trigram", "lex_values"):
        conn.execute(f"DROP TABLE IF EXISTS {t}")
    conn.commit()
    conn.close()
    if not quiet:
        print(f"{store_path}: lexical index cleared; next server start re-ingests")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("db", help="path to legal.db (the user database)")
    ap.add_argument(
        "--reindex-store",
        metavar="STEMMADB",
        help="also clear the lexical index of this .stemmadb sidecar "
        "(server must be stopped)",
    )
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()
    mine(args.db, quiet=args.quiet)
    if args.reindex_store:
        reindex_store(args.reindex_store, quiet=args.quiet)


if __name__ == "__main__":
    main()
