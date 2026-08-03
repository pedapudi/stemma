# The legal corpus

One user database, `data/legal.db`, built by `build_legal_db.py`:

| table | rows | contents |
|---|---:|---|
| `regulations` | 57,523 | California Code of Regulations (Nemotron careg subset), one CCR section per row |
| `sections` | 35,173 | eCFR (Nemotron eCFR subset); one row is a *bundle* of consecutive CFR sections |

`load_vectors.py` stages the pre-computed careg embeddings for promotion into
the dense index.

## Derived citation tables — `mine_citations.py`

Legal text is relational in prose: every record's header names its own
canonical citation, and every body cites other sections. `mine_citations.py`
surfaces that structure as two ordinary tables inside `legal.db`, so stemma's
knowledge-graph compiler discovers the citation network as joins with no
KG-side changes:

- **`refs (id, table_name, row_id, ref UNIQUE)`** — the canonical citations a
  record IS. For `regulations` the header's `Cites:` line is taken verbatim
  (`2 CCR § 599.825`, appendix forms like `8 CCR § 1710 App. B` included).
  For `sections` every `§ N` heading in the record becomes a row
  (`40 CFR § 180.1`), because eCFR rows bundle several consecutive sections.
- **`citations (id, src_table, src_id, ref, resolved)`** — cross-references
  mined from body text, normalized into the same key space: explicit
  `N CCR § X` / `N CFR §|part X` forms, plus relative `§ X` / `Section X`
  forms attributed to the record's own title. Mining is conservative
  (precision over recall): statute citations (`Section 11346.2 of the
  Government Code`, `section 553 of title 5, United States Code`, enumerations
  ending in a code name) are rejected; verbose and title-only forms are left
  unmined. Known residue: bare statute references without a code name nearby
  (IRC sections in Title 26 text, `Section 107, Pub. L. 86-645`) can be
  mis-attributed to the record's own title — they land unresolved, so they do
  not corrupt the join, but they exist.

**Foreign keys.** SQLite requires the parent of a foreign key to be UNIQUE, so
`refs.ref` carries a UNIQUE constraint and `citations` could reference it
directly — but a third of mined citations point outside the corpus (titles
not in the subset, repealed sections, statute residue), and a mostly-violated
constraint is worse than none. Instead `citations.ref` keeps the raw
normalized target and `citations.resolved INTEGER REFERENCES refs(id)` is the
real, enforceable FK, NULL when the target is off-corpus
(`PRAGMA foreign_key_check` is clean by construction).

**Surrogate ids.** `refs` ids start above 10,000,000 and `citations` ids above
20,000,000. The corpus tables use dense `1..N` ids and stemma's
inclusion-dependency mining tests raw integer containment, so dense derived
ids would sit inside the corpus id spaces and manufacture spurious join
candidates. Disjoint ranges leave exactly the semantic joins discoverable:
the declared `citations.resolved → refs.id`, and the mined
`refs.row_id →? {regulations,sections}.id` / `citations.src_id →? …`
inclusions. (`row_id`/`src_id` are polymorphic — `table_name`/`src_table`
picks the parent — so the integer containment edge the compiler infers toward
the larger id space is real but partial; consumers see it marked
`method:"inferred"`.)

**What the pipeline does with it.** `stemma_kg::compile` discovers the
citation network unmodified: the declared `citations.resolved → refs.id` edge
(schema layer, confidence 1.0) plus inferred `citations.src_id →?
regulations.id` and `refs.row_id →? regulations.id` (containment 1.000).
Collective disambiguation then produces real coherence evidence — resolving
`promotional list eligibility 2 CCR § 240` boosts the regulation that *cites*
§ 240 with `regulations #477 ←src_id?— citations #20000604` on the winning
candidates, and `what cites 2 CCR § 240 …` links the citation row to the
cited regulation through the two-hop path
`citations #20000604 —resolved→ refs —row_id?→ regulations #1204`.

The script is idempotent: it drops and recreates only `refs` and `citations`,
never touches `regulations`/`sections`, and re-running converges to the same
state. No environment variables; path and flags only.

```sh
# migrate a corpus db (safe to re-run):
python3 eval/legal/mine_citations.py eval/legal/data/legal.db

# tests (tiny synthetic fixture, no corpus needed):
python3 eval/legal/test_mine_citations.py
```

**Applying to a live deployment:** stop the server first, then run the miner
with `--reindex-store` so the sidecar's lexical index (which the server skips
rebuilding when populated) is cleared and rebuilt over the new tables at the
next start:

```sh
python3 eval/legal/mine_citations.py eval/legal/data/legal.db \
    --reindex-store eval/legal/data/legal.stemmadb
```

`--reindex-store` drops the `lex_*` tables only; vectors, the knowledge graph
(which recompiles itself via fingerprints) and history are untouched.

Current corpus numbers (2026-08): 100% of regulations and 91.6% of sections
records parse an own ref (the residue is FAR-style parts whose headings carry
no `§`, and empty/reserved bodies); 265,640 refs; 237,499 citations mined, of
which 68.9% resolve into the corpus.
