use nq_core::Timestamp;
use tracing::debug;

#[derive(Debug, Default)]
pub struct CounterSeries {
    // windows: Option<usize>,
    timestamps: Vec<Timestamp>,
    samples: Vec<f64>,
}

pub struct SampleRange {
    start: (Timestamp, f64),
    end: (Timestamp, f64),
}

impl CounterSeries {
    pub fn new() -> Self {
        Self {
            timestamps: Vec::new(),
            samples: Vec::new(),
        }
    }

    pub fn add(&mut self, timestamp: Timestamp, sample: f64) {
        let idx = self.timestamps.partition_point(|p| p < &timestamp);

        if idx == self.samples.len() {
            self.timestamps.push(timestamp);
            self.samples.push(sample);
        } else {
            self.timestamps.insert(idx, timestamp);
            self.samples.insert(idx, sample);
        }
    }

    pub fn sample_interval(&self, from: Timestamp, to: Timestamp) -> Option<SampleRange> {
        let start_idx = self.timestamps.partition_point(|t| t < &from);
        let end_idx = self.timestamps.partition_point(|t| t < &to);

        // end is either somewhere in the range or the last idx:
        let end = self.timestamps.len().saturating_sub(1).min(end_idx);

        let start = self
            .timestamps
            .get(start_idx)
            .copied()
            .zip(self.samples.get(start_idx).copied())?;
        let end = self
            .timestamps
            .get(end)
            .copied()
            .zip(self.samples.get(end).copied())?;

        debug!("sample interval: from={from:?}, to={to:?}, start_idx={start_idx}, end_idx={end_idx}, start={start:?}, end={end:?}");

        Some(SampleRange { start, end })
    }

    pub fn average(&self) -> Option<f64> {
        self.interval_average(*self.timestamps.first()?, *self.timestamps.last()?)
    }

    pub fn sum(&self) -> f64 {
        self.samples.last().copied().unwrap_or_default()
    }

    pub fn interval_sum(&self, from: Timestamp, to: Timestamp) -> f64 {
        let Some(SampleRange {
            start: (_start_ts, start_sample),
            end: (_end_ts, end_sample),
        }) = self.sample_interval(from, to)
        else {
            return 0.0;
        };

        end_sample - start_sample
    }

    pub fn interval_average(&self, from: Timestamp, to: Timestamp) -> Option<f64> {
        let SampleRange {
            start: (start_ts, start_sample),
            end: (end_ts, end_sample),
        } = self.sample_interval(from, to)?;

        if start_ts == end_ts {
            Some(end_sample)
        } else {
            Some((end_sample - start_sample) / end_ts.duration_since(start_ts).as_secs_f64())
        }
    }

    pub fn samples(&self) -> impl Iterator<Item = f64> + Clone + '_ {
        self.samples.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nq_core::Timestamp;

    use crate::{counter::CounterSeries, instant_minus_intervals};

    fn avg_first_n(n: f64) -> f64 {
        (n + 1.0) / 2.0
    }

    #[test]
    fn average_simple() {
        let mut ts = CounterSeries::new();
        assert_eq!(ts.average(), None);

        let now = Timestamp::now();

        for n in 0..10 {
            ts.add(now + n * Duration::from_secs(1), n as f64);
        }

        assert_eq!(ts.average(), Some(1.0))
    }

    #[test]
    fn average_intervaled() {
        let mut ts = CounterSeries::new();
        let start = Timestamp::now();

        let intervals = 4;
        let interval_length = Duration::from_secs(1);

        for n in 0..=10 {
            ts.add(start + interval_length * n as u32, n as f64);
        }

        let total_avg = ts.average().unwrap();
        assert_eq!(total_avg, avg_first_n(10.0));

        let to = start + interval_length * 10 + Duration::from_millis(1);
        let from = instant_minus_intervals(to, intervals, interval_length);
        let interval_avg = ts.interval_average(from, to).unwrap();
        assert_eq!(interval_avg, 8.5); // (10 + 9 + 8 + 7) / 4
    }
}
