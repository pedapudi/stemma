#!/usr/bin/env python3
"""Generates the synthetic legal evaluation dataset (legal-synth-v1).

Reverse generation per docs/design/07-eval-harness.md: sample a record from
the legal corpus, have the LM write an oblique question about it, then verify
tier membership MECHANICALLY against the store's lexical index. The gold
rowid is known by construction (we sampled it); the tier label is never
trusted from generation — a candidate question that fails its tier's check is
rejected and regenerated, and every record carries the verification evidence
in its provenance.

Tier checks (all read-only queries against legal.stemmadb):

  anchor   the question must contain >= 1 content phrase that exact/trigram-hits
       the gold row's lex entries, and the rest must be paraphrase (longest
       contiguous phrase hit bounded).
  paraphrase   NO content token of the question (resolver stopword list removed) may
       produce an exact OR trigram hit on the gold row's lex_values entries;
       colloquial register enforced by a banned-token list.
  cross-record   two gold rows, one per table (state + federal, same topic); both rows
       pass the paraphrase-style no-anchor check, or the pair is explicitly mixed
       (one anchored, one not — recorded which).
  join   legal has no join tables yet (citation mining is in flight), so the
       legal set carries an empty join slot in its header and relational
       queries are generated against the mini corpus instead, where the gold
       tuple is verified by executing the join path.
  absent  the defining topic phrase must have ZERO exact and ZERO trigram hits
       corpus-wide, and the topic comes from a domain the corpus plainly
       lacks (argument recorded per question).

Configuration comes from the repo config file and flags only — never from
environment variables. The LM endpoint is read from `eval.lm` in the config,
falling back to `console.lm`.

Usage:
    python3 eval/legal/gen_eval_set.py \
        --config config.json --n-per-tier 50 --n-nil 25 --seed 11 \
        --out eval/datasets/legal-synth-v1.jsonl

Databases are opened strictly read-only (file:...?mode=ro); this script
never writes anything except the output JSONL files and the review HTML.
"""

import argparse
import concurrent.futures
import html
import itertools
import json
import os
import random
import re
import sqlite3
import sys
import threading
import time
import urllib.error
import urllib.request

GENERATOR = "eval/legal/gen_eval_set.py v1"

# The resolver's stopword list, mirrored from crates/stemma-resolve/src/lib.rs
# (STOPWORDS). "Content token" everywhere below means: not in this list.
STOPWORDS = {
    "a", "an", "and", "are", "at", "by", "did", "do", "does", "for", "from",
    "how", "in", "is", "it", "of", "on", "or", "s", "that", "the", "to",
    "was", "were", "what", "when", "where", "which", "who", "with",
}

# Colloquial-register gate: none of these may appear in a generated question.
REGISTER_BANNED = {
    "pursuant", "herein", "hereof", "thereof", "thereto", "promulgate",
    "promulgated", "aforementioned", "shall", "subsection", "ccr", "cfr",
}
REGISTER_BANNED_SUBSTR = ["§"]  # the section sign

# Words too generic to serve as an cross-record pairing term or an anchor anchor nucleus.
BOILERPLATE = {
    "california", "regulation", "regulations", "title", "division", "chapter",
    "article", "section", "sections", "subchapter", "subdivision", "cites",
    "refs", "annos", "source", "federal", "code", "pursuant", "shall",
    "means", "meaning", "person", "persons", "department", "commission",
    "board", "agency", "administrator", "secretary", "director", "required",
    "requirements", "requirement", "provisions", "provision", "following",
    "general", "definitions", "purposes", "paragraph", "applicable",
    "accordance", "chapter", "volume", "amended", "state", "states",
    "united", "government",
}

# absent topic candidates: (defining phrase, absence argument). Each phrase is
# verified corpus-wide (0 exact-phrase hits AND 0 trigram hits) before use;
# candidates that hit anything are discarded, so this list only *proposes*.
ABSENT_TOPICS = [
    ("parking meter", "Metered street parking is municipal code, not CCR or CFR."),
    ("homeowners association", "HOA governance is the Davis-Stirling Act (Civil Code), a statute — not regulation."),
    ("library card", "Public library membership is a local/county service policy, not state or federal regulation."),
    ("garage sale", "Yard/garage sales are municipal permitting at most; neither corpus regulates them."),
    ("lemonade stand", "A children's sidewalk stand is (at most) local ordinance territory; absent from both corpora."),
    ("wedding venue", "Event-venue booking is private contract; not a regulated activity in either corpus."),
    ("dog park", "Dog parks are municipal parks-and-rec rules, not state/federal regulation."),
    ("birthday party", "Private social events are not regulated subject matter in either corpus."),
    ("school detention", "School discipline practice at this level is district policy, not CCR/CFR text."),
    ("ice cream truck", "Mobile vending of this kind is municipal licensing; not in CCR or eCFR subsets."),
    ("carpool lane", "HOV lane rules live in the Vehicle Code (statute) and MUTCD signage, not these corpora."),
    ("bike lane", "Bikeway design is local public-works and statute, not CCR/CFR regulation text."),
    ("pothole repair", "Street maintenance is municipal public works, not regulation."),
    ("movie theater", "Cinema operation per se is not a regulated category in either corpus."),
    ("gym membership", "Health-studio contracts are Civil Code statute, not regulation."),
    ("food court", "Retail food facilities are the California Retail Food Code — Health & Safety Code statute, not CCR."),
    ("taco truck", "Mobile food vending is retail food code (statute) plus municipal permits, not CCR/CFR."),
    ("book club", "Private associations are not regulated subject matter."),
    ("roommate agreement", "Residential co-tenancy contracts are private/civil-code matter, not regulation."),
    ("bus stop bench", "Street furniture is municipal franchise territory, not state or federal regulation."),
    ("soccer league", "Youth/adult recreational sports leagues are private organizations; absent from both corpora."),
    ("skate park", "Skate parks are municipal recreation facilities, not regulated by CCR/CFR."),
    ("night market", "Temporary retail food events are retail food code (statute) and local permits, not CCR."),
    ("street performer", "Busking is municipal ordinance, not state or federal regulation."),
    ("coffee shop wifi", "Retail amenity policy is private business practice; not regulation."),
    ("baby shower", "Private social events are not regulated subject matter in either corpus."),
    ("laundromat token", "Coin-laundry operations at this level are not a CCR/CFR subject."),
    ("apartment doorman", "Building staffing is a private employment/lease matter, not regulation text."),
    ("karaoke bar", "Entertainment venues per se are ABC statute + municipal license territory, not these corpora."),
    ("holiday parade", "Parade permits are municipal special-event ordinances, not CCR/CFR."),
    ("neighborhood watch", "Community policing programs are municipal/civic, not regulation."),
    ("flea market", "Swap meets at this level are local licensing, not CCR/CFR regulation text."),
    ("vending machine snack", "Snack-vending placement is not a regulated topic in either corpus subset."),
    ("school bake sale", "Bake sales are district/PTA policy, not CCR or CFR text."),
    ("city council meeting", "Municipal meeting procedure is the Brown Act (statute), not regulation."),
    ("block party", "Street-closure social events are municipal special-event permits, not CCR/CFR."),
    ("school talent show", "School extracurricular events are district policy, not regulation text."),
    ("trick or treating", "Halloween customs are (at most) municipal curfew territory; absent from both corpora."),
    ("yoga studio", "Fitness studios per se are not a regulated category in either corpus subset."),
    ("poker night", "Private social gambling at this level is Penal Code statute, not CCR/CFR."),
    ("car wash fundraiser", "Charity car washes are municipal storm-water/permit territory at most; absent here."),
    ("prom night", "School dances are district/school policy, not regulation."),
    ("puppy playdate", "Informal pet socializing is not regulated subject matter."),
    ("open mic night", "Venue programming is private business practice, not regulation."),
    ("neighborhood potluck", "Private shared meals are not a regulated activity in either corpus."),
]


