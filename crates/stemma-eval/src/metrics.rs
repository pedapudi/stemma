//! Layer-1 metric computation (docs/design/07-eval-harness.md): per-query
//! scoring of a resolution trace against denotation-verified targets, and
//! per-(tier × ablation) cell aggregation.
//!
//! Matching strictness follows the design:
//! - value-loose — the candidate's stored value equals the target literal
//!   after normalization (or, for document candidates, contains it);
//! - column-strict — the candidate sits in the gold column AND its rowid is
//!   in the denotation-verified gold set. The value-loose/column-strict gap
//!   is the measured coincidence rate (Zhong 2020).
//! Mention F is reported strict (byte-identical span) and weak (overlap),
//! micro and macro, at β = 2 (Röder 2018; recall-weighted per 06).

use serde::{Deserialize, Serialize};
use stemma_resolve::Trace;

use crate::dataset::{EvalQuestion, Target};

/// Per-target scoring detail, kept for the report's drill-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetOutcome {
    pub table: String,
    pub column: String,
    pub literal: String,
    pub loose_rank: Option<usize>,
    pub strict_rank: Option<usize>,
    /// Whether a *selected* candidate links the gold row (table + rowid).
    pub linked: bool,
    /// Top candidate of the best-matching mention, for failure listings.
    pub best_candidate: Option<String>,
}

/// Everything measured about one query under one ablation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOutcome {
    pub id: String,
    pub tier: String,
    pub question: String,
    pub n_targets: usize,
    // Fractions of this query's targets hit at k (value-loose / col-strict).
    pub r1_loose: f64,
    pub r5_loose: f64,
    pub rinf_loose: f64,
    pub r1_strict: f64,
    pub r5_strict: f64,
    pub rinf_strict: f64,
    /// Mean reciprocal rank of the gold row across targets (column-strict).
    pub mrr: f64,
    /// The conjunctive headline: every target linked by a selected candidate
    /// (absent-tier queries: absence affirmed).
    pub grounded: bool,
    /// The resolver produced no confident mention (empty, weak, or nothing
    /// selected) — the affirmative-absence outcome.
    pub nil_outcome: bool,
    // Mention detection counts (targets with a locatable literal only).
    pub gold_spans: usize,
    pub pred_spans: usize,
    pub strict_gold_matched: usize,
    pub weak_gold_matched: usize,
    pub strict_pred_matched: usize,
    pub weak_pred_matched: usize,
    // Cost.
    pub latency_ms: f64,
    pub dense_probes: usize,
    pub adjudicated_mentions: usize,
    pub mentions: usize,
    pub selected_candidates: usize,
    /// (score, is_gold_row) for every selected candidate — calibration input.
    pub calibration: Vec<(f64, bool)>,
    pub targets: Vec<TargetOutcome>,
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_wildcards(s: &str) -> String {
    s.replace(['%', '_'], " ")
}

/// Loose value match: normalized equality, containment for documents, and
/// containment of the wildcard-stripped literal for LIKE targets.
fn value_matches(target: &Target, value: &str, is_doc: bool) -> bool {
    let v = norm(value);
    match target.match_mode.as_str() {
        "like" => {
            let lit = norm(&strip_wildcards(&target.literal));
            !lit.is_empty() && v.contains(&lit)
        }
        "doc" => true, // doc targets are row-identity targets; value is moot
        _ => {
            let lit = norm(&target.literal);
            v == lit || (is_doc && v.contains(&lit))
        }
    }
}

/// Column-strict gold-row membership. `probe` answers membership for
/// truncated rowid sets (a LIMIT-1 query against the user database).
fn is_gold_row(
    target: &Target,
    table: &str,
    column: &str,
    rowid: i64,
    probe: &mut dyn FnMut(&Target, i64) -> bool,
) -> bool {
    if !table.eq_ignore_ascii_case(&target.table) {
        return false;
    }
    // Doc targets are row targets: the whole row is gold, whichever indexed
    // column surfaced it. Value targets require the gold column.
    if target.match_mode != "doc" && !column.eq_ignore_ascii_case(&target.column) {
        return false;
    }
    if target.rowids.contains(&rowid) {
        return true;
    }
    target.rowids_truncated && probe(target, rowid)
}

