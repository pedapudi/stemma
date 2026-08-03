//! The `grade` subcommand: compare a run to the accepted baseline under
//! 07-eval-harness.md's rules and exit nonzero on failure, printing the
//! named failures. The 1-point guard is a floor, not a license — a smaller
//! drop that a paired randomization test marks significant still fails.
//!
//! Checks implemented (07 "How a run is graded"):
//! 1. no cell regresses (>1 point of column-strict recall@5, or any
//!    significant drop at α = 0.05);
//! 2. tier-mechanism containment: off-target cells move <1 point in either
//!    direction between consecutive ablations;
//! 3. NIL precision does not drop; confident-wrongs are named in the run
//!    file's NIL panel;
//! 4. cost envelopes hold (p95 latency per tier, adjudication routing rate)
//!    against the budgets stored next to the baseline numbers.
//! Rule 5 (layer-3 agent-grounding regression cases) is separately gated
//! and not part of this binary — an honest deviation recorded in 07.

use std::path::Path;

use anyhow::Context;

use crate::runner::{target_tiers, Baseline, Failure, RunFile};
use crate::stats;

const POINT: f64 = 0.01; // one point of recall
const ALPHA: f64 = 0.05;

/// All named failures of `run` against `baseline`.
pub fn check(run: &RunFile, baseline: &Baseline, permutations: usize) -> Vec<Failure> {
    let mut failures = Vec::new();

    // 1. Cell regressions vs baseline.
    for (ab, tiers) in &run.cells {
        let Some(base_tiers) = baseline.cells.get(ab) else {
            continue;
        };
        for (tier, cell) in tiers {
            let Some(base) = base_tiers.get(tier) else {
                continue;
            };
            let diffs: Vec<f64> = cell
                .per_query
                .iter()
                .filter_map(|(id, v)| base.per_query.get(id).map(|b| v - b))
                .collect();
            if diffs.is_empty() {
                continue;
            }
            let drop = -stats::mean(&diffs);
            let p = stats::paired_randomization_p(&diffs, permutations);
            let significant_drop = drop > 0.0 && p < ALPHA;
            if drop > POINT || significant_drop {
                let worst: Vec<String> = cell
                    .per_query
                    .iter()
                    .filter(|(id, v)| {
                        base.per_query.get(*id).is_some_and(|b| **v < *b)
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                failures.push(Failure {
                    check: "cell-regression".into(),
                    cell: format!("{ab} × {tier}"),
                    detail: format!(
                        "recall@5 dropped {:.1} points vs baseline (p = {:.4}, n = {}){}",
                        drop * 100.0,
                        p,
                        diffs.len(),
                        if significant_drop && drop <= POINT {
                            " — under the 1-point floor but statistically significant"
                        } else {
                            ""
                        }
                    ),
                    queries: worst,
                });
            }
        }
    }

    // 2. Tier-mechanism containment between consecutive ablations, off-target
    //    tiers only — movement in EITHER direction is a failure (upward
    //    off-target drift means the mechanism fires without real evidence).
    for (ab, tiers) in &run.cells {
        let Some(targets) = target_tiers(ab) else {
            continue;
        };
        for (tier, cell) in tiers {
            if targets.contains(&tier.as_str()) {
                continue;
            }
            if let Some(d) = &cell.delta_prev {
                if d.mean.abs() >= POINT {
                    failures.push(Failure {
                        check: "containment".into(),
                        cell: format!("{ab} × {tier}"),
                        detail: format!(
                            "off-target cell moved {:+.1} points ({}); {} targets {:?}",
                            d.mean * 100.0,
                            d.vs,
                            ab,
                            targets
                        ),
                        queries: Vec::new(),
                    });
                }
            }
        }
    }

    // 3. NIL precision must not drop.
    for (ab, nil) in &run.nil {
        let base = baseline.nil_precision.get(ab).copied().flatten();
        if let (Some(now), Some(before)) = (nil.precision, base) {
            if now < before {
                failures.push(Failure {
                    check: "nil-precision".into(),
                    cell: ab.clone(),
                    detail: format!("NIL precision {now:.3} < baseline {before:.3}"),
                    queries: nil.confident_wrong.iter().map(|c| c.id.clone()).collect(),
                });
            }
        }
    }

    // 4. Cost envelopes.
    for (ab, tiers) in &run.cells {
        for (tier, cell) in tiers {
            if let Some(budget) = baseline.budgets.p95_latency_ms.get(tier) {
                if cell.cell.latency_p95_ms > *budget {
                    failures.push(Failure {
                        check: "latency-budget".into(),
                        cell: format!("{ab} × {tier}"),
                        detail: format!(
                            "p95 latency {:.0} ms exceeds budget {:.0} ms",
                            cell.cell.latency_p95_ms, budget
                        ),
                        queries: Vec::new(),
                    });
                }
            }
            if cell.cell.adjudication_rate > baseline.budgets.adjudication_rate_max {
                failures.push(Failure {
                    check: "adjudication-budget".into(),
                    cell: format!("{ab} × {tier}"),
                    detail: format!(
                        "adjudication routing rate {:.3} exceeds budget {:.3}",
                        cell.cell.adjudication_rate, baseline.budgets.adjudication_rate_max
                    ),
                    queries: Vec::new(),
                });
            }
        }
    }

    failures
}

/// CLI wrapper: load, check, print, exit nonzero on failure.
pub fn grade(run_path: &Path, baseline_path: &Path, permutations: usize) -> anyhow::Result<bool> {
    let run: RunFile = serde_json::from_str(
        &std::fs::read_to_string(run_path)
            .with_context(|| format!("reading run {}", run_path.display()))?,
    )
    .with_context(|| format!("parsing run {}", run_path.display()))?;
    let baseline: Baseline = serde_json::from_str(
        &std::fs::read_to_string(baseline_path)
            .with_context(|| format!("reading baseline {}", baseline_path.display()))?,
    )
    .with_context(|| format!("parsing baseline {}", baseline_path.display()))?;
    anyhow::ensure!(
        run.corpus == baseline.corpus,
        "run corpus {:?} does not match baseline corpus {:?}",
        run.corpus,
        baseline.corpus
    );
    let failures = check(&run, &baseline, permutations);
    if failures.is_empty() {
        println!(
            "PASS: {} vs baseline {} ({} ablations, tiers {:?})",
            run.run_id, baseline.run_id, run.ablations.len(), run.tiers
        );
        return Ok(true);
    }
    println!("FAIL: {} vs baseline {}", run.run_id, baseline.run_id);
    for f in &failures {
        println!("  [{}] {}: {}", f.check, f.cell, f.detail);
        for q in f.queries.iter().take(8) {
            println!("      - {q}");
        }
        if f.queries.len() > 8 {
            println!("      … {} more", f.queries.len() - 8);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Cell;
    use crate::runner::{Baseline, BaselineCell, Budgets, CellReport, Delta, NilReport};
    use std::collections::BTreeMap;

    fn cell(r5: f64, per_query: &[(&str, f64)]) -> CellReport {
        CellReport {
            cell: Cell {
                n: per_query.len(),
                n_targets: per_query.len(),
                r1_loose: r5,
                r5_loose: r5,
                rinf_loose: r5,
                r1_strict: r5,
                r5_strict: r5,
                rinf_strict: r5,
                mrr: r5,
                grounded: r5,
                mention_f_strict_micro: 0.0,
                mention_f_weak_micro: 0.0,
                mention_f_strict_macro: 0.0,
                mention_f_weak_macro: 0.0,
                latency_median_ms: 10.0,
                latency_p95_ms: 20.0,
                dense_probes_mean: 0.0,
                adjudication_rate: 0.0,
                selected_per_mention: 1.0,
            },
            delta_prev: None,
            delta_baseline: None,
            per_query: per_query
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect(),
            queries: Vec::new(),
        }
    }

    fn base_cell(r5: f64, per_query: &[(&str, f64)]) -> BaselineCell {
        BaselineCell {
            r5_strict: r5,
            grounded: r5,
            n: per_query.len(),
            per_query: per_query
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect(),
        }
    }

    fn scaffold(run_r5: f64, base_r5: f64, n: usize) -> (RunFile, Baseline) {
        let run_pq: Vec<(String, f64)> =
            (0..n).map(|i| (format!("q{i}"), run_r5)).collect();
        let base_pq: Vec<(String, f64)> =
            (0..n).map(|i| (format!("q{i}"), base_r5)).collect();
        let run_pq_ref: Vec<(&str, f64)> =
            run_pq.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let base_pq_ref: Vec<(&str, f64)> =
            base_pq.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let run = RunFile {
            run_id: "r".into(),
            corpus: "c".into(),
            dataset: "d".into(),
            git_rev: "g".into(),
            date: "now".into(),
            ablations: vec!["lex".into()],
            tiers: vec!["L1".into()],
            cells: BTreeMap::from([(
                "lex".to_string(),
                BTreeMap::from([("L1".to_string(), cell(run_r5, &run_pq_ref))]),
            )]),
            nil: BTreeMap::from([("lex".to_string(), NilReport::default())]),
            calibration: BTreeMap::new(),
            backend_cost: BTreeMap::new(),
            tukey: BTreeMap::new(),
            pass: None,
            failures: Vec::new(),
            notes: Vec::new(),
        };
        let baseline = Baseline {
            corpus: "c".into(),
            dataset: "d".into(),
            run_id: "b".into(),
            git_rev: "g".into(),
            date: "then".into(),
            cells: BTreeMap::from([(
                "lex".to_string(),
                BTreeMap::from([("L1".to_string(), base_cell(base_r5, &base_pq_ref))]),
            )]),
            nil_precision: BTreeMap::from([("lex".to_string(), None)]),
            budgets: Budgets {
                p95_latency_ms: BTreeMap::from([("L1".to_string(), 1000.0)]),
                adjudication_rate_max: 0.5,
            },
        };
        (run, baseline)
    }

    #[test]
    fn equal_run_passes() {
        let (run, baseline) = scaffold(0.6, 0.6, 40);
        assert!(check(&run, &baseline, 2000).is_empty());
    }

    #[test]
    fn small_consistent_drop_fails_significance_even_under_the_floor() {
        // 0.8-point drop on every query: under the 1-point floor, but a
        // paired randomization test flags it — the floor is not a license.
        let (run, baseline) = scaffold(0.592, 0.60, 250);
        let failures = check(&run, &baseline, 5000);
        assert!(
            failures.iter().any(|f| f.check == "cell-regression"),
            "expected a significance failure, got {failures:?}"
        );
    }

    #[test]
    fn big_drop_fails_the_floor() {
        let (run, baseline) = scaffold(0.55, 0.60, 20);
        let failures = check(&run, &baseline, 2000);
        assert!(failures.iter().any(|f| f.check == "cell-regression"));
    }

    #[test]
    fn improvements_pass() {
        let (run, baseline) = scaffold(0.70, 0.60, 40);
        assert!(check(&run, &baseline, 2000).is_empty());
    }

    #[test]
    fn off_target_movement_fails_containment_both_directions() {
        for direction in [0.02, -0.02] {
            let (mut run, baseline) = scaffold(0.6, 0.6, 10);
            // Rebrand the ablation as +dense (target tier: L2 only) and give
            // its L1 cell a delta_prev exceeding one point.
            let cells = run.cells.remove("lex").unwrap();
            run.cells.insert("+dense".into(), cells);
            run.ablations = vec!["lex".into(), "+dense".into()];
            let c = run
                .cells
                .get_mut("+dense")
                .unwrap()
                .get_mut("L1")
                .unwrap();
            c.delta_prev = Some(Delta {
                vs: "prev:lex".into(),
                mean: direction,
                ci: [direction - 0.01, direction + 0.01],
                p: 0.01,
                n: 10,
            });
            let failures = check(&run, &baseline, 500);
            assert!(
                failures.iter().any(|f| f.check == "containment"),
                "direction {direction}: {failures:?}"
            );
        }
    }

    #[test]
    fn latency_budget_violation_fails() {
        let (mut run, baseline) = scaffold(0.6, 0.6, 10);
        run.cells.get_mut("lex").unwrap().get_mut("L1").unwrap().cell.latency_p95_ms = 5000.0;
        let failures = check(&run, &baseline, 500);
        assert!(failures.iter().any(|f| f.check == "latency-budget"));
    }

    #[test]
    fn nil_precision_drop_fails() {
        let (mut run, mut baseline) = scaffold(0.6, 0.6, 10);
        baseline.nil_precision.insert("lex".into(), Some(0.9));
        run.nil.get_mut("lex").unwrap().precision = Some(0.7);
        let failures = check(&run, &baseline, 500);
        assert!(failures.iter().any(|f| f.check == "nil-precision"));
    }
}