# ---------------------------------------------------------------- utilities

def norm(s):
    return s.lower()


def tokenize(s):
    """Approximates the store's unicode61 tokenizer closely enough for
    verification: lowercase alphanumeric runs."""
    return re.findall(r"[a-z0-9]+", s.lower())


def content_tokens(s):
    return [t for t in tokenize(s) if t not in STOPWORDS]


def ro_connect(path):
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    conn.execute("PRAGMA query_only = ON")
    return conn


class ThreadLocal:
    """One lazily-built instance of `factory` per worker thread. SQLite
    connections are not shareable across threads; everything else about a
    row attempt is."""

    def __init__(self, factory):
        self._factory = factory
        self._tl = threading.local()

    def __call__(self):
        v = getattr(self._tl, "v", None)
        if v is None:
            v = self._tl.v = self._factory()
        return v


def parallel_collect(draw, attempt, n, workers):
    """Rows are independent; only the feedback loop inside one row is
    serial. Draw items on the caller's thread (streams stay deterministic),
    run `attempt(item)` across `workers` threads in waves, and yield results
    in draw order so acceptance order matches the serial generator's. The
    caller merges stats and stops consuming once it has `n` — a wave may
    overshoot by design, which costs one wave of LM calls at most.

    `attempt` must be self-contained: no shared mutable state, stats
    returned as deltas, never mutated in place."""
    if workers <= 1:
        while True:
            item = draw()
            if item is None:
                return
            yield attempt(item)
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as ex:
        done = 0
        while done < n:
            wave = []
            while len(wave) < workers:
                item = draw()
                if item is None:
                    break
                wave.append(item)
            if not wave:
                return
            for res in ex.map(attempt, wave):
                yield res
                if res[0] is not None:
                    done += 1


def fts_phrase(tokens):
    """A quoted FTS5 phrase query from a token list."""
    return '"' + " ".join(t.replace('"', "") for t in tokens) + '"'


# ---------------------------------------------------------- the lexical index

class LexIndex:
    """Read-only verification instrument over the store's lexical tables."""

    def __init__(self, stemmadb_path):
        self.db = ro_connect(stemmadb_path)

    def gold_entries(self, table, rowid):
        """All lex_values entries for a user row: [(lex_id, column, value_norm)]."""
        return self.db.execute(
            "SELECT id, src_column, value_norm FROM lex_values"
            " WHERE src_table = ? AND src_rowid = ?",
            (table, rowid),
        ).fetchall()

    def _hit(self, virtual, query, lex_ids):
        ids = ",".join(str(int(i)) for i in lex_ids)
        try:
            row = self.db.execute(
                f"SELECT rowid FROM {virtual} WHERE {virtual} MATCH ?"
                f" AND rowid IN ({ids}) LIMIT 1",
                (query,),
            ).fetchone()
        except sqlite3.OperationalError:
            return False  # unmatchable query (e.g. all-punctuation)
        return row is not None

    def fts_phrase_hit(self, tokens, lex_ids):
        """Exact channel analog: the token sequence appears (unicode61
        phrase match) in one of the given lex entries."""
        if not tokens:
            return False
        return self._hit("lex_fts", fts_phrase(tokens), lex_ids)

    def trigram_hit(self, text, lex_ids):
        """Trigram channel analog: the string appears as a case-insensitive
        substring of one of the given lex entries (>= 3 chars)."""
        if len(text) < 3:
            return False
        return self._hit("lex_trigram", '"' + text.replace('"', "") + '"', lex_ids)

    def token_hits(self, token, lex_ids):
        """(exact_hit, trigram_hit) for a single content token against the
        given lex entries."""
        exact = self.fts_phrase_hit([token], lex_ids)
        trig = self.trigram_hit(token, lex_ids)
        return exact, trig

    def _count_all(self, virtual, query):
        try:
            return self.db.execute(
                f"SELECT count(*) FROM {virtual} WHERE {virtual} MATCH ?",
                (query,),
            ).fetchone()[0]
        except sqlite3.OperationalError:
            return 0

    def corpus_phrase_counts(self, phrase):
        """(fts_phrase_hits, trigram_hits) corpus-wide, for absent absence."""
        toks = tokenize(phrase)
        return (
            self._count_all("lex_fts", fts_phrase(toks)),
            self._count_all("lex_trigram", '"' + phrase.replace('"', "") + '"'),
        )

    def doc_freq(self, term):
        """Corpus document frequency of a token, via the store's fts5vocab."""
        row = self.db.execute(
            "SELECT doc FROM lex_vocab WHERE term = ?", (term.lower(),)
        ).fetchone()
        return row[0] if row else 0

    def top_docs(self, match, src_table, limit=3):
        """Best-bm25 document rows of src_table matching an FTS query."""
        try:
            return [
                r[0]
                for r in self.db.execute(
                    "SELECT v.src_rowid FROM lex_fts f"
                    " JOIN lex_values v ON v.id = f.rowid"
                    " WHERE lex_fts MATCH ? AND v.src_table = ?"
                    " AND v.src_column = 'text'"
                    " ORDER BY bm25(lex_fts) LIMIT ?",
                    (match, src_table, limit),
                )
            ]
        except sqlite3.OperationalError:
            return []


# ------------------------------------------------------------- tier verifiers

def register_offenders(question):
    toks = set(tokenize(question))
    bad = sorted(toks & REGISTER_BANNED)
    bad += [c for c in REGISTER_BANNED_SUBSTR if c in question]
    return bad


def well_formed(question):
    q = question.strip()
    return 10 <= len(q) <= 240 and q.endswith("?")


def anchor_windows(lex, question, lex_ids, max_len=5):
    """Every contiguous token window of the question that phrase-hits the
    gold entries (fts phrase or trigram substring), with window length
    capped. Returns (hits, longest_run) where hits are (window_tokens,
    channel) and longest_run is the longest hitting run in tokens."""
    toks = tokenize(question)
    hits = []
    longest = 0
    for i in range(len(toks)):
        run = 0
        for n in range(1, min(max_len + 3, len(toks) - i) + 1):
            window = toks[i : i + n]
            phrase = " ".join(window)
            f = lex.fts_phrase_hit(window, lex_ids)
            g = lex.trigram_hit(phrase, lex_ids)
            if f or g:
                run = n
                if n <= max_len:
                    hits.append((window, "exact" if f else "trigram"))
            elif n > 1:
                break  # a longer window can't hit if this one didn't
        longest = max(longest, run)
    return hits, longest