/// Row-level link for the grounded headline: gold table + rowid, any column
/// (the record is right even if it surfaced via a sibling column).
fn is_gold_record(
    target: &Target,
    table: &str,
    rowid: i64,
    probe: &mut dyn FnMut(&Target, i64) -> bool,
) -> bool {
    if !table.eq_ignore_ascii_case(&target.table) {
        return false;
    }
    if target.rowids.contains(&rowid) {
        return true;
    }
    target.rowids_truncated && probe(target, rowid)
}

/// Locates the gold span for a target literal in the question, by normalized
/// case-insensitive search. Returns byte offsets. Targets whose literal does
/// not appear in the question (oblique mentions) contribute no gold span and
/// are excluded from mention-detection scoring.
pub fn locate_gold_span(question: &str, target: &Target) -> Option<(usize, usize)> {
    let needle_raw = if target.match_mode == "like" {
        strip_wildcards(&target.literal).trim().to_string()
    } else {
        target.literal.trim().to_string()
    };
    if needle_raw.is_empty() {
        return None;
    }
    let hay = question.to_lowercase();
    let needle = needle_raw.to_lowercase();
    // to_lowercase can change byte lengths for non-ASCII; map back through
    // char indices to stay safe.
    if hay.len() == question.len() {
        return hay.find(&needle).map(|i| (i, i + needle.len()));
    }
    // Rare non-ASCII path: scan char windows.
    let chars: Vec<(usize, char)> = question.char_indices().collect();
    let nchars = needle.chars().count();
    for w in 0..chars.len() {
        if w + nchars > chars.len() {
            break;
        }
        let start = chars[w].0;
        let end = chars
            .get(w + nchars)
            .map(|c| c.0)
            .unwrap_or(question.len());
        if question[start..end].to_lowercase() == needle {
            return Some((start, end));
        }
    }
    None
}

