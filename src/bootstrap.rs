const SEED: u64 = 0x5846_4552_424f_4f54;

pub(crate) fn median_confidence_interval(
    samples: &[f64],
    confidence_level: f64,
    resamples: usize,
) -> Option<(f64, f64)> {
    if samples.is_empty()
        || samples.iter().any(|sample| !sample.is_finite())
        || !(0.0..1.0).contains(&confidence_level)
        || resamples == 0
    {
        return None;
    }

    if samples.len() == 1 || samples.iter().all(|sample| *sample == samples[0]) {
        return Some((samples[0], samples[0]));
    }

    let mut rng = SplitMix64::new(seed_for(samples));
    let mut resample = vec![0.0; samples.len()];
    let mut medians = Vec::with_capacity(resamples);

    for _ in 0..resamples {
        for value in &mut resample {
            *value = samples[rng.index(samples.len())];
        }
        resample.sort_by(f64::total_cmp);
        medians.push(median_sorted(&resample));
    }

    medians.sort_by(f64::total_cmp);
    let tail = (1.0 - confidence_level) * 50.0;
    Some((
        percentile_sorted(&medians, tail),
        percentile_sorted(&medians, 100.0 - tail),
    ))
}

fn seed_for(samples: &[f64]) -> u64 {
    samples.iter().fold(SEED, |seed, sample| {
        seed.rotate_left(17) ^ sample.to_bits().wrapping_mul(0x9e37_79b9_7f4a_7c15)
    })
}

fn median_sorted(sorted: &[f64]) -> f64 {
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        f64::midpoint(sorted[middle - 1], sorted[middle])
    } else {
        sorted[middle]
    }
}

fn percentile_sorted(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }

    let rank = (percentile / 100.0) * (sorted.len() - 1) as f64;
    let lower_index = rank.floor() as usize;
    let upper_index = rank.ceil() as usize;
    if lower_index == upper_index {
        sorted[lower_index]
    } else {
        let weight = rank - lower_index as f64;
        sorted[lower_index] + (sorted[upper_index] - sorted[lower_index]) * weight
    }
}

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn index(&mut self, len: usize) -> usize {
        ((u128::from(self.next()) * len as u128) >> 64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_inputs() {
        assert_eq!(median_confidence_interval(&[], 0.95, 100), None);
        assert_eq!(
            median_confidence_interval(&[1.0, f64::NAN], 0.95, 100),
            None
        );
        assert_eq!(median_confidence_interval(&[1.0], 1.0, 100), None);
        assert_eq!(median_confidence_interval(&[1.0], 0.95, 0), None);
    }

    #[test]
    fn identical_samples_have_an_exact_interval() {
        assert_eq!(
            median_confidence_interval(&[7.0, 7.0, 7.0], 0.95, 1_000),
            Some((7.0, 7.0))
        );
    }

    #[test]
    fn bootstrap_is_deterministic_and_contains_the_sample_median() {
        let samples = [10.0, 20.0, 30.0, 40.0, 50.0];
        let first = median_confidence_interval(&samples, 0.95, 10_000).unwrap();
        let second = median_confidence_interval(&samples, 0.95, 10_000).unwrap();

        assert_eq!(first, second);
        assert!(first.0 <= 30.0);
        assert!(first.1 >= 30.0);
        assert!(first.0 < first.1);
    }
}