def verify_anchor(lex, question, table, rowid):
    """anchor: >= 1 content anchor phrase hits the gold row; rest paraphrased
    (longest contiguous hitting run bounded)."""
    v = {"tier": "anchor", "checks": []}
    if not well_formed(question):
        v["fail"] = "malformed"
        return False, None, v
    reg = register_offenders(question)
    entries = lex.gold_entries(table, rowid)
    lex_ids = [e[0] for e in entries]
    hits, longest = anchor_windows(lex, question, lex_ids)
    # An anchor must contain a content token of >= 4 chars that is not
    # boilerplate — "of the" hitting the row proves nothing.
    anchors = [
        (w, ch)
        for w, ch in hits
        if any(len(t) >= 4 and t not in STOPWORDS and t not in BOILERPLATE for t in w)
    ]
    v["checks"].append({"anchor_candidates": len(hits), "anchors": [
        {"phrase": " ".join(w), "channel": ch} for w, ch in anchors[:6]
    ]})
    v["longest_hit_run_tokens"] = longest
    v["register_offenders"] = reg
    if reg:
        v["fail"] = "register"
        return False, None, v
    if not anchors:
        v["fail"] = "no_anchor_phrase_hits_gold_row"
        return False, None, v
    if longest > 7:
        v["fail"] = "not_paraphrased_verbatim_run_%d_tokens" % longest
        return False, None, v
    # best anchor: longest window, prefer exact channel
    best = max(anchors, key=lambda a: (len(a[0]), a[1] == "exact"))
    v["anchor"] = {"phrase": " ".join(best[0]), "channel": best[1]}
    v["pass"] = True
    return True, " ".join(best[0]), v


def paraphrase_offending_tokens(lex, question, lex_ids):
    """Content tokens of the question that exact- or trigram-hit the given
    gold entries. Empty list == no lexical anchor on that row."""
    off = []
    for tok in sorted(set(content_tokens(question))):
        exact, trig = lex.token_hits(tok, lex_ids)
        if exact or trig:
            off.append({"token": tok, "exact": exact, "trigram": trig})
    return off


def verify_paraphrase(lex, question, table, rowid):
    """paraphrase: no content token hits the gold row on either channel, and the
    register is colloquial."""
    v = {"tier": "paraphrase"}
    if not well_formed(question):
        v["fail"] = "malformed"
        return False, v
    entries = lex.gold_entries(table, rowid)
    lex_ids = [e[0] for e in entries]
    off = paraphrase_offending_tokens(lex, question, lex_ids)
    reg = register_offenders(question)
    v["offending_tokens"] = off
    v["register_offenders"] = reg
    v["content_tokens_checked"] = len(set(content_tokens(question)))
    if off:
        v["fail"] = "lexical_anchor_present"
        return False, v
    if reg:
        v["fail"] = "register"
        return False, v
    v["pass"] = True
    return True, v


def verify_cross_record(lex, question, reg_rowid, sec_rowid):
    """cross-record: two gold rows (regulations + sections). Both no-anchor, or
    explicitly mixed — never both anchored."""
    v = {"tier": "cross-record"}
    if not well_formed(question):
        v["fail"] = "malformed"
        return False, v
    reg_off = paraphrase_offending_tokens(
        lex, question, [e[0] for e in lex.gold_entries("regulations", reg_rowid)])
    sec_off = paraphrase_offending_tokens(
        lex, question, [e[0] for e in lex.gold_entries("sections", sec_rowid)])
    reg_reg = register_offenders(question)
    v["regulations_offending"] = reg_off
    v["sections_offending"] = sec_off
    v["register_offenders"] = reg_reg
    if reg_reg:
        v["fail"] = "register"
        return False, v
    a, b = bool(reg_off), bool(sec_off)
    if a and b:
        v["fail"] = "both_rows_anchored"
        return False, v
    v["mode"] = ("no_anchor" if not (a or b)
                 else "mixed:regulations_anchored" if a
                 else "mixed:sections_anchored")
    v["pass"] = True
    return True, v


def verify_absent(lex, question, phrase):
    """absent: the defining phrase is in the question and has zero exact-phrase
    and zero trigram hits corpus-wide."""
    v = {"tier": "absent", "defining_phrase": phrase}
    if not well_formed(question):
        v["fail"] = "malformed"
        return False, v
    if norm(phrase) not in norm(question):
        v["fail"] = "phrase_not_in_question"
        return False, v
    fts_n, tri_n = lex.corpus_phrase_counts(phrase)
    v["corpus_hits"] = {"fts_phrase": fts_n, "trigram": tri_n}
    reg = register_offenders(question)
    v["register_offenders"] = reg
    if fts_n or tri_n:
        v["fail"] = "phrase_present_in_corpus"
        return False, v
    if reg:
        v["fail"] = "register"
        return False, v
    v["pass"] = True
    return True, v


# ------------------------------------------------------------------ sampling

TITLE_RE = re.compile(r"Title\s+(\d+(?:\.\d+)?)")
PART_RE = re.compile(r"\nPART\s+(\d+)")


def strata(conn, table):
    """rowids grouped by stratum: CCR title for regulations, CFR
    (title, part) for sections."""
    groups = {}
    for rowid, head in conn.execute(
        f"SELECT id, substr(text, 1, 400) FROM {table}"
    ):
        m = TITLE_RE.search(head)
        key = m.group(1) if m else "?"
        if table == "sections":
            p = PART_RE.search(head)
            key = (key, p.group(1) if p else "?")
        groups.setdefault(key, []).append(rowid)
    return groups


def stratified_stream(groups, rng):
    """Deterministic round-robin over shuffled strata — an endless-enough
    iterator of rowids spread across titles/parts."""
    keys = sorted(groups, key=str)
    rng.shuffle(keys)
    pools = {}
    for k in keys:
        pool = sorted(groups[k])
        rng.shuffle(pool)
        pools[k] = pool
    idx = 0
    while any(pools.values()):
        k = keys[idx % len(keys)]
        idx += 1
        if pools[k]:
            yield pools[k].pop()


# ------------------------------------------------------------------ LM client

class LM:
    def __init__(self, endpoint, model, api_key="", extra_body=None):
        self.endpoint = endpoint.rstrip("/")
        self.model = model
        self.api_key = api_key
        # Extra request-body fields from the config's lm section (e.g.
        # llama.cpp's chat_template_kwargs {"enable_thinking": false}).
        self.extra_body = extra_body or {}
        self.calls = 0
        self.consecutive_failures = 0

    def generate_json(self, prompt, schema, temperature=0.8, max_tokens=900,
                      retries=3):
        body = {
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": temperature,
            "max_tokens": max_tokens,
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "gen", "schema": schema},
            },
            **self.extra_body,
        }
        headers = {"Content-Type": "application/json"}
        if self.api_key:
            headers["Authorization"] = "Bearer " + self.api_key
        last = None
        for attempt in range(retries):
            req = urllib.request.Request(
                self.endpoint + "/chat/completions",
                data=json.dumps(body).encode(),
                headers=headers,
            )
            try:
                self.calls += 1
                with urllib.request.urlopen(req, timeout=180) as r:
                    out = json.load(r)
                result = json.loads(out["choices"][0]["message"]["content"])
                self.consecutive_failures = 0
                return result
            except (urllib.error.URLError, urllib.error.HTTPError,
                    KeyError, TypeError, json.JSONDecodeError,
                    TimeoutError) as e:
                last = e
                time.sleep(2 * (attempt + 1))
        self.consecutive_failures += 1
        if self.consecutive_failures >= 3:
            raise SystemExit(
                f"LM endpoint unusable ({self.consecutive_failures} calls "
                f"failed in a row; last: {last}) — aborting. Partial output "
                "was written after each completed tier.")
        raise RuntimeError(f"LM call failed after {retries} tries: {last}")


