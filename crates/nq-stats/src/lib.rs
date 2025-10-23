// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

mod counter;

use std::cmp::Ordering;
use std::time::Duration;

use nq_core::Timestamp;

pub use crate::counter::CounterSeries;

pub fn instant_minus_intervals(
    to: Timestamp,
    intervals: usize,
    interval_length: Duration,
) -> Timestamp {
    to - (interval_length * intervals as u32)
}

#[derive(Debug, Default)]
pub struct TimeSeries {
    // windows: Option<usize>,
    samples: Vec<(Timestamp, f64)>,
}

impl TimeSeries {
    pub fn new() -> Self {
        Self {
            samples: Vec::new(),
        }
    }

    pub fn add(&mut self, time: Timestamp, sample: f64) {
        match self.samples.binary_search_by_key(&time, |(time, _)| *time) {
            Ok(index) => self.samples.insert(index, (time, sample)),
            Err(index) => {
                if index == self.samples.len() {
                    self.samples.push((time, sample))
                } else {
                    self.samples.insert(index, (time, sample));
                }
            }
        }
    }

    pub fn sample_interval(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> impl Iterator<Item = f64> + Clone + '_ {
        self.samples
            .iter()
            .rev()
            .skip_while(move |(ts, _)| *ts > to)
            .take_while(move |(ts, _)| *ts >= from)
            .map(|(_, s)| s)
            .copied()
    }

    pub fn average(&self) -> Option<f64> {
        average(self.values())
    }

    pub fn std(&self) -> Option<f64> {
        std(self.values())
    }

    pub fn trimmed_mean(&self, percentile: f64) -> Option<f64> {
        trimmed_mean(self.values(), percentile)
    }

    pub fn sum(&self) -> f64 {
        self.values().sum::<f64>()
    }

    pub fn interval_sum(&self, from: Timestamp, to: Timestamp) -> f64 {
        self.sample_interval(from, to).sum()
    }

    pub fn interval_average(&self, from: Timestamp, to: Timestamp) -> Option<f64> {
        average(self.sample_interval(from, to))
    }

    pub fn interval_std(&self, from: Timestamp, to: Timestamp) -> Option<f64> {
        std(self.sample_interval(from, to))
    }

    pub fn interval_trimmed_mean(
        &self,
        from: Timestamp,
        to: Timestamp,
        percentile: f64,
    ) -> Option<f64> {
        trimmed_mean(self.sample_interval(from, to), percentile)
    }

    pub fn values(&self) -> impl Iterator<Item = f64> + Clone + '_ {
        self.samples.iter().map(|(_, sample)| sample).copied()
    }

    pub fn samples(&self) -> &[(Timestamp, f64)] {
        &self.samples
    }

    pub fn quantile(&self, quantile: f64) -> Option<f64> {
        let mut values: Vec<f64> = self.values().collect();
        values.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Less));

        values
            .get((values.len() as f64 * quantile) as usize)
            .copied()
    }
}

pub fn average(samples: impl Iterator<Item = f64> + Clone) -> Option<f64> {
    let n = samples.clone().count();

    if n == 0 {
        None
    } else {
        Some(samples.sum::<f64>() / n as f64)
    }
}

pub fn std(samples: impl Iterator<Item = f64> + Clone) -> Option<f64> {
    let avg = average(samples.clone())?;
    let n = samples.clone().count();

    if n == 0 {
        None
    } else {
        let sum_of_squares = samples.map(|s| (avg - s).powi(2)).sum::<f64>();
        Some((sum_of_squares / n as f64).sqrt())
    }
}

/// Calculate the trimmed mean of a series. We keep all values
/// less than the given percentile and the calculate the average.
fn trimmed_mean(samples: impl Iterator<Item = f64> + Clone, percentile: f64) -> Option<f64> {
    let mut samples: Vec<_> = samples.collect();
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let to_keep = (samples.len() as f64 * percentile) as usize;
    samples.truncate(to_keep);

    average(samples.into_iter())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nq_core::Timestamp;

    use crate::{TimeSeries, instant_minus_intervals};

    fn avg_first_n(n: f64) -> f64 {
        (n + 1.0) / 2.0
    }

    fn std_first_n(n: f64) -> f64 {
        ((n.powi(2) - 1.0) / 12.0).sqrt()
    }

    #[test]
    fn average_simple() {
        let mut ts = TimeSeries::new();
        assert_eq!(ts.average(), None);

        for n in 1..10 {
            ts.add(Timestamp::now(), n as f64);
            assert_eq!(ts.average(), Some(avg_first_n(n as f64)));
        }
    }

    #[test]
    fn average_intervaled() {
        let mut ts = TimeSeries::new();
        let start = Timestamp::now();

        let intervals = 4;
        let interval_length = Duration::from_secs(1);

        for n in 1..=10 {
            ts.add(start + interval_length * n as u32, n as f64);
        }

        let total_avg = ts.average().unwrap();
        assert_eq!(total_avg, avg_first_n(10.0));

        let to = start + interval_length * 10 + Duration::from_millis(1);
        let from = instant_minus_intervals(to, intervals, interval_length);
        let interval_avg = ts.interval_average(from, to).unwrap();
        assert_eq!(interval_avg, 8.5); // (10 + 9 + 8 + 7) / 4
    }

    #[test]
    fn std_simple() {
        let mut ts = TimeSeries::new();

        for n in 1..10 {
            ts.add(Timestamp::now(), n as f64);
            assert_eq!(ts.std(), Some(std_first_n(n as f64)));
        }
    }

    #[test]
    fn std_intervaled() {
        let mut ts = TimeSeries::new();
        let start = Timestamp::now();

        let intervals = 4;
        let interval_length = Duration::from_secs(1);

        for n in 1..=10 {
            ts.add(start + interval_length * n as u32, n as f64);
        }

        let total_avg = ts.std().unwrap();
        assert_eq!(total_avg, std_first_n(10.0));

        let to = start + interval_length * 10 + Duration::from_millis(1);
        let from = instant_minus_intervals(to, intervals, interval_length);
        let interval_std = ts.interval_std(from, to).unwrap();

        // avg = (10 + 9 + 8 + 7) / 4 = 8.5
        // dist = 1.5, 0.5, -0.5, 1.5
        // sq = 2.25 + 0.25 + 0.25 + 2.25 = 5.0
        // div = 5.0 / 4.0 = 1.25
        // sqrt = 1.118033988749895
        assert_eq!(interval_std, 1.118033988749895);
    }

    #[test]
    fn trimmed_mean() {
        let mut ts = TimeSeries::new();
        let start = Timestamp::now();

        let intervals = 10;
        let interval_length = Duration::from_secs(1);

        // keep the 90th percentile of values;
        let percentile = 0.90;

        for n in 1..=20 {
            ts.add(start + interval_length * n as u32, n as f64);
        }

        // The 90% of 1..=20 == 18. So we should see the average
        // of the first 18 natural numbers.
        let trimmed_mean = ts.trimmed_mean(percentile).unwrap();
        assert_eq!(trimmed_mean, avg_first_n(18.0));

        // The 90% of the last 10 intervals, [11, 20] is 19, so the trimmed
        // mean should drop the last value, 20. Average of [11, 19] is 14.5
        let to = start + interval_length * 20 + Duration::from_millis(1);
        let from = instant_minus_intervals(to, intervals, interval_length);

        let trimmed_mean = ts.interval_trimmed_mean(from, to, percentile).unwrap();
        assert_eq!(trimmed_mean, 15.0);
    }
}
