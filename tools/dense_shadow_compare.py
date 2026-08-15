#!/usr/bin/env python3
"""Compare approximate dense retrieval with the exact result on the same queries."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import sys
from pathlib import Path


OUTCOMES = {"resolved", "equivalent", "ambiguous", "unknown", "unanswerable"}


class ContractError(ValueError):
    """The shadow-comparison input violates its JSONL contract."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def _number(value: object, field: str) -> float:
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
            record = json.loads(raw)
        except json.JSONDecodeError as error:
            raise ContractError(f"line {line_number}: invalid JSON: {error.msg}") from error
        _require(isinstance(record, dict), f"line {line_number}: record must be an object")
        records.append(record)

    _require(records, "input is empty")
    header, *queries = records
    _require(header.get("type") == "dense_shadow_study", "first record must have type 'dense_shadow_study'")
    _require(queries, "input contains no query records")
    return header, queries


def _validate_candidates(candidates: object, field: str, limit: int, allow_empty: bool) -> list[dict]:
    _require(isinstance(candidates, list), f"{field} must be a list")
    _require(allow_empty or candidates, f"{field} must not be empty")
    _require(len(candidates) <= limit, f"{field} exceeds candidate_limit")
    identities: set[str] = set()
    previous = math.inf
    for index, candidate in enumerate(candidates, 1):
        prefix = f"{field}[{index}]"
        _require(isinstance(candidate, dict), f"{prefix} must be an object")
        identity = candidate.get("id")
        _require(isinstance(identity, str) and identity.strip(), f"{prefix}.id is required")
        _require(identity not in identities, f"{field} contains duplicate candidate {identity!r}")
        identities.add(identity)
        similarity = _number(candidate.get("similarity"), f"{prefix}.similarity")
        _require(-1 <= similarity <= 1, f"{prefix}.similarity must be between -1 and 1")
        _require(similarity <= previous, f"{field} must be ordered by descending similarity")
        previous = similarity
    return candidates


def _validate_result(result: object, field: str, limit: int, allow_empty: bool) -> dict:
    _require(isinstance(result, dict), f"{field} must be an object")
    _validate_candidates(result.get("candidates"), f"{field}.candidates", limit, allow_empty)
    _require(result.get("outcome") in OUTCOMES, f"{field}.outcome is invalid")
    if "latency_ms" in result:
        _require(_number(result["latency_ms"], f"{field}.latency_ms") > 0, f"{field}.latency_ms must be positive")
    return result


def _validate(header: dict, queries: list[dict]) -> None:
    _require(header.get("schema_version") == 1, "schema_version must be 1")
    for field in ("corpus_fingerprint", "vector_revision", "sidecar_checksum", "metric"):
        _require(isinstance(header.get(field), str) and header[field].strip(), f"{field} is required")
    for field in ("vector_count", "dimension", "candidate_limit"):
        value = header.get(field)
        _require(
            isinstance(value, int) and not isinstance(value, bool) and value > 0,
            f"{field} must be a positive integer",
        )
    margin = _number(header.get("ambiguity_margin"), "ambiguity_margin")
    _require(0 <= margin <= 2, "ambiguity_margin must be between 0 and 2")

    query_ids: set[str] = set()
    for index, query in enumerate(queries, 1):
        prefix = f"query record {index}"
        _require(query.get("type") == "query", f"{prefix}: type must be 'query'")
        query_id = query.get("query_id")
        _require(isinstance(query_id, str) and query_id.strip(), f"{prefix}: query_id is required")
        _require(query_id not in query_ids, f"{prefix}: duplicate query_id {query_id!r}")
        query_ids.add(query_id)
        for field in ("query_vector_fingerprint", "eligible_scope_fingerprint"):
            value = query.get(field)
            _require(isinstance(value, str) and value.strip(), f"{prefix}: {field} is required")

        limit = header["candidate_limit"]
        exact = _validate_result(query.get("exact"), f"{prefix}.exact", limit, False)
        approximate = _validate_result(query.get("approximate"), f"{prefix}.approximate", limit, True)
        exact_scores = {candidate["id"]: candidate["similarity"] for candidate in exact["candidates"]}
        for candidate in approximate["candidates"]:
            if candidate["id"] in exact_scores:
                _require(
                    math.isclose(candidate["similarity"], exact_scores[candidate["id"]], rel_tol=0, abs_tol=1e-12),
                    f"{prefix}: shared candidate similarities differ after authoritative rescoring",
                )


def load_study(path: Path) -> tuple[dict, list[dict]]:
    """Load and validate one shadow-comparison study."""
    header, queries = _read_jsonl(path)
    _validate(header, queries)
    return header, queries


def _margin(result: dict) -> float | None:
    candidates = result["candidates"]
    return candidates[0]["similarity"] - candidates[1]["similarity"] if len(candidates) >= 2 else None


