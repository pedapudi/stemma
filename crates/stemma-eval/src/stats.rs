//! Paired significance machinery for grading: paired randomization tests,
//! bootstrap confidence intervals, and randomised Tukey HSD for
//! multiple-comparison control (Smucker 2007; Carterette 2012). All
//! procedures are seeded and deterministic so a grade is reproducible.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Fixed seed: grading must be reproducible run to run.
const SEED: u64 = 0x5EED_57E3_3A00_0001;

/// Two-sided paired randomization (sign-flip) test on per-query differences.
/// Returns the p-value for the observed mean difference.
pub fn paired_randomization_p(diffs: &[f64], permutations: usize) -> f64 {
    if diffs.is_empty() {
        return 1.0;
    }
    let observed = mean(diffs).abs();
    if diffs.iter().all(|d| *d == 0.0) {
        return 1.0;
    }
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut at_least = 0usize;
    for _ in 0..permutations {
        let mut sum = 0.0;
        for &d in diffs {
            sum += if rng.random::<bool>() { d } else { -d };
        }
        if (sum / diffs.len() as f64).abs() >= observed - 1e-12 {
            at_least += 1;
        }
    }
    // +1 smoothing: a randomization p-value is never exactly zero.
    (at_least + 1) as f64 / (permutations + 1) as f64
}

/// Percentile bootstrap 95% CI on the mean of per-query differences.
pub fn bootstrap_ci95(diffs: &[f64], resamples: usize) -> (f64, f64) {
    if diffs.is_empty() {
        return (0.0, 0.0);
    }
    let mut rng = StdRng::seed_from_u64(SEED ^ 0xB007);
    let n = diffs.len();
    let mut means = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let mut sum = 0.0;
        for _ in 0..n {
            sum += diffs[rng.random_range(0..n)];
        }
        means.push(sum / n as f64);
    }
    means.sort_by(|a, b| a.total_cmp(b));
    let lo = means[((resamples as f64) * 0.025) as usize];
    let hi = means[(((resamples as f64) * 0.975) as usize).min(resamples - 1)];
    (lo, hi)
}

/// Randomised Tukey HSD (Carterette 2012): given per-query scores for m
/// variants (rows = queries, columns = variants, paired), returns the
/// familywise-adjusted p-value for each variant pair (i, j), i < j.
///
/// The null distribution is built by permuting each query's scores across
/// variants; the test statistic is the *maximum* pairwise mean difference,
/// so every pairwise p is automatically corrected for the m·(m−1)/2
/// comparisons.
pub fn randomized_tukey_hsd(
    scores: &[Vec<f64>],
    variants: usize,
    permutations: usize,
) -> Vec<((usize, usize), f64)> {
    let pairs: Vec<(usize, usize)> = (0..variants)
        .flat_map(|i| ((i + 1)..variants).map(move |j| (i, j)))
        .collect();
    if scores.is_empty() || variants < 2 {
        return pairs.into_iter().map(|p| (p, 1.0)).collect();
    }
    let n = scores.len() as f64;
    let observed: Vec<f64> = pairs
        .iter()
        .map(|&(i, j)| {
            (scores.iter().map(|r| r[i]).sum::<f64>() - scores.iter().map(|r| r[j]).sum::<f64>())
                .abs()
                / n
        })
        .collect();

    let mut rng = StdRng::seed_from_u64(SEED ^ 0x7CEE);
    let mut exceed = vec![0usize; pairs.len()];
    let mut perm_row: Vec<f64> = vec![0.0; variants];
    let mut sums = vec![0.0f64; variants];
    for _ in 0..permutations {
        sums.iter_mut().for_each(|s| *s = 0.0);
        for row in scores {
            perm_row.copy_from_slice(row);
            // Fisher–Yates within the query: exchangeability under the null.
            for k in (1..variants).rev() {
                let x = rng.random_range(0..=k);
                perm_row.swap(k, x);
            }
            for (s, v) in sums.iter_mut().zip(&perm_row) {
                *s += v;
            }
        }
        let max_stat = pairs
            .iter()
            .map(|&(i, j)| (sums[i] - sums[j]).abs() / n)
            .fold(0.0f64, f64::max);
        for (e, obs) in exceed.iter_mut().zip(&observed) {
            if max_stat >= *obs - 1e-12 {
                *e += 1;
            }
        }
    }
    pairs
        .into_iter()
        .zip(exceed)
        .map(|(p, e)| (p, (e + 1) as f64 / (permutations + 1) as f64))
        .collect()
}

pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// Percentile of a sample by nearest-rank (p in [0, 1]).
pub fn percentile(xs: &[f64], p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let idx = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_diffs_are_not_significant() {
        let diffs = vec![0.0; 50];
        assert_eq!(paired_randomization_p(&diffs, 1000), 1.0);
    }

    #[test]
    fn consistent_diffs_are_significant() {
        // 40 queries all improving by 0.2: about as significant as it gets.
        let diffs = vec![0.2; 40];
        let p = paired_randomization_p(&diffs, 10_000);
        assert!(p < 0.01, "p = {p}");
    }

    #[test]
    fn noise_is_not_significant() {
        // Alternating ±0.1 sums to zero.
        let diffs: Vec<f64> = (0..40).map(|i| if i % 2 == 0 { 0.1 } else { -0.1 }).collect();
        let p = paired_randomization_p(&diffs, 10_000);
        assert!(p > 0.5, "p = {p}");
    }

    #[test]
    fn bootstrap_brackets_the_mean() {
        let diffs = vec![0.1, 0.2, 0.15, 0.05, 0.12, 0.18, 0.09, 0.2, 0.11, 0.14];
        let (lo, hi) = bootstrap_ci95(&diffs, 10_000);
        let m = mean(&diffs);
        assert!(lo <= m && m <= hi, "({lo}, {hi}) should bracket {m}");
        assert!(lo > 0.0, "clearly positive effect: lo = {lo}");
    }

    #[test]
    fn tukey_separates_signal_from_noise() {
        // Variant 0 ~ variant 1; variant 2 clearly better on every query.
        let scores: Vec<Vec<f64>> = (0..40)
            .map(|i| {
                let base = 0.5 + 0.01 * ((i % 5) as f64);
                vec![base, base + if i % 2 == 0 { 0.01 } else { -0.01 }, base + 0.3]
            })
            .collect();
        let ps = randomized_tukey_hsd(&scores, 3, 5_000);
        let p01 = ps.iter().find(|(p, _)| *p == (0, 1)).unwrap().1;
        let p02 = ps.iter().find(|(p, _)| *p == (0, 2)).unwrap().1;
        assert!(p01 > 0.05, "null pair should not be significant: {p01}");
        assert!(p02 < 0.05, "strong pair should be significant: {p02}");
    }

    #[test]
    fn percentile_nearest_rank() {
        let xs = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile(&xs, 0.5), 5.0);
        assert_eq!(percentile(&xs, 0.95), 10.0);
        assert_eq!(percentile(&xs, 0.0), 1.0);
    }
}
