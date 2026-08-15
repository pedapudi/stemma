#!/usr/bin/env python3
"""Deterministic tests for the offline retrieval-policy study."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from ambit_retrieval_study import ContractError, load_study, run_study, select_candidates


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "eval" / "retrieval_study" / "sample.jsonl"


class AmbitRetrievalStudyTest(unittest.TestCase):
    def setUp(self):
        self.header, self.queries = load_study(FIXTURE)

    def test_policies_share_limit_and_use_their_evidence(self):
        selected = select_candidates(self.queries[0], self.header)
        self.assertTrue(all(len(candidates) <= 3 for candidates in selected.values()))
        self.assertEqual(selected["existing_bounded"], ["gold", "distractor"])
        self.assertEqual(selected["deeper_fixed"], ["gold", "distractor", "alternative"])
        self.assertEqual(selected["score_margin"], ["gold", "distractor"])
        self.assertEqual(selected["graph_directed"], ["gold", "distractor", "alternative"])
        self.assertEqual(selected["ambit_directed"], ["gold", "distractor", "alternative"])
        self.assertEqual(selected["combined_bounded"], ["gold", "distractor", "alternative"])

    def test_results_are_split_by_query_kind(self):
        result = run_study(self.header, self.queries)
        record = result["results"]["record_as_query"]
        free_form = result["results"]["free_form"]
        self.assertEqual(record["existing_bounded"]["query_count"], 2)
        self.assertEqual(record["existing_bounded"]["gold_survival_rate"], 0.5)
        self.assertEqual(record["graph_directed"]["ambiguity_localization_rate"], 1.0)
        self.assertEqual(free_form["existing_bounded"]["false_commitment_rate"], 1.0)
        self.assertEqual(free_form["ambit_directed"]["supported_alternative_recall"], 1.0)
        self.assertEqual(record["existing_bounded"]["mean_latency_ms"], 2.5)

    def test_output_is_deterministic(self):
        first = json.dumps(run_study(self.header, self.queries), sort_keys=True)
        second = json.dumps(run_study(self.header, self.queries), sort_keys=True)
        self.assertEqual(first, second)
        self.assertRegex(json.loads(first)["query_population_fingerprint"], r"^sha256:[0-9a-f]{64}$")

        without_latency = [
            {key: value for key, value in query.items() if key != "latency_ms"}
            for query in self.queries
        ]
        self.assertEqual(
            json.loads(first)["query_population_fingerprint"],
            run_study(self.header, without_latency)["query_population_fingerprint"],
        )

    def test_absent_query_kind_has_null_rates(self):
        result = run_study(self.header, self.queries[:2])
        metrics = result["results"]["free_form"]["existing_bounded"]
        self.assertEqual(metrics["query_count"], 0)
        self.assertIsNone(metrics["gold_survival_rate"])
        self.assertIsNone(metrics["mean_added_candidates"])

    def test_fingerprints_are_required(self):
        lines = FIXTURE.read_text().splitlines()
        header = json.loads(lines[0])
        del header["vector_fingerprint"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "invalid.jsonl"
            path.write_text("\n".join([json.dumps(header), *lines[1:]]) + "\n")
            with self.assertRaisesRegex(ContractError, "vector_fingerprint is required"):
                load_study(path)


if __name__ == "__main__":
    unittest.main()