def _percentile(values: list[float], percentile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(percentile * len(ordered)) - 1)]


def _latency_summary(values: list[float]) -> dict:
    return {
        "sample_count": len(values),
        "mean_ms": math.fsum(values) / len(values) if values else None,
        "p50_ms": _percentile(values, 0.50),
        "p95_ms": _percentile(values, 0.95),
    }


def compare(header: dict, queries: list[dict]) -> dict:
    """Measure approximate fidelity against exact retrieval without semantic labels."""
    recovered = 0
    exact_candidate_count = 0
    leading_agreement = 0
    exact_ambiguous = 0
    missed_ambiguity = 0
    exact_narrow_margin = 0
    false_wide_margin = 0
    outcome_changes = 0
    exact_latencies: list[float] = []
    approximate_latencies: list[float] = []
    speedups: list[float] = []
    margin = header["ambiguity_margin"]

    for query in queries:
        exact = query["exact"]
        approximate = query["approximate"]
        exact_ids = {candidate["id"] for candidate in exact["candidates"]}
        approximate_ids = {candidate["id"] for candidate in approximate["candidates"]}
        recovered += len(exact_ids & approximate_ids)
        exact_candidate_count += len(exact_ids)
        leading_agreement += int(
            bool(approximate["candidates"])
            and exact["candidates"][0]["id"] == approximate["candidates"][0]["id"]
        )

        if exact["outcome"] == "ambiguous":
            exact_ambiguous += 1
            missed_ambiguity += int(approximate["outcome"] != "ambiguous")
        outcome_changes += int(exact["outcome"] != approximate["outcome"])

        exact_margin = _margin(exact)
        approximate_margin = _margin(approximate)
        if exact_margin is not None and exact_margin <= margin:
            exact_narrow_margin += 1
            false_wide_margin += int(approximate_margin is None or approximate_margin > margin)

        exact_latency = exact.get("latency_ms")
        approximate_latency = approximate.get("latency_ms")
        if exact_latency is not None:
            exact_latencies.append(float(exact_latency))
        if approximate_latency is not None:
            approximate_latencies.append(float(approximate_latency))
        if exact_latency is not None and approximate_latency is not None:
            speedups.append(float(exact_latency) / float(approximate_latency))

    count = len(queries)
    canonical_queries = (
        {
            **query,
            "exact": {key: value for key, value in query["exact"].items() if key != "latency_ms"},
            "approximate": {key: value for key, value in query["approximate"].items() if key != "latency_ms"},
        }
        for query in queries
    )
    canonical = "\n".join(json.dumps(query, sort_keys=True, separators=(",", ":")) for query in canonical_queries)
    return {
        "schema_version": 1,
        "corpus_fingerprint": header["corpus_fingerprint"],
        "vector_revision": header["vector_revision"],
        "sidecar_checksum": header["sidecar_checksum"],
        "vector_count": header["vector_count"],
        "dimension": header["dimension"],
        "metric": header["metric"],
        "candidate_limit": header["candidate_limit"],
        "ambiguity_margin": margin,
        "query_population_fingerprint": f"sha256:{hashlib.sha256(canonical.encode()).hexdigest()}",
        "metrics": {
            "query_count": count,
            "recovered_exact_candidate_count": recovered,
            "exact_candidate_count": exact_candidate_count,
            "approximate_candidate_recall": recovered / exact_candidate_count,
            "leading_candidate_agreement_count": leading_agreement,
            "leading_candidate_agreement_rate": leading_agreement / count,
            "exact_ambiguous_count": exact_ambiguous,
            "missed_ambiguity_count": missed_ambiguity,
            "missed_ambiguity_rate": missed_ambiguity / exact_ambiguous if exact_ambiguous else None,
            "exact_narrow_margin_count": exact_narrow_margin,
            "false_wide_margin_count": false_wide_margin,
            "false_wide_margin_rate": false_wide_margin / exact_narrow_margin if exact_narrow_margin else None,
            "outcome_change_count": outcome_changes,
            "outcome_change_rate": outcome_changes / count,
            "exact_latency": _latency_summary(exact_latencies),
            "approximate_latency": _latency_summary(approximate_latencies),
            "paired_latency_count": len(speedups),
            "mean_speedup": math.fsum(speedups) / len(speedups) if speedups else None,
        },
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="shadow-comparison JSONL")
    parser.add_argument("--output", type=Path, help="write JSON here instead of standard output")
    args = parser.parse_args(argv)
    try:
        header, queries = load_study(args.input)
        rendered = json.dumps(compare(header, queries), indent=2, sort_keys=True) + "\n"
        if args.output:
            args.output.write_text(rendered, encoding="utf-8")
        else:
            sys.stdout.write(rendered)
        return 0
    except (ContractError, OSError) as error:
        print(f"comparison failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
