# Offline retrieval-policy study

Status: research-only. Ambit supplies offline collision diagnostics from an encoder's embedding geometry. The study reads exported candidate evidence and has no production dependency on Ambit.

The tool compares six candidate-selection policies over every input query. Each policy uses the same candidate pool and `candidate_limit`.

- `existing_bounded` keeps candidates marked `existing_selected`.
- `deeper_fixed` keeps the highest-ranked candidates.
- `score_margin` keeps candidates whose score is within `score_margin` of the highest score.
- `graph_directed` keeps the existing selection, then adds candidates in descending `graph_support` order.
- `ambit_directed` keeps the existing selection, then adds candidates in descending `ambit_collision` order.
- `combined_bounded` keeps the existing selection, then adds candidates by the sum of normalized graph support and collision evidence.

All directed policies add only candidates with positive evidence. Rank breaks ties. The shared limit truncates every result.

## Input contract

The input is JSON Lines. Its first record describes the study:

```json
{"type":"study","schema_version":1,"database_fingerprint":"sha256:...","vector_fingerprint":"sha256:...","candidate_limit":3,"score_margin":0.03}
```

`database_fingerprint` identifies the source data and schema. `vector_fingerprint` identifies the encoder configuration and stored vectors. The producing system chooses the fingerprint algorithm and records its name as a prefix.

Each remaining record describes one query:

```json
{
  "type": "query",
  "query_id": "stable-query-id",
  "query": "What is the filing deadline?",
  "query_kind": "record_as_query",
  "gold_candidate_id": "record-17",
  "supported_alternative_ids": ["record-29"],
  "candidates": [
    {
      "id": "record-17",
      "rank": 1,
      "score": 0.91,
      "existing_selected": true,
      "graph_support": 0.8,
      "ambit_collision": 0.2
    }
  ],
  "latency_ms": {"existing_bounded": 2.4}
}
```

`query` preserves the evaluated input for review. Use `record_as_query` when a known database record produced the input. Use `free_form` for independently authored input. Candidate ranks are unique positive integers, and higher scores are better.

`graph_support` measures evidence that connects a candidate to already grounded entities or relations. `ambit_collision` measures whether a candidate belongs to the query's local collision region. Both diagnostics use `[0, 1]`, where larger values provide stronger expansion evidence.

The gold candidate and every supported alternative must occur in `candidates`. `existing_selected` records the current bounded result before experimental expansion. The optional `latency_ms` object carries measured end-to-end latency for any policy; the offline tool does not estimate missing latency.

## Output metrics

The JSON output reports each policy for all queries and for each query kind. It also echoes both source fingerprints and adds a fingerprint of the canonical query population. Optional latency measurements do not affect the population fingerprint.

- Gold survival is the share of queries whose selection contains the gold candidate.
- Supported-alternative recall is the share of all labeled alternatives across queries that survive selection.
- Added candidates count candidates absent from `existing_bounded`.
- Ambiguity localization succeeds when a query with a supported alternative retains at least two supported interpretations.
- False commitment occurs when a query with a supported alternative retains exactly one candidate.
- Mean latency uses only the provided measurements and reports its sample count.

A rate or mean is `null` when its denominator is zero. Every rate is accompanied by its numerator and denominator counts.

## Run the study

```sh
python3 tools/ambit_retrieval_study.py eval/retrieval_study/sample.jsonl
python3 tools/ambit_retrieval_study.py study.jsonl --output results.json
python3 tools/test_ambit_retrieval_study.py
```