/// Scores one trace against one question. `probe` resolves gold-row
/// membership for truncated rowid sets; `full_value` fetches the untruncated
/// stored value of a candidate (the trace truncates at 160 chars).
pub fn score_query(
    q: &EvalQuestion,
    trace: &Trace,
    probe: &mut dyn FnMut(&Target, i64) -> bool,
    full_value: &mut dyn FnMut(&str, &str, i64) -> Option<String>,
) -> QueryOutcome {
    let mention_spans: Vec<&stemma_resolve::Span> =
        trace.mentions.iter().map(|&i| &trace.spans[i]).collect();

    // The affirmative-absence outcome: no mention carries a selected
    // candidate (spans demoted to weak by adjudication nil included).
    let nil_outcome = mention_spans
        .iter()
        .all(|s| s.status != "selected" || !s.candidates.iter().any(|c| c.selected));

    // ----- per-target retrieval scoring -----
    let mut touts = Vec::new();
    let (mut h1l, mut h5l, mut hil) = (0usize, 0usize, 0usize);
    let (mut h1s, mut h5s, mut his) = (0usize, 0usize, 0usize);
    let mut rr_sum = 0.0;
    let mut all_linked = true;
    for target in &q.targets {
        let mut loose_rank: Option<usize> = None;
        let mut strict_rank: Option<usize> = None;
        let mut linked = false;
        // Ranked scan within each mention span (k = per-mention rank).
        for span in &mention_spans {
            for (rank, c) in span.candidates.iter().enumerate() {
                let val = if c.value_truncated {
                    full_value(&c.table, &c.column, c.rowid).unwrap_or_else(|| c.value.clone())
                } else {
                    c.value.clone()
                };
                if value_matches(target, &val, c.is_doc)
                    && loose_rank.is_none_or(|r| rank < r)
                {
                    loose_rank = Some(rank);
                }
                if is_gold_row(target, &c.table, &c.column, c.rowid, probe)
                    && strict_rank.is_none_or(|r| rank < r)
                {
                    strict_rank = Some(rank);
                }
                if c.selected && is_gold_record(target, &c.table, c.rowid, probe) {
                    linked = true;
                }
            }
        }
        // Unbounded recall looks at the full traced candidate set — every
        // span, selected or not (06: near-misses are the diagnostic).
        let mut loose_any = loose_rank.is_some();
        let mut strict_any = strict_rank.is_some();
        if !loose_any || !strict_any {
            'outer: for span in &trace.spans {
                for c in &span.candidates {
                    if !loose_any {
                        let val = if c.value_truncated {
                            full_value(&c.table, &c.column, c.rowid)
                                .unwrap_or_else(|| c.value.clone())
                        } else {
                            c.value.clone()
                        };
                        if value_matches(target, &val, c.is_doc) {
                            loose_any = true;
                        }
                    }
                    if !strict_any && is_gold_row(target, &c.table, &c.column, c.rowid, probe) {
                        strict_any = true;
                    }
                    if loose_any && strict_any {
                        break 'outer;
                    }
                }
            }
        }
        if loose_rank.is_some_and(|r| r < 1) {
            h1l += 1;
        }
        if loose_rank.is_some_and(|r| r < 5) {
            h5l += 1;
        }
        if loose_any {
            hil += 1;
        }
        if strict_rank.is_some_and(|r| r < 1) {
            h1s += 1;
        }
        if strict_rank.is_some_and(|r| r < 5) {
            h5s += 1;
        }
        if strict_any {
            his += 1;
        }
        if let Some(r) = strict_rank {
            rr_sum += 1.0 / (r as f64 + 1.0);
        }
        if !linked {
            all_linked = false;
        }
        let best = mention_spans
            .iter()
            .flat_map(|s| s.candidates.first())
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .map(|c| format!("{}.{} #{} — {}", c.table, c.column, c.rowid, c.value));
        touts.push(TargetOutcome {
            table: target.table.clone(),
            column: target.column.clone(),
            literal: target.literal.clone(),
            loose_rank,
            strict_rank,
            linked,
            best_candidate: best,
        });
    }
    let nt = q.targets.len().max(1) as f64;

    // ----- mention detection -----
    let gold_spans: Vec<(usize, usize)> = q
        .targets
        .iter()
        .filter_map(|t| locate_gold_span(&q.question, t))
        .collect();
    let pred: Vec<(usize, usize)> = mention_spans
        .iter()
        .filter(|s| s.status == "selected")
        .map(|s| (s.start, s.end))
        .collect();
    let overlap = |a: (usize, usize), b: (usize, usize)| a.0 < b.1 && b.0 < a.1;
    let strict_gold_matched = gold_spans.iter().filter(|g| pred.contains(g)).count();
    let weak_gold_matched = gold_spans
        .iter()
        .filter(|g| pred.iter().any(|p| overlap(**g, *p)))
        .count();
    let strict_pred_matched = pred.iter().filter(|p| gold_spans.contains(p)).count();
    let weak_pred_matched = pred
        .iter()
        .filter(|p| gold_spans.iter().any(|g| overlap(**p, *g)))
        .count();

    // ----- headline -----
    let grounded = if q.nil {
        nil_outcome
    } else {
        !q.targets.is_empty() && all_linked && !nil_outcome
    };

    // ----- cost + calibration -----
    let dense_probes = trace
        .spans
        .iter()
        .filter(|s| {
            s.candidates
                .iter()
                .any(|c| c.channels.iter().any(|ch| ch.channel == "dense"))
        })
        .count();
    let adjudicated_mentions = mention_spans
        .iter()
        .filter(|s| s.candidates.iter().any(|c| c.adjudicated))
        .count();
    let selected_candidates: usize = mention_spans
        .iter()
        .map(|s| s.candidates.iter().filter(|c| c.selected).count())
        .sum();
    let mut calibration = Vec::new();
    for span in &mention_spans {
        for c in span.candidates.iter().filter(|c| c.selected) {
            let gold = q
                .targets
                .iter()
                .any(|t| is_gold_row(t, &c.table, &c.column, c.rowid, probe));
            calibration.push((c.score, gold));
        }
    }

    QueryOutcome {
        id: q.id.clone(),
        tier: q.tier.clone(),
        question: q.question.clone(),
        n_targets: q.targets.len(),
        r1_loose: h1l as f64 / nt,
        r5_loose: h5l as f64 / nt,
        rinf_loose: hil as f64 / nt,
        r1_strict: h1s as f64 / nt,
        r5_strict: h5s as f64 / nt,
        rinf_strict: his as f64 / nt,
        mrr: rr_sum / nt,
        grounded,
        nil_outcome,
        gold_spans: gold_spans.len(),
        pred_spans: pred.len(),
        strict_gold_matched,
        weak_gold_matched,
        strict_pred_matched,
        weak_pred_matched,
        latency_ms: trace.elapsed_ms,
        dense_probes,
        adjudicated_mentions,
        mentions: mention_spans.len(),
        selected_candidates,
        calibration,
        targets: touts,
    }
}

