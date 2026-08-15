#!/usr/bin/env python3
"""Compare bounded retrieval policies over one frozen candidate population."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import sys
from collections.abc import Iterable
from pathlib import Path


POLICIES = (
    "existing_bounded",
    "deeper_fixed",
    "score_margin",
    "graph_directed",
    "ambit_directed",
    "combined_bounded",
)
QUERY_KINDS = ("record_as_query", "free_form")


class ContractError(ValueError):
    """The study input does not satisfy the JSONL contract."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def _finite_number(value: object, field: str) -> float:
    _require(isinstance(value, (int, float)) and not isinstance(value, bool), f"{field} must be a number")
    number = float(value)
    _require(math.isfinite(number), f"{field} must be finite")
    return number


def _read_jsonl(path: Path) -> tuple[dict, list[dict]]:
    records: list[dict] = []
    for line_number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not raw.strip():
            continue
        try:
            value = json.loads(raw)
        except json.JSONDecodeError as error:
            raise ContractError(f"line {line_number}: invalid JSON: {error.msg}") from error
        _require(isinstance(value, dict), f"line {line_number}: record must be an object")
        records.append(value)

    _require(records, "input is empty")
    header, *queries = records
    _require(header.get("type") == "study", "first record must have type 'study'")
    _require(queries, "input contains no query records")
    return header, queries


def _validate(header: dict, queries: list[dict]) -> None:
    _require(header.get("schema_version") == 1, "schema_version must be 1")
    for field in ("database_fingerprint", "vector_fingerprint"):
        _require(isinstance(header.get(field), str) and header[field].strip(), f"{field} is required")

    limit = header.get("candidate_limit")
    _require(
        isinstance(limit, int) and not isinstance(limit, bool) and limit > 0,
        "candidate_limit must be a positive integer",
    )
    margin = _finite_number(header.get("score_margin"), "score_margin")
    _require(margin >= 0, "score_margin must be non-negative")

    query_ids: set[str] = set()
    for index, query in enumerate(queries, 1):
        prefix = f"query record {index}"
        _require(query.get("type") == "query", f"{prefix}: type must be 'query'")
        query_id = query.get("query_id")
        _require(isinstance(query_id, str) and query_id, f"{prefix}: query_id is required")
        _require(query_id not in query_ids, f"{prefix}: duplicate query_id {query_id!r}")
        query_ids.add(query_id)
        _require(isinstance(query.get("query"), str) and query["query"].strip(), f"{prefix}: query is required")
        _require(query.get("query_kind") in QUERY_KINDS, f"{prefix}: invalid query_kind")

        candidates = query.get("candidates")
        _require(isinstance(candidates, list) and candidates, f"{prefix}: candidates must be a non-empty list")
        candidate_ids: set[str] = set()
        ranks: set[int] = set()
        baseline_count = 0
        for candidate_index, candidate in enumerate(candidates, 1):
            field = f"{prefix}, candidate {candidate_index}"
            _require(isinstance(candidate, dict), f"{field}: candidate must be an object")
            candidate_id = candidate.get("id")
            _require(isinstance(candidate_id, str) and candidate_id, f"{field}: id is required")
            _require(candidate_id not in candidate_ids, f"{field}: duplicate id {candidate_id!r}")
            candidate_ids.add(candidate_id)
            rank = candidate.get("rank")
            _require(
                isinstance(rank, int) and not isinstance(rank, bool) and rank > 0,
                f"{field}: rank must be a positive integer",
            )
            _require(rank not in ranks, f"{field}: duplicate rank {rank}")
            ranks.add(rank)
            _finite_number(candidate.get("score"), f"{field}.score")
            _require(
                isinstance(candidate.get("existing_selected"), bool),
                f"{field}.existing_selected must be boolean",
            )
            baseline_count += int(candidate["existing_selected"])
            for diagnostic in ("graph_support", "ambit_collision"):
                value = _finite_number(candidate.get(diagnostic), f"{field}.{diagnostic}")
                _require(0 <= value <= 1, f"{field}.{diagnostic} must be between 0 and 1")
        _require(baseline_count <= limit, f"{prefix}: existing selection exceeds candidate_limit")

        gold = query.get("gold_candidate_id")
        _require(isinstance(gold, str) and gold in candidate_ids, f"{prefix}: gold_candidate_id must name a candidate")
        alternatives = query.get("supported_alternative_ids")
        _require(isinstance(alternatives, list), f"{prefix}: supported_alternative_ids must be a list")
        _require(
            all(isinstance(item, str) for item in alternatives),
            f"{prefix}: supported alternatives must be strings",
        )
        _require(len(set(alternatives)) == len(alternatives), f"{prefix}: supported alternatives must be unique")
        _require(gold not in alternatives, f"{prefix}: gold candidate cannot also be an alternative")
        _require(set(alternatives) <= candidate_ids, f"{prefix}: a supported alternative is absent from candidates")

        latencies = query.get("latency_ms", {})
        _require(isinstance(latencies, dict), f"{prefix}: latency_ms must be an object")
        _require(set(latencies) <= set(POLICIES), f"{prefix}: latency_ms contains an unknown policy")
        for policy, value in latencies.items():
            _require(
                _finite_number(value, f"{prefix}.latency_ms.{policy}") >= 0,
                f"{prefix}: latency must be non-negative",
            )


