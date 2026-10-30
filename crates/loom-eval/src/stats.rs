//! Bootstrap confidence intervals and paired difference statistics.

/// Default number of bootstrap resamples.
pub const RESAMPLES: usize = 1000;
/// Default PRNG seed for deterministic bootstrap runs.
pub const SEED: u64 = 42;

/// A tiny deterministic SplitMix64 pseudorandom number generator.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Creates a new generator from a 64-bit seed.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Generates the next 64-bit unsigned integer.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Generates a random `usize` uniformly in `0..n`. Returns 0 if `n == 0`.
    pub fn gen_range(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % (n as u64)) as usize
        }
    }
}

/// Computes the 2.5th and 97.5th percentiles of resampled means using bootstrap resampling.
pub fn bootstrap_ci(values: &[f64], resamples: usize, seed: u64) -> (f64, f64) {
    if values.is_empty() || resamples == 0 {
        return (0.0, 0.0);
    }
    let n = values.len();
    let mut rng = SplitMix64::new(seed);
    let mut means = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let mut sum = 0.0;
        for _ in 0..n {
            let idx = rng.gen_range(n);
            sum += values[idx];
        }
        means.push(sum / (n as f64));
    }
    means.sort_by(|x, y| x.total_cmp(y));

    let lo_idx = ((resamples as f64 * 0.025).round() as usize).min(resamples - 1);
    let hi_idx = ((resamples as f64 * 0.975).round() as usize).min(resamples - 1);
    (means[lo_idx], means[hi_idx])
}

/// Computes the sample mean of `b - a` and its 95% confidence interval `(mean, lo, hi)`
/// by resampling paired query indices.
pub fn paired_diff_ci(a: &[f64], b: &[f64], resamples: usize, seed: u64) -> (f64, f64, f64) {
    if a.is_empty() || b.is_empty() || a.len() != b.len() || resamples == 0 {
        return (0.0, 0.0, 0.0);
    }
    let n = a.len();
    let mean = (0..n).map(|i| b[i] - a[i]).sum::<f64>() / (n as f64);
    let diffs: Vec<f64> = (0..n).map(|i| b[i] - a[i]).collect();
    let (lo, hi) = bootstrap_ci(&diffs, resamples, seed);
    (mean, lo, hi)
}

/// Stratified bootstrap resampling across groups (e.g. repositories).
/// Resamples within each stratum to preserve stratum sizes, computing the macro-average or micro-average.
pub fn stratified_bootstrap_ci(
    strata: &[Vec<f64>],
    resamples: usize,
    seed: u64,
    macro_average: bool,
) -> (f64, f64) {
    let valid_strata: Vec<&Vec<f64>> = strata.iter().filter(|s| !s.is_empty()).collect();
    if valid_strata.is_empty() || resamples == 0 {
        return (0.0, 0.0);
    }
    let mut rng = SplitMix64::new(seed);
    let mut means = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        if macro_average {
            let mut stratum_means = Vec::with_capacity(valid_strata.len());
            for s in &valid_strata {
                let n = s.len();
                let sum: f64 = (0..n).map(|_| s[rng.gen_range(n)]).sum();
                stratum_means.push(sum / (n as f64));
            }
            let macro_mean = stratum_means.iter().sum::<f64>() / (stratum_means.len() as f64);
            means.push(macro_mean);
        } else {
            let mut total_sum = 0.0;
            let mut total_n = 0;
            for s in &valid_strata {
                let n = s.len();
                total_sum += (0..n).map(|_| s[rng.gen_range(n)]).sum::<f64>();
                total_n += n;
            }
            means.push(total_sum / (total_n as f64));
        }
    }
    means.sort_by(|x, y| x.total_cmp(y));
    let lo_idx = ((resamples as f64 * 0.025).round() as usize).min(resamples - 1);
    let hi_idx = ((resamples as f64 * 0.975).round() as usize).min(resamples - 1);
    (means[lo_idx], means[hi_idx])
}