/// F<sub>β</sub> with β = 2 — recall weighted four times precision (06).
pub fn f_beta2(precision: f64, recall: f64) -> f64 {
    if precision + recall == 0.0 {
        return 0.0;
    }
    5.0 * precision * recall / (4.0 * precision + recall)
}

/// One aggregated (tier × ablation) cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cell {
    pub n: usize,
    pub n_targets: usize,
    // Micro (over targets) recall at each k, both strictnesses.
    pub r1_loose: f64,
    pub r5_loose: f64,
    pub rinf_loose: f64,
    pub r1_strict: f64,
    pub r5_strict: f64,
    pub rinf_strict: f64,
    pub mrr: f64,
    pub grounded: f64,
    // Mention detection F (β = 2).
    pub mention_f_strict_micro: f64,
    pub mention_f_weak_micro: f64,
    pub mention_f_strict_macro: f64,
    pub mention_f_weak_macro: f64,
    // Cost.
    pub latency_median_ms: f64,
    pub latency_p95_ms: f64,
    pub dense_probes_mean: f64,
    pub adjudication_rate: f64,
    pub selected_per_mention: f64,
}

pub fn aggregate(outcomes: &[&QueryOutcome]) -> Cell {
    let n = outcomes.len();
    let nt: usize = outcomes.iter().map(|o| o.n_targets).sum();
    let ntf = nt.max(1) as f64;
    let micro = |f: &dyn Fn(&QueryOutcome) -> f64| {
        outcomes
            .iter()
            .map(|o| f(o) * o.n_targets as f64)
            .sum::<f64>()
            / ntf
    };
    let mean = |f: &dyn Fn(&QueryOutcome) -> f64| {
        if n == 0 {
            0.0
        } else {
            outcomes.iter().map(|o| f(o)).sum::<f64>() / n as f64
        }
    };
    // Micro mention F: pool counts across queries.
    let sg: usize = outcomes.iter().map(|o| o.gold_spans).sum();
    let sp: usize = outcomes.iter().map(|o| o.pred_spans).sum();
    let f_micro = |gm: usize, pm: usize| {
        let recall = if sg == 0 { 0.0 } else { gm as f64 / sg as f64 };
        let precision = if sp == 0 { 0.0 } else { pm as f64 / sp as f64 };
        f_beta2(precision, recall)
    };
    let f_macro = |gm: &dyn Fn(&QueryOutcome) -> usize, pm: &dyn Fn(&QueryOutcome) -> usize| {
        let scored: Vec<f64> = outcomes
            .iter()
            .filter(|o| o.gold_spans > 0)
            .map(|o| {
                let recall = gm(o) as f64 / o.gold_spans as f64;
                let precision = if o.pred_spans == 0 {
                    0.0
                } else {
                    pm(o) as f64 / o.pred_spans as f64
                };
                f_beta2(precision, recall)
            })
            .collect();
        crate::stats::mean(&scored)
    };
    let lat: Vec<f64> = outcomes.iter().map(|o| o.latency_ms).collect();
    let mentions: usize = outcomes.iter().map(|o| o.mentions).sum();
    let adjudicated: usize = outcomes.iter().map(|o| o.adjudicated_mentions).sum();
    let selected: usize = outcomes.iter().map(|o| o.selected_candidates).sum();
    Cell {
        n,
        n_targets: nt,
        r1_loose: micro(&|o| o.r1_loose),
        r5_loose: micro(&|o| o.r5_loose),
        rinf_loose: micro(&|o| o.rinf_loose),
        r1_strict: micro(&|o| o.r1_strict),
        r5_strict: micro(&|o| o.r5_strict),
        rinf_strict: micro(&|o| o.rinf_strict),
        mrr: mean(&|o| o.mrr),
        grounded: mean(&|o| if o.grounded { 1.0 } else { 0.0 }),
        mention_f_strict_micro: f_micro(
            outcomes.iter().map(|o| o.strict_gold_matched).sum(),
            outcomes.iter().map(|o| o.strict_pred_matched).sum(),
        ),
        mention_f_weak_micro: f_micro(
            outcomes.iter().map(|o| o.weak_gold_matched).sum(),
            outcomes.iter().map(|o| o.weak_pred_matched).sum(),
        ),
        mention_f_strict_macro: f_macro(&|o| o.strict_gold_matched, &|o| o.strict_pred_matched),
        mention_f_weak_macro: f_macro(&|o| o.weak_gold_matched, &|o| o.weak_pred_matched),
        latency_median_ms: crate::stats::percentile(&lat, 0.5),
        latency_p95_ms: crate::stats::percentile(&lat, 0.95),
        dense_probes_mean: mean(&|o| o.dense_probes as f64),
        adjudication_rate: if mentions == 0 {
            0.0
        } else {
            adjudicated as f64 / mentions as f64
        },
        selected_per_mention: if mentions == 0 {
            0.0
        } else {
            selected as f64 / mentions as f64
        },
    }
}

