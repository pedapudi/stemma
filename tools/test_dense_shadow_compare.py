#!/usr/bin/env python3
"""Deterministic tests for dense shadow comparison."""

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from dense_shadow_compare import ContractError, compare, load_study


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "eval" / "dense_shadow" / "sample.jsonl"


class DenseShadowCompareTest(unittest.TestCase):
    def setUp(self):
        self.header, self.queries = load_study(FIXTURE)

    def test_fidelity_metrics_use_exact_results_as_oracle(self):
        metrics = compare(self.header, self.queries)["metrics"]
        self.assertEqual(metrics["query_count"], 4)
        self.assertEqual(metrics["recovered_exact_candidate_count"], 5)
        self.assertEqual(metrics["exact_candidate_count"], 11)
        self.assertAlmostEqual(metrics["approximate_candidate_recall"], 5 / 11)
        self.assertEqual(metrics["leading_candidate_agreement_rate"], 0.5)
        self.assertEqual(metrics["missed_ambiguity_rate"], 0.5)
        self.assertEqual(metrics["false_wide_margin_rate"], 1.0)
        self.assertEqual(metrics["outcome_change_rate"], 0.5)

    def test_latency_summary_uses_only_available_measurements(self):
        metrics = compare(self.header, self.queries)["metrics"]
        self.assertEqual(metrics["exact_latency"]["sample_count"], 3)
        self.assertEqual(metrics["exact_latency"]["p50_ms"], 12.0)
        self.assertEqual(metrics["exact_latency"]["p95_ms"], 20.0)
        self.assertEqual(metrics["approximate_latency"]["p50_ms"], 4.0)
        self.assertEqual(metrics["paired_latency_count"], 2)
        self.assertEqual(metrics["mean_speedup"], 4.0)

    def test_population_fingerprint_ignores_latency(self):
        self.assertEqual(compare(self.header, self.queries), compare(self.header, self.queries))
        first = compare(self.header, self.queries)["query_population_fingerprint"]
        without_latency = copy.deepcopy(self.queries)
        for query in without_latency:
            query["exact"].pop("latency_ms", None)
            query["approximate"].pop("latency_ms", None)
        second = compare(self.header, without_latency)["query_population_fingerprint"]
        self.assertEqual(first, second)

    def test_shared_candidates_require_authoritative_rescoring(self):
        invalid = copy.deepcopy(self.queries)
        invalid[0]["approximate"]["candidates"][0]["similarity"] -= 0.01
        lines = [json.dumps(self.header), *(json.dumps(query) for query in invalid)]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "invalid.jsonl"
            path.write_text("\n".join(lines) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(ContractError, "authoritative rescoring"):
                load_study(path)


if __name__ == "__main__":
    unittest.main()