/// Computes the sample mean of paired differences `b - a` and its 95% confidence interval
/// using stratified resampling across repository groups.
pub fn stratified_paired_diff_ci(
    paired_strata: &[(Vec<f64>, Vec<f64>)],
    resamples: usize,
    seed: u64,
    macro_average: bool,
) -> (f64, f64, f64) {
    let diff_strata: Vec<Vec<f64>> = paired_strata
        .iter()
        .map(|(a, b)| a.iter().zip(b).map(|(x, y)| y - x).collect())
        .collect();
    let point_estimate = if macro_average {
        let valid: Vec<_> = diff_strata.iter().filter(|s| !s.is_empty()).collect();
        if valid.is_empty() {
            0.0
        } else {
            valid
                .iter()
                .map(|s| s.iter().sum::<f64>() / (s.len() as f64))
                .sum::<f64>()
                / (valid.len() as f64)
        }
    } else {
        let total_sum: f64 = diff_strata.iter().flat_map(|s| s.iter()).sum();
        let total_n: usize = diff_strata.iter().map(|s| s.len()).sum();
        if total_n == 0 {
            0.0
        } else {
            total_sum / total_n as f64
        }
    };
    let (lo, hi) = stratified_bootstrap_ci(&diff_strata, resamples, seed, macro_average);
    (point_estimate, lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_deterministic_for_a_seed() {
        let values = [0.1, 0.2, 0.5, 0.8, 1.0];
        let ci1 = bootstrap_ci(&values, RESAMPLES, SEED);
        let ci2 = bootstrap_ci(&values, RESAMPLES, SEED);
        assert_eq!(ci1, ci2);
    }

    #[test]
    fn ci_contains_sample_mean() {
        let values = [0.1, 0.2, 0.4, 0.7, 0.9, 1.0, 0.3, 0.6];
        let mean: f64 = values.iter().sum::<f64>() / (values.len() as f64);
        let (lo, hi) = bootstrap_ci(&values, RESAMPLES, SEED);
        assert!(lo <= mean, "lo {lo} <= mean {mean}");
        assert!(mean <= hi, "mean {mean} <= hi {hi}");
    }

    #[test]
    fn paired_diff_of_identical_vectors_is_zero() {
        let a = [0.1, 0.4, 0.8, 1.0];
        let (mean, lo, hi) = paired_diff_ci(&a, &a, RESAMPLES, SEED);
        assert_eq!((mean, lo, hi), (0.0, 0.0, 0.0));
    }

    #[test]
    fn constant_shift_gives_exact_mean_and_ci() {
        let a = [0.1, 0.3, 0.5, 0.7, 0.9];
        let b: Vec<f64> = a.iter().map(|&x| x + 0.1).collect();
        let (mean, lo, hi) = paired_diff_ci(&a, &b, RESAMPLES, SEED);
        assert!((mean - 0.1).abs() < 1e-9, "mean {mean}");
        assert!((lo - 0.1).abs() < 1e-9, "lo {lo}");
        assert!((hi - 0.1).abs() < 1e-9, "hi {hi}");
    }

    #[test]
    fn empty_input_returns_zeros() {
        assert_eq!(bootstrap_ci(&[], RESAMPLES, SEED), (0.0, 0.0));
        assert_eq!(paired_diff_ci(&[], &[], RESAMPLES, SEED), (0.0, 0.0, 0.0));
    }

    #[test]
    fn stratified_bootstrap_recovers_macro_and_micro_means() {
        // Stratum 1: 2 items with mean 1.0; Stratum 2: 8 items with mean 0.0
        let s1 = vec![1.0, 1.0];
        let s2 = vec![0.0; 8];
        let (lo_macro, hi_macro) = stratified_bootstrap_ci(&[s1.clone(), s2.clone()], RESAMPLES, SEED, true);
        // Macro mean = (1.0 + 0.0) / 2 = 0.5
        assert!((lo_macro - 0.5).abs() < 1e-6);
        assert!((hi_macro - 0.5).abs() < 1e-6);

        let (lo_micro, hi_micro) = stratified_bootstrap_ci(&[s1, s2], RESAMPLES, SEED, false);
        // Micro mean = 2.0 / 10 = 0.2
        assert!((lo_micro - 0.2).abs() < 1e-6);
        assert!((hi_micro - 0.2).abs() < 1e-6);
    }
}