/// Calibration curve: P(candidate links a gold row | fused-score bucket),
/// ten buckets over [0, 1].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationBucket {
    pub lo: f64,
    pub hi: f64,
    pub n: usize,
    pub p_gold: f64,
}

pub fn calibration_curve(outcomes: &[&QueryOutcome]) -> Vec<CalibrationBucket> {
    let mut buckets = vec![(0usize, 0usize); 10];
    for o in outcomes {
        for &(score, gold) in &o.calibration {
            let b = ((score * 10.0) as usize).min(9);
            buckets[b].0 += 1;
            if gold {
                buckets[b].1 += 1;
            }
        }
    }
    buckets
        .into_iter()
        .enumerate()
        .map(|(i, (n, g))| CalibrationBucket {
            lo: i as f64 / 10.0,
            hi: (i + 1) as f64 / 10.0,
            n,
            p_gold: if n == 0 { 0.0 } else { g as f64 / n as f64 },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::Target;

    fn target(literal: &str) -> Target {
        Target {
            table: "offices".into(),
            column: "city".into(),
            literal: literal.into(),
            match_mode: "exact".into(),
            rowids: vec![17],
            rowids_truncated: false,
        }
    }

    #[test]
    fn value_matching_modes() {
        let t = target("Seattle");
        assert!(value_matches(&t, "  seattle ", false));
        assert!(!value_matches(&t, "seattle office", false));
        assert!(value_matches(&t, "the Seattle office report", true)); // doc containment
        let like = Target {
            match_mode: "like".into(),
            literal: "%Chen%".into(),
            ..target("")
        };
        assert!(value_matches(&like, "Wei Chen", false));
        assert!(!value_matches(&like, "Wei Ching", false));
    }

    #[test]
    fn gold_span_location() {
        let t = target("Seattle");
        let q = "the Q3 numbers for the seattle office";
        assert_eq!(locate_gold_span(q, &t), Some((23, 30)));
        assert_eq!(locate_gold_span("no match here", &t), None);
    }

    #[test]
    fn f_beta2_weights_recall() {
        // recall 1.0, precision 0.5 scores far above recall 0.5, precision 1.0
        assert!(f_beta2(0.5, 1.0) > f_beta2(1.0, 0.5));
        assert_eq!(f_beta2(0.0, 0.0), 0.0);
    }

    #[test]
    fn truncated_rowid_sets_probe_the_db() {
        let t = Target {
            rowids: vec![1, 2],
            rowids_truncated: true,
            ..target("Seattle")
        };
        let probed = std::cell::Cell::new(false);
        let mut probe = |_t: &Target, rowid: i64| {
            probed.set(true);
            rowid == 99
        };
        assert!(is_gold_row(&t, "offices", "city", 2, &mut probe));
        assert!(is_gold_row(&t, "offices", "city", 99, &mut probe));
        assert!(probed.get());
        assert!(!is_gold_row(&t, "offices", "city", 3, &mut probe));
        assert!(!is_gold_row(&t, "other", "city", 2, &mut probe));
    }
}