QUESTIONS_SCHEMA = {
    "type": "object",
    "properties": {
        "questions": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "anchor": {"type": "string"},
                },
                "required": ["question"],
            },
        }
    },
    "required": ["questions"],
}


def excerpt(text, limit=2400):
    return text if len(text) <= limit else text[:limit] + "\n[...]"


def frequent_terms(text, n=18):
    counts = {}
    for t in tokenize(text):
        if len(t) >= 4 and t not in STOPWORDS and t not in BOILERPLATE:
            counts[t] = counts.get(t, 0) + 1
    return [w for w, _ in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:n]]


# ------------------------------------------------------------------- prompts

ANCHOR_PROMPT = """Below is an excerpt of a legal regulation. Write {k} different \
questions an ordinary person (not a lawyer) might type into a search box, each \
answerable by this text.

Rules for every question:
- Include EXACTLY ONE short distinctive phrase copied verbatim from the text \
(2-4 consecutive words: a term of art, a named thing, a specific noun phrase). \
Report it in "anchor".
- Every OTHER word must paraphrase: plain, everyday language, no other wording \
borrowed from the text. Never quote a whole clause.
- No citation numbers, no "Title X", no section symbols. End with "?". Keep it \
under 25 words.
{feedback}
TEXT:
{text}
"""

PARAPHRASE_PROMPT = """Below is an excerpt of a legal regulation. Write {k} different \
questions that this text ANSWERS, but that share NO vocabulary with it.

Hard rules for every question:
- No meaningful word of your question may appear anywhere in the text — not \
even as a substring of a longer word (do not use "work" if "worker" appears, \
do not use "employ" if "employment" appears). Only these grammar words are \
exempt: {stop}.
- Use casual, colloquial, everyday register — how a person talks, not how a \
lawyer writes ("getting fired", not "separation from service").
- The question must still clearly be asking about the specific situation this \
text governs, so a human could tell the text answers it.
- End with "?". Under 20 words.
- Known words from the text you must avoid (plus any word containing them): \
{avoid}.
{feedback}
TEXT:
{text}
"""

CROSS_RECORD_PROMPT = """Below are two regulations on the same topic: one from the \
California Code of Regulations (STATE) and one from the Code of Federal \
Regulations (FEDERAL). Write {k} different questions an ordinary person might \
ask that BOTH texts help answer — the state rule and the federal rule are each \
a valid part of the answer.

Rules for every question:
- Casual, everyday register; no legal citations, no "Title X", no section \
symbols.
- Prefer wording that does NOT reuse vocabulary from either text — paraphrase \
into plain speech (avoid these words and any word containing them: {avoid}).
- Do not mention "state", "federal", "California", or ask to compare the two — \
just ask the underlying question a person with this problem would ask.
- End with "?". Under 22 words.
{feedback}
STATE TEXT:
{reg_text}

FEDERAL TEXT:
{sec_text}
"""

ABSENT_PROMPT = """Write {k} different casual questions a person might type into \
a search box about "{topic}". Every question MUST contain the exact phrase \
"{topic}". Everyday register, no legal citations. Each ends with "?". Under 18 \
words. Make them sound like someone expecting rules or requirements to exist \
(who regulates it, is a permit needed, what are the rules), varied in phrasing.
{feedback}"""

JOIN_PROMPT = """Rewrite the question below in {k} different casual, natural \
ways. Every rewrite MUST keep these exact words unchanged: {keep}. Do not add \
facts. Keep it under 18 words and end with "?".

QUESTION: {q}
"""


# --------------------------------------------------------------- generation

def gen_tier_single_row(tier, mk_lex, mk_legal, lm, stream_reg, stream_sec, n, seed,
                   stats, review, max_calls=6, k=4, workers=1):
    """Shared loop for the single-row tiers anchor and paraphrase. Rows run in parallel
    (each row's feedback iteration stays serial); acceptance follows stream
    order, stats merge on this thread."""
    state = {"take_reg": True}

    def draw():
        for _ in range(2):
            stream = stream_reg if state["take_reg"] else stream_sec
            table = "regulations" if state["take_reg"] else "sections"
            state["take_reg"] = not state["take_reg"]
            try:
                return table, next(stream)
            except StopIteration:
                continue
        return None

    def attempt(item):
        table, rowid = item
        lex, legal = mk_lex(), mk_legal()
        d = dict.fromkeys(("candidates", "rejected", "rows_abandoned",
                           "accepted"), 0)
        text = legal.execute(
            f"SELECT text FROM {table} WHERE id = ?", (rowid,)).fetchone()[0]
        ex = excerpt(text)
        accepted = None
        calls = 0
        candidates = 0
        feedback = ""
        rejected_here = []
        while calls < max_calls and not accepted:
            if tier == "anchor":
                prompt = ANCHOR_PROMPT.format(k=k, text=ex, feedback=feedback)
            else:
                prompt = PARAPHRASE_PROMPT.format(
                    k=k, text=ex, feedback=feedback,
                    stop=", ".join(sorted(STOPWORDS)),
                    avoid=", ".join(frequent_terms(text)),
                )
            try:
                out = lm.generate_json(prompt, QUESTIONS_SCHEMA)
            except RuntimeError as e:
                print(f"  [{tier}] LM error on {table}#{rowid}: {e}", file=sys.stderr)
                break
            calls += 1
            offenders_seen = []
            for cand in out.get("questions", []):
                q = (cand.get("question") or "").strip()
                if not q:
                    continue
                candidates += 1
                d["candidates"] += 1
                if tier == "anchor":
                    ok, anchor, v = verify_anchor(lex, q, table, rowid)
                else:
                    ok, v = verify_paraphrase(lex, q, table, rowid)
                    anchor = None
                if ok:
                    accepted = (q, anchor, v, calls, candidates)
                    break
                d["rejected"] += 1
                rejected_here.append({"question": q, "fail": v.get("fail")})
                for o in v.get("offending_tokens", []):
                    offenders_seen.append(o["token"])
            if not accepted and tier == "paraphrase" and offenders_seen:
                feedback = ("- Previous attempt FAILED: these words appear in "
                            "the text, avoid them and any word containing "
                            "them: " + ", ".join(sorted(set(offenders_seen))) + "\n")
            elif not accepted and tier == "anchor":
                feedback = ("- Previous attempt FAILED: no 2-4 word phrase was "
                            "copied verbatim from the text. Copy one short "
                            "distinctive phrase exactly.\n")
        if not accepted:
            d["rows_abandoned"] += 1
            return None, None, d, None
        q, anchor, v, calls, candidates = accepted
        d["accepted"] += 1
        target = {"table": table, "column": "text", "rowids": [rowid],
                  "match_mode": "doc"}
        if anchor:
            target["literal"] = anchor
        rec = {
            "question": q,
            "tier": tier,
            "corpus": "legal",
            "targets": [target],
            "nil": False,
            "provenance": {
                "generator": GENERATOR,
                "seed": seed,
                "attempts": {"lm_calls": calls, "candidates": candidates},
                "verification": v,
            },
        }
        rv = {"rec": rec, "snippets": [(table, rowid, text[:500])],
              "rejected": rejected_here[:4]}
        log = f"{table}#{rowid} ({candidates} candidates, {calls} calls): {q}"
        return rec, rv, d, log

    records = []
    for rec, rv, d, log in parallel_collect(draw, attempt, n, workers):
        if rec is not None and len(records) >= n:
            d = {**d, "accepted": 0}  # overshoot row: effort counts, row doesn't
        for key, delta in d.items():
            stats[tier][key] += delta
        if rec is None or len(records) >= n:
            continue
        records.append(rec)
        review.append(rv)
        print(f"  [{tier}] {len(records)}/{n} {log}")
    return records


