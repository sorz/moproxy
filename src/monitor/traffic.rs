use std::{
    time::Instant,
    ops::Add,
    collections::VecDeque,
};
use serde_derive::Serialize;
use crate::proxy::Traffic;

/// Monitor & caculate throughtput using traffic samples.
#[derive(Debug)]
pub struct Meter {
    samples: VecDeque<TrafficSample>,
}

#[derive(Debug)]
pub struct TrafficSample {
    time: Instant,
    amt: Traffic,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Throughput {
    pub tx_bps: usize,
    pub rx_bps: usize,
}

impl Into<TrafficSample> for Traffic {
    fn into(self) -> TrafficSample {
        TrafficSample {
            time: Instant::now(),
            amt: self,
        }
    }
}

impl Throughput {
    fn from_samples(t0: &TrafficSample, t1: &TrafficSample) -> Self {
        let t = t1.time - t0.time;
        let t = t.as_secs() as f64 + t.subsec_nanos() as f64 / 1e9;
        let f = |x0, x1| (((x1 - x0) as f64) / t * 8.0).round() as usize;
        Throughput {
            tx_bps: f(t0.amt.tx_bytes, t1.amt.tx_bytes),
            rx_bps: f(t0.amt.rx_bytes, t1.amt.rx_bytes),
        }
    }
}

impl Add for Throughput {
    type Output = Throughput;

    fn add(self, other: Self) -> Self {
        Throughput {
            tx_bps: self.tx_bps + other.tx_bps,
            rx_bps: self.rx_bps + other.rx_bps,
        }
    }
}

impl Meter {
    pub fn new() -> Self {
        Meter {
            samples: VecDeque::with_capacity(2),
        }
    }

    pub fn add_sample<T>(&mut self, sample: T)
    where T: Into<TrafficSample> {
        self.samples.truncate(1);
        self.samples.push_front(sample.into());
    }

    pub fn throughput<T>(&self, sample: T) -> Throughput
    where T: Into<TrafficSample> {
        let current = sample.into();
        if let Some(oldest) = self.samples.back() {
            Throughput::from_samples(oldest, &current)
        } else {
            Default::default()
        }
    }
}

