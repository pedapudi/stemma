# Dense retrieval shadow comparison

Status: research and release validation. The tool compares an approximate dense candidate generator with the exact SQLite result for the same query vector and eligible scope. Exact retrieval supplies the unlabeled correctness oracle.

The comparison reads exported JSON Lines and writes deterministic JSON. It has no runtime dependency on the optional approximate index.

## Input contract

The first record binds every observation to one corpus and vector generation:

```json
{
  "type": "dense_shadow_study",
  "schema_version": 1,
  "corpus_fingerprint": "sha256:...",
  "vector_revision": "sha256:...",
  "sidecar_checksum": "sha256:...",
  "vector_count": 100000,
  "dimension": 768,
  "metric": "cosine",
  "candidate_limit": 10,
  "ambiguity_margin": 0.03
}
```

The receipt fields identify the authoritative corpus, stored vector space, and approximate sidecar. `candidate_limit` applies to both result lists. `ambiguity_margin` classifies a top-two authoritative similarity gap as narrow.

Each remaining record pairs both retrieval paths for one query vector:

```json
{
  "type": "query",
  "query_id": "stable-query-id",
  "query_vector_fingerprint": "sha256:...",
  "eligible_scope_fingerprint": "sha256:...",
  "exact": {
    "candidates": [{"id": "17", "similarity": 0.91}],
    "outcome": "resolved",
    "latency_ms": 12.4
  },
  "approximate": {
    "candidates": [{"id": "17", "similarity": 0.91}],
    "outcome": "resolved",
    "latency_ms": 3.1
  }
}
```

Candidate identities are stable SQLite vector identities. Candidate lists use descending authoritative cosine similarity after the approximate sidecar results map back to SQLite vectors. Shared candidates must have the same similarity in both lists. The producer uses stable identity as the final tie-break.

`approximate.candidates` preserves the rescored sidecar proposal before an ambiguity safeguard expands it with exact search. This placement lets the tool measure false wide margins. `exact.outcome` comes from the exact resolution path. `approximate.outcome` comes from the complete approximate path after its exact ambiguity safeguards. Valid outcomes are `resolved`, `equivalent`, `ambiguous`, `unknown`, and `unanswerable`. These system outcomes require no human semantic labels.

Latency is optional and must measure the complete path named by its result block. The query-population fingerprint excludes latency so warm and cold measurements over the same queries remain comparable.

## Fidelity metrics

- Approximate candidate recall is the share of exact top candidates recovered by the rescored approximate list.
- Leading-candidate agreement is the share of queries whose first candidate identity matches.
- Missed ambiguity is an exact `ambiguous` outcome that the guarded approximate path changes to another outcome.
- False wide margin occurs when the exact top-two gap is at most `ambiguity_margin` and the approximate list has a larger gap or fewer than two candidates.
- Outcome change is any difference between the exact and guarded approximate outcome.
- Latency summaries report the mean, median, 95th percentile, and mean paired speedup for available measurements.

A rate is `null` when its denominator is zero. False wide margin diagnoses unsafe candidate evidence even when the exact safeguard preserves the final outcome.

## Run a comparison

```sh
python3 tools/dense_shadow_compare.py eval/dense_shadow/sample.jsonl
python3 tools/dense_shadow_compare.py shadow.jsonl --output comparison.json
python3 tools/test_dense_shadow_compare.py
```

## Release benchmark guidance

Run optimized builds at 384, 768, and 1,024 dimensions when the index supports those shapes. If deployment uses different dimensions, choose three supported dimensions that span the expected range. Measure 10,000, 50,000, 100,000, and the largest expected vectors per frequently searched scope. Record cold-cache and warm-cache observations in separate files.

Use a frozen query-vector population for both paths. Preserve the query-vector and eligible-scope fingerprints across each comparison. Randomize execution order outside the export format when cache warming could favor one path.

Choose numerical gates before collecting release results. Enabling approximate retrieval requires zero missed ambiguity and zero guarded outcome changes in the sampled queries. A false wide margin is acceptable only when the exact safeguard preserves the final outcome. The release record must also state the required candidate recall and latency improvement for its corpus.