def find_partner(lex, legal, reg_rowid):
    """A sections row topically paired to a regulations row, found by
    shared distinctive terms with best bm25."""
    text = legal.execute(
        "SELECT text FROM regulations WHERE id = ?", (reg_rowid,)).fetchone()[0]
    # Rank the row's terms by corpus rarity (fts5vocab document frequency):
    # a pairing over rare terms is topical, one over ubiquitous terms is not.
    terms = [t for t in frequent_terms(text, 14) if len(t) >= 6]
    terms = [t for t in terms if 3 <= lex.doc_freq(t) <= 15000]
    terms.sort(key=lex.doc_freq)
    # Prefer tighter topical pairing: three shared distinctive terms, then two.
    for combo in itertools.chain(itertools.combinations(terms[:7], 3),
                                 itertools.combinations(terms[:7], 2)):
        match = " AND ".join(f'"{t}"' for t in combo)
        rows = lex.top_docs(match, "sections", limit=2)
        if rows:
            return rows[0], combo
    return None, None


def gen_tier_cross_record(mk_lex, mk_legal, lm, stream_reg, n, seed, stats, review,
                max_calls=6, k=4, workers=1):
    def draw():
        try:
            return next(stream_reg)
        except StopIteration:
            return None

    def attempt(reg_rowid):
        lex, legal = mk_lex(), mk_legal()
        d = dict.fromkeys(("candidates", "rejected", "rows_abandoned",
                           "accepted", "no_partner"), 0)
        sec_rowid, terms = find_partner(lex, legal, reg_rowid)
        if sec_rowid is None:
            d["no_partner"] += 1
            return None, None, d, None
        reg_text = legal.execute(
            "SELECT text FROM regulations WHERE id = ?", (reg_rowid,)).fetchone()[0]
        sec_text = legal.execute(
            "SELECT text FROM sections WHERE id = ?", (sec_rowid,)).fetchone()[0]
        avoid = sorted(set(frequent_terms(reg_text, 12) + frequent_terms(sec_text, 12)))
        accepted = None
        calls = 0
        candidates = 0
        feedback = ""
        rejected_here = []
        while calls < max_calls and not accepted:
            prompt = CROSS_RECORD_PROMPT.format(
                k=k, reg_text=excerpt(reg_text, 1800),
                sec_text=excerpt(sec_text, 1800),
                avoid=", ".join(avoid), feedback=feedback)
            try:
                out = lm.generate_json(prompt, QUESTIONS_SCHEMA)
            except RuntimeError as e:
                print(f"  [cross-record] LM error on pair ({reg_rowid},{sec_rowid}): {e}",
                      file=sys.stderr)
                break
            calls += 1
            offenders_seen = []
            for cand in out.get("questions", []):
                q = (cand.get("question") or "").strip()
                if not q:
                    continue
                candidates += 1
                d["candidates"] += 1
                ok, v = verify_cross_record(lex, q, reg_rowid, sec_rowid)
                if ok:
                    accepted = (q, v, calls, candidates)
                    break
                d["rejected"] += 1
                rejected_here.append({"question": q, "fail": v.get("fail")})
                for key in ("regulations_offending", "sections_offending"):
                    for o in v.get(key, []):
                        offenders_seen.append(o["token"])
            if not accepted and offenders_seen:
                feedback = ("- Previous attempt FAILED: these words appear in "
                            "the texts, avoid them and any word containing "
                            "them: " + ", ".join(sorted(set(offenders_seen))) + "\n")
        if not accepted:
            d["rows_abandoned"] += 1
            return None, None, d, None
        q, v, calls, candidates = accepted
        v["pairing_terms"] = list(terms)
        d["accepted"] += 1
        rec = {
            "question": q,
            "tier": "cross-record",
            "corpus": "legal",
            "targets": [
                {"table": "regulations", "column": "text",
                 "rowids": [reg_rowid], "match_mode": "doc"},
                {"table": "sections", "column": "text",
                 "rowids": [sec_rowid], "match_mode": "doc"},
            ],
            "nil": False,
            "provenance": {
                "generator": GENERATOR,
                "seed": seed,
                "attempts": {"lm_calls": calls, "candidates": candidates},
                "verification": v,
            },
        }
        rv = {"rec": rec,
              "snippets": [("regulations", reg_rowid, reg_text[:400]),
                           ("sections", sec_rowid, sec_text[:400])],
              "rejected": rejected_here[:4]}
        log = (f"regulations#{reg_rowid} + sections#{sec_rowid} "
               f"[{v['mode']}]: {q}")
        return rec, rv, d, log

    records = []
    for rec, rv, d, log in parallel_collect(draw, attempt, n, workers):
        if rec is not None and len(records) >= n:
            d = {**d, "accepted": 0}
        for key, delta in d.items():
            stats["cross-record"][key] += delta
        if rec is None or len(records) >= n:
            continue
        records.append(rec)
        review.append(rv)
        print(f"  [cross-record] {len(records)}/{n} {log}")
    return records