def load_study(path: Path) -> tuple[dict, list[dict]]:
    """Load and validate one study input."""
    header, queries = _read_jsonl(path)
    _validate(header, queries)
    return header, queries


def select_candidates(query: dict, header: dict) -> dict[str, list[str]]:
    """Apply every policy to a query under the shared candidate limit."""
    limit = header["candidate_limit"]
    ranked = sorted(query["candidates"], key=lambda candidate: candidate["rank"])
    baseline = [candidate for candidate in ranked if candidate["existing_selected"]]

    def ids(candidates: Iterable[dict]) -> list[str]:
        return [candidate["id"] for candidate in list(candidates)[:limit]]

    def extend(priority, eligible) -> list[str]:
        additions = [candidate for candidate in ranked if not candidate["existing_selected"] and eligible(candidate)]
        additions.sort(key=lambda candidate: (priority(candidate), candidate["rank"]))
        return ids([*baseline, *additions])

    top_score = max(candidate["score"] for candidate in ranked)
    margin = header["score_margin"]
    return {
        "existing_bounded": ids(baseline),
        "deeper_fixed": ids(ranked),
        "score_margin": ids(candidate for candidate in ranked if top_score - candidate["score"] <= margin),
        "graph_directed": extend(
            lambda candidate: -candidate["graph_support"],
            lambda candidate: candidate["graph_support"] > 0,
        ),
        "ambit_directed": extend(
            lambda candidate: -candidate["ambit_collision"],
            lambda candidate: candidate["ambit_collision"] > 0,
        ),
        "combined_bounded": extend(
            lambda candidate: -(candidate["graph_support"] + candidate["ambit_collision"]),
            lambda candidate: candidate["graph_support"] > 0 or candidate["ambit_collision"] > 0,
        ),
    }


def _metrics(queries: list[dict], selections: dict[str, dict[str, list[str]]], policy: str) -> dict:
    ambiguous = [query for query in queries if query["supported_alternative_ids"]]
    gold_survival = 0
    alternative_hits = 0
    alternative_total = 0
    added = 0
    localized = 0
    false_commitments = 0
    latencies: list[float] = []

    for query in queries:
        selected = selections[query["query_id"]][policy]
        selected_set = set(selected)
        baseline_set = set(selections[query["query_id"]]["existing_bounded"])
        gold_survival += int(query["gold_candidate_id"] in selected_set)
        alternatives = set(query["supported_alternative_ids"])
        alternative_hits += len(alternatives & selected_set)
        alternative_total += len(alternatives)
        added += len(selected_set - baseline_set)
        if alternatives:
            supported = alternatives | {query["gold_candidate_id"]}
            localized += int(len(supported & selected_set) >= 2)
            false_commitments += int(len(selected) == 1)
        if policy in query.get("latency_ms", {}):
            latencies.append(float(query["latency_ms"][policy]))

    count = len(queries)
    ambiguous_count = len(ambiguous)
    return {
        "query_count": count,
        "ambiguous_query_count": ambiguous_count,
        "gold_survival_count": gold_survival,
        "gold_survival_rate": gold_survival / count if count else None,
        "supported_alternative_hit_count": alternative_hits,
        "supported_alternative_count": alternative_total,
        "supported_alternative_recall": alternative_hits / alternative_total if alternative_total else None,
        "added_candidate_count": added,
        "mean_added_candidates": added / count if count else None,
        "ambiguity_localization_count": localized,
        "ambiguity_localization_rate": localized / ambiguous_count if ambiguous_count else None,
        "false_commitment_count": false_commitments,
        "false_commitment_rate": false_commitments / ambiguous_count if ambiguous_count else None,
        "latency_sample_count": len(latencies),
        "mean_latency_ms": math.fsum(latencies) / len(latencies) if latencies else None,
    }


def run_study(header: dict, queries: list[dict]) -> dict:
    """Return deterministic aggregate results for a validated study."""
    selections = {query["query_id"]: select_candidates(query, header) for query in queries}
    population = ({key: value for key, value in query.items() if key != "latency_ms"} for query in queries)
    canonical = (json.dumps(query, sort_keys=True, separators=(",", ":")) for query in population)
    population_bytes = "\n".join(canonical).encode()
    groups = {
        "all": queries,
        **{kind: [query for query in queries if query["query_kind"] == kind] for kind in QUERY_KINDS},
    }
    return {
        "schema_version": 1,
        "database_fingerprint": header["database_fingerprint"],
        "vector_fingerprint": header["vector_fingerprint"],
        "query_population_fingerprint": f"sha256:{hashlib.sha256(population_bytes).hexdigest()}",
        "candidate_limit": header["candidate_limit"],
        "score_margin": header["score_margin"],
        "results": {
            group: {policy: _metrics(group_queries, selections, policy) for policy in POLICIES}
            for group, group_queries in groups.items()
        },
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="study JSONL")
    parser.add_argument("--output", type=Path, help="write JSON here instead of standard output")
    args = parser.parse_args(argv)
    try:
        header, queries = load_study(args.input)
        rendered = json.dumps(run_study(header, queries), indent=2, sort_keys=True) + "\n"
        if args.output:
            args.output.write_text(rendered, encoding="utf-8")
        else:
            sys.stdout.write(rendered)
        return 0
    except (ContractError, OSError) as error:
        print(f"study failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