def gen_tier_absent(mk_lex, lm, n, seed, rng, stats, review, k=3, workers=1):
    topics = list(ABSENT_TOPICS)
    rng.shuffle(topics)
    topic_iter = iter(topics)

    def draw():
        return next(topic_iter, None)

    def attempt(item):
        phrase, argument = item
        lex = mk_lex()
        d = dict.fromkeys(("candidates", "rejected", "rows_abandoned",
                           "accepted", "topics_rejected_present"), 0)
        fts_n, tri_n = lex.corpus_phrase_counts(phrase)
        if fts_n or tri_n:
            d["topics_rejected_present"] += 1
            print(f"  [absent] topic '{phrase}' PRESENT in corpus "
                  f"(fts={fts_n}, trigram={tri_n}) — discarded")
            return None, None, d, None
        try:
            out = lm.generate_json(
                ABSENT_PROMPT.format(k=k, topic=phrase, feedback=""),
                QUESTIONS_SCHEMA)
        except RuntimeError as e:
            print(f"  [absent] LM error on '{phrase}': {e}", file=sys.stderr)
            return None, None, d, None
        accepted = None
        candidates = 0
        rejected_here = []
        for cand in out.get("questions", []):
            q = (cand.get("question") or "").strip()
            if not q:
                continue
            candidates += 1
            d["candidates"] += 1
            ok, v = verify_absent(lex, q, phrase)
            if ok:
                accepted = (q, v)
                break
            d["rejected"] += 1
            rejected_here.append({"question": q, "fail": v.get("fail")})
        if not accepted:
            d["rows_abandoned"] += 1
            return None, None, d, None
        q, v = accepted
        v["absence_argument"] = argument
        d["accepted"] += 1
        rec = {
            "question": q,
            "tier": "absent",
            "corpus": "legal",
            "targets": [],
            "nil": True,
            "provenance": {
                "generator": GENERATOR,
                "seed": seed,
                "attempts": {"lm_calls": 1, "candidates": candidates},
                "verification": v,
            },
        }
        rv = {"rec": rec, "snippets": [], "rejected": rejected_here[:4]}
        return rec, rv, d, f"'{phrase}': {q}"

    records = []
    for rec, rv, d, log in parallel_collect(draw, attempt, n, workers):
        if rec is not None and len(records) >= n:
            d = {**d, "accepted": 0}
        for key, delta in d.items():
            stats["absent"][key] += delta
        if rec is None or len(records) >= n:
            continue
        records.append(rec)
        review.append(rv)
        print(f"  [absent] {len(records)}/{n} {log}")
    return records


# ------------------------------------------------------------------- mini join

def mini_join_cases(mini):
    """Relational cases against the mini corpus. Each case: a seed question,
    the surface mentions that must survive paraphrase, the join SQL that
    derives the gold tuple, and how result columns map to targets. The gold
    tuple is verified by executing the join — tier membership by traversal,
    per 07."""
    q1 = ("SELECT p.id, t.id, s.id, p.name, t.name, s.item"
          " FROM teams t JOIN people p ON t.lead_id = p.id"
          " JOIN shipments s ON s.team_id = t.id WHERE p.name LIKE ? AND t.name = ?")
    q2 = ("SELECT p.id, t.id, s.id, p.name, t.name, s.item"
          " FROM people p JOIN team_members m ON m.person_id = p.id"
          " JOIN teams t ON t.id = m.team_id"
          " JOIN shipments s ON s.team_id = t.id WHERE p.name LIKE ?")
    q3 = ("SELECT p.id, o.id, r.id, p.name, o.name, r.title"
          " FROM people p JOIN offices o ON p.office_id = o.id"
          " JOIN reports r ON r.office_id = o.id WHERE p.name LIKE ? AND r.quarter = ?")
    q4 = ("SELECT t.id, p.id, o.id, t.name, p.name, o.city"
          " FROM teams t JOIN people p ON t.lead_id = p.id"
          " JOIN offices o ON p.office_id = o.id WHERE t.name = ?")
    q5 = ("SELECT o.id, p.id, o.name, p.name"
          " FROM offices o JOIN people p ON p.office_id = o.id WHERE o.name LIKE ?")
    cases = [
        # (case_id, seed question, mentions to keep, sql, params, target spec)
        ("lead-team-ship-chen-billing",
         "What did Chen's Billing team ship?", ["Chen", "Billing"],
         q1, ("%Chen%", "Billing"),
         [("people", "name", 0), ("teams", "name", 1), ("shipments", "item", 2)]),
        ("lead-team-ship-chen-query",
         "What has Chen's Query Engines team shipped?", ["Chen", "Query Engines"],
         q1, ("%Chen%", "Query Engines"),
         [("people", "name", 0), ("teams", "name", 1), ("shipments", "item", 2)]),
        ("member-team-ship-priya",
         "What did Priya's team ship?", ["Priya"],
         q2, ("%Priya%",),
         [("people", "name", 0), ("teams", "name", 1), ("shipments", "item", 2)]),
        ("member-team-ship-okafor",
         "What has Okafor's team shipped?", ["Okafor"],
         q2, ("%Okafor%",),
         [("people", "name", 0), ("teams", "name", 1), ("shipments", "item", 2)]),
        ("person-office-reports-wei-q3",
         "Show the Q3 numbers for Wei Chen's office?", ["Q3", "Wei Chen"],
         q3, ("%Wei Chen%", "2025Q3"),
         [("people", "name", 0), ("offices", "name", 1), ("reports", "title", 2)]),
        ("person-office-reports-dana-q3",
         "What were the Q3 numbers where Dana Chen works?", ["Q3", "Dana Chen"],
         q3, ("%Dana Chen%", "2025Q3"),
         [("people", "name", 0), ("offices", "name", 1), ("reports", "title", 2)]),
        ("person-office-reports-priya-q4",
         "What is the Q4 outlook for Priya Natarajan's office?", ["Q4", "Priya"],
         q3, ("%Priya%", "2025Q4"),
         [("people", "name", 0), ("offices", "name", 1), ("reports", "title", 2)]),
        ("team-lead-city-billing",
         "Which city does the Billing lead work from?", ["Billing"],
         q4, ("Billing",),
         [("teams", "name", 0), ("people", "name", 1), ("offices", "city", 2)]),
        ("team-lead-city-query",
         "Which city is the Query Engines lead based in?", ["Query Engines"],
         q4, ("Query Engines",),
         [("teams", "name", 0), ("people", "name", 1), ("offices", "city", 2)]),
        ("team-lead-city-holdings",
         "Which city does the Holdings Research lead sit in?", ["Holdings Research"],
         q4, ("Holdings Research",),
         [("teams", "name", 0), ("people", "name", 1), ("offices", "city", 2)]),
        ("office-people-northgate",
         "Who works out of the Northgate office?", ["Northgate"],
         q5, ("%Northgate%",),
         [("offices", "name", 0), ("people", "name", 1)]),
        ("office-people-crown",
         "Who is based in the Crown Building?", ["Crown"],
         q5, ("%Crown%",),
         [("offices", "name", 0), ("people", "name", 1)]),
    ]
    out = []
    for cid, seed_q, keep, sql, params, spec in cases:
        rows = mini.execute(sql, params).fetchall()
        if not rows:
            continue
        targets = []
        for tbl, col, idx in spec:
            ids = sorted({r[idx] for r in rows})
            lit_idx = idx + len(spec)
            lits = sorted({str(r[lit_idx]) for r in rows})
            t = {"table": tbl, "column": col, "rowids": ids, "match_mode": "value"}
            if len(lits) == 1:
                t["literal"] = lits[0]
            targets.append(t)
        out.append({"case": cid, "seed_q": seed_q, "keep": keep,
                    "sql": sql, "params": list(params), "targets": targets,
                    "tuples": [list(r[: len(spec)]) for r in rows]})
    return out


def gen_mini_join(mini, lm, n, seed, rng, stats, review, k=3):
    records = []
    cases = mini_join_cases(mini)
    rng.shuffle(cases)
    per_case = max(1, (n + len(cases) - 1) // len(cases)) if cases else 0
    for case in cases:
        if len(records) >= n:
            break
        variants = [case["seed_q"]]
        if per_case > 1:
            try:
                out = lm.generate_json(
                    JOIN_PROMPT.format(k=k, q=case["seed_q"],
                                     keep=", ".join('"%s"' % m for m in case["keep"])),
                    QUESTIONS_SCHEMA, temperature=0.8)
                variants += [(c.get("question") or "").strip()
                             for c in out.get("questions", [])]
            except RuntimeError as e:
                print(f"  [join] LM error on {case['case']}: {e}", file=sys.stderr)
        taken = 0
        for q in variants:
            if not q or taken >= per_case or len(records) >= n:
                break
            stats["join"]["candidates"] += 1
            v = {"tier": "join", "join_path": case["sql"],
                 "join_params": case["params"],
                 "gold_tuples": case["tuples"],
                 "mentions_required": case["keep"]}
            if not well_formed(q):
                stats["join"]["rejected"] += 1
                v["fail"] = "malformed"
                continue
            missing = [m for m in case["keep"] if m.lower() not in q.lower()]
            if missing:
                stats["join"]["rejected"] += 1
                v["fail"] = "mention_lost:" + ",".join(missing)
                continue
            v["pass"] = True
            stats["join"]["accepted"] += 1
            rec = {
                "question": q,
                "tier": "join",
                "corpus": "mini",
                "targets": case["targets"],
                "nil": False,
                "provenance": {
                    "generator": GENERATOR,
                    "seed": seed,
                    "attempts": {"lm_calls": 1 if len(variants) > 1 else 0,
                                 "candidates": len(variants)},
                    "verification": v,
                },
            }
            records.append(rec)
            review.append({"rec": rec, "snippets": [], "rejected": []})
            taken += 1
            print(f"  [join] {len(records)}/{n} {case['case']}: {q}")
    return records


# ------------------------------------------------------------------ review UI

REVIEW_CSS = """
:root { --fg: #1a1a1a; --bg: #fff; --dim: #666; --line: #ddd; --mark: #fff3c4; }
@media (prefers-color-scheme: dark) {
  :root { --fg: #ddd; --bg: #16181a; --dim: #999; --line: #333; --mark: #4a4020; }
}
* { box-sizing: border-box; }
body { margin: 2rem auto; max-width: 62rem; padding: 0 1rem; color: var(--fg);
  background: var(--bg); font: 15px/1.55 system-ui, sans-serif; }
h1 { font-size: 1.3rem; } h2 { font-size: 1.05rem; margin-top: 2.5rem;
  border-bottom: 1px solid var(--line); padding-bottom: .4rem; }
code, pre, td.mono, .mono { font-family: ui-monospace, SFMono-Regular, Menlo,
  Consolas, monospace; font-size: .85em; }
table.stats { border-collapse: collapse; margin: 1rem 0; }
table.stats th, table.stats td { border: 1px solid var(--line);
  padding: .3rem .7rem; text-align: right; }
table.stats th:first-child, table.stats td:first-child { text-align: left; }
.item { border: 1px solid var(--line); border-radius: 4px; margin: 1rem 0;
  padding: .8rem 1rem; }
.q { font-size: 1.05rem; margin: 0 0 .4rem; }
.meta { color: var(--dim); font-size: .8rem; margin-bottom: .5rem; }
pre.snip { border: 1px solid var(--line); background: transparent;
  padding: .5rem .7rem; max-height: 11rem; overflow: auto;
  white-space: pre-wrap; margin: .4rem 0; }
details > summary { cursor: pointer; color: var(--dim); font-size: .82rem; }
pre.verif { border-left: 2px solid var(--line); padding: .2rem .7rem;
  overflow-x: auto; }
.tag { display: inline-block; border: 1px solid var(--line);
  border-radius: 3px; padding: 0 .4rem; font-size: .75rem; margin-right: .4rem; }
mark { background: var(--mark); color: inherit; }
.rej { color: var(--dim); font-size: .8rem; }
"""


def render_review(path, dataset_name, seed, stats, review_items, header):
    def esc(s):
        return html.escape(str(s))

    tiers = ["anchor", "paraphrase", "join", "cross-record", "absent"]
    rows = []
    for t in tiers:
        s = stats.get(t, {})
        cand = s.get("candidates", 0)
        acc = s.get("accepted", 0)
        rej = s.get("rejected", 0)
        rate = f"{100.0 * acc / cand:.0f}%" if cand else "-"
        rows.append(f"<tr><td>{t}</td><td>{acc}</td><td>{cand}</td>"
                    f"<td>{rej}</td><td>{rate}</td>"
                    f"<td>{s.get('rows_abandoned', 0)}</td></tr>")
    parts = [
        f"<title>{esc(dataset_name)} review</title>",
        f"<style>{REVIEW_CSS}</style>",
        f"<h1>{esc(dataset_name)} — review pass</h1>",
        f"<p class=meta>seed {seed} · generated {time.strftime('%Y-%m-%d')} · "
        "every question below passed mechanical tier verification; this page "
        "is the human skim required before the set is frozen "
        "(docs/design/07-eval-harness.md). Read each question, check it is "
        "genuinely answerable by its gold snippet(s), and flag any that are "
        "not.</p>",
        "<table class=stats><tr><th>tier</th><th>accepted</th>"
        "<th>candidates</th><th>rejected</th><th>acceptance</th>"
        "<th>rows abandoned</th></tr>",
        *rows,
        "</table>",
        f"<p class=meta>join note: {esc(header['l3']['note'])}</p>",
    ]
    for t in tiers:
        items = [it for it in review_items if it["rec"]["tier"] == t]
        if not items:
            continue
        parts.append(f"<h2>{t} · {len(items)} questions</h2>")
        for it in items:
            rec = it["rec"]
            v = rec["provenance"]["verification"]
            tgt = " · ".join(
                f"{x['table']}.{x['column']} #{','.join(map(str, x['rowids']))}"
                for x in rec["targets"]) or "absent (no target)"
            parts.append("<div class=item>")
            parts.append(f"<p class=q>{esc(rec['question'])}</p>")
            attempts = rec["provenance"]["attempts"]
            parts.append(
                f"<p class=meta><span class=tag>{t}</span>"
                f"<span class=mono>{esc(tgt)}</span> · "
                f"{attempts['candidates']} candidate(s), "
                f"{attempts['lm_calls']} LM call(s)</p>")
            for tbl, rowid, snip in it["snippets"]:
                shown = esc(snip)
                lit = next((x.get("literal") for x in rec["targets"]
                            if x["table"] == tbl and x.get("literal")), None)
                if lit:
                    pat = re.compile(re.escape(esc(lit)), re.IGNORECASE)
                    shown = pat.sub(lambda m: f"<mark>{m.group(0)}</mark>", shown, count=1)
                parts.append(
                    f"<p class=meta>gold {esc(tbl)}#{rowid}</p>"
                    f"<pre class=snip>{shown}</pre>")
            parts.append(
                "<details><summary>verification record</summary>"
                f"<pre class='verif mono'>{esc(json.dumps(v, indent=1))}</pre>"
                "</details>")
            if it["rejected"]:
                rej = "; ".join(
                    f"“{esc(r['question'])}” ({esc(r['fail'])})"
                    for r in it["rejected"])
                parts.append(f"<p class=rej>rejected siblings: {rej}</p>")
            parts.append("</div>")
    with open(path, "w") as f:
        f.write("\n".join(parts))


# ---------------------------------------------------------------------- main

def load_lm_config(config_path):
    with open(config_path) as f:
        cfg = json.load(f)
    lm_cfg = (cfg.get("eval", {}) or {}).get("lm") or cfg["console"]["lm"]
    base = os.path.dirname(os.path.abspath(config_path))
    dbs = {k: os.path.join(base, v) for k, v in cfg.get("databases", {}).items()}
    return cfg, lm_cfg, dbs


def new_stats():
    keys = ("candidates", "accepted", "rejected", "rows_abandoned",
            "no_partner", "topics_rejected_present")
    return {t: dict.fromkeys(keys, 0) for t in ("anchor", "paraphrase", "join", "cross-record", "absent")}


def write_jsonl(path, header, records):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(json.dumps(header) + "\n")
        for r in records:
            f.write(json.dumps(r) + "\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--config", required=True, help="repo config.json")
    ap.add_argument("--n-per-tier", type=int, default=50)
    ap.add_argument("--n-nil", type=int, default=25)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--out", default="eval/datasets/legal-synth-v1.jsonl")
    ap.add_argument("--mini-out", default=None,
                    help="mini join output (default: mini-join-v1.jsonl next to --out)")
    ap.add_argument("--html", default=None,
                    help="review page (default: <out-stem>-review.html)")
    ap.add_argument("--tiers", default="anchor,paraphrase,join,cross-record,absent",
                    help="comma list of tiers to generate")
    ap.add_argument("--workers", type=int, default=12,
                    help="concurrent rows in flight (each row's feedback "
                         "loop stays serial; the endpoint batches requests)")
    args = ap.parse_args()

    cfg, lm_cfg, dbs = load_lm_config(args.config)
    legal_db = dbs["legal"]
    mini_db = dbs["mini"]
    stemmadb = os.path.splitext(legal_db)[0] + ".stemmadb"
    out = args.out
    mini_out = args.mini_out or os.path.join(os.path.dirname(out), "mini-join-v1.jsonl")
    html_out = args.html or os.path.splitext(out)[0] + "-review.html"
    tiers = [t.strip().lower() for t in args.tiers.split(",") if t.strip()]

    legal = ro_connect(legal_db)
    mini = ro_connect(mini_db)
    lex = LexIndex(stemmadb)
    mk_lex = ThreadLocal(lambda: LexIndex(stemmadb))
    mk_legal = ThreadLocal(lambda: ro_connect(legal_db))
    lm = LM(lm_cfg["endpoint"], lm_cfg["model"], lm_cfg.get("api_key", ""),
            lm_cfg.get("extra_body"))
    rng = random.Random(args.seed)

    print(f"corpus: {legal_db}\nstore:  {stemmadb}\nlm:     "
          f"{lm_cfg['endpoint']} ({lm_cfg['model']})\nseed:   {args.seed}")

    reg_strata = strata(legal, "regulations")
    sec_strata = strata(legal, "sections")
    print(f"strata: {len(reg_strata)} CCR titles, {len(sec_strata)} CFR (title, part)")

    stats = new_stats()
    review = []
    records = []
    mini_records = []

    join_note = ("legal join slot is empty: the legal corpus has no join tables "
               "yet (citation mining is in flight on a sibling branch); "
               "relational-tier queries live in mini-join-v1.jsonl against the "
               "mini corpus until legal citation edges land.")

    # Deterministic sampling streams (independent per tier so a change to one
    # tier's consumption does not reshuffle another's).
    def streams():
        r = random.Random(args.seed)
        return (stratified_stream(reg_strata, random.Random(r.randint(0, 1 << 30))),
                stratified_stream(sec_strata, random.Random(r.randint(0, 1 << 30))),
                stratified_stream(reg_strata, random.Random(r.randint(0, 1 << 30))),
                stratified_stream(sec_strata, random.Random(r.randint(0, 1 << 30))),
                stratified_stream(reg_strata, random.Random(r.randint(0, 1 << 30))))

    s_reg_anchor, s_sec_anchor, s_reg_para, s_sec_para, s_reg_cross = streams()

    def flush():
        counts = {t: sum(1 for r in records if r["tier"] == t)
                  for t in ("anchor", "paraphrase", "cross-record", "absent")}
        counts["join"] = 0
        header = {
            "type": "header",
            "dataset": "legal-synth-v1",
            "version": 1,
            "generator": GENERATOR,
            "model": lm_cfg["model"],
            "seed": args.seed,
            "corpus": {"legal_db": os.path.basename(legal_db),
                       "regulations_rows": legal.execute(
                           "SELECT count(*) FROM regulations").fetchone()[0],
                       "sections_rows": legal.execute(
                           "SELECT count(*) FROM sections").fetchone()[0]},
            "counts": counts,
            "join": {"status": "todo", "note": join_note},
            "stats": stats,
        }
        write_jsonl(out, header, records)
        mini_header = {
            "type": "header",
            "dataset": "mini-join-v1",
            "version": 1,
            "generator": GENERATOR,
            "model": lm_cfg["model"],
            "seed": args.seed,
            "corpus": {"mini_db": os.path.basename(mini_db)},
            "counts": {"join": len(mini_records)},
        }
        write_jsonl(mini_out, mini_header, mini_records)
        render_review(html_out, "legal-synth-v1 (+ mini-join-v1)", args.seed,
                      stats, review, header)

    t0 = time.time()
    if "anchor" in tiers:
        print("== anchor (lexical anchor) ==")
        records += gen_tier_single_row("anchor", mk_lex, mk_legal, lm, s_reg_anchor, s_sec_anchor,
                                  args.n_per_tier, args.seed, stats, review,
                                  workers=args.workers)
        flush()
    if "paraphrase" in tiers:
        print("== paraphrase (semantic, no lexical anchor) ==")
        records += gen_tier_single_row("paraphrase", mk_lex, mk_legal, lm, s_reg_para, s_sec_para,
                                  args.n_per_tier, args.seed, stats, review,
                                  workers=args.workers)
        flush()
    if "cross-record" in tiers:
        print("== cross-record (cross-record, state + federal) ==")
        records += gen_tier_cross_record(mk_lex, mk_legal, lm, s_reg_cross, args.n_per_tier,
                               args.seed, stats, review, workers=args.workers)
        flush()
    if "absent" in tiers:
        print("== absent (verified absence) ==")
        records += gen_tier_absent(mk_lex, lm, args.n_nil, args.seed, rng, stats,
                                review, workers=args.workers)
        flush()
    if "join" in tiers:
        print("== join (relational, mini corpus) ==")
        mini_records = gen_mini_join(mini, lm, args.n_per_tier, args.seed, rng,
                                   stats, review)
        flush()

    dt = time.time() - t0
    print(f"\nwrote {len(records)} legal records -> {out}")
    print(f"wrote {len(mini_records)} mini join records -> {mini_out}")
    print(f"review page -> {html_out}")
    print(f"{lm.calls} LM calls in {dt / 60:.1f} min")
    for t, s in stats.items():
        if s["candidates"] or s["accepted"]:
            rate = 100.0 * s["accepted"] / s["candidates"] if s["candidates"] else 0
            print(f"  {t}: {s['accepted']} accepted / {s['candidates']} "
                  f"candidates ({rate:.0f}%), {s['rows_abandoned']} rows abandoned")


if __name__ == "__main__":
    main()
