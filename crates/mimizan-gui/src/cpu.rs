//! The machine's cores, for the negative panel.
//!
//! One reading of the whole computer, four times a second, and only while
//! the section is open. The first reading is a baseline; the next one, a
//! quarter-second later, is the load. A gap of a second or more (the section
//! was closed) starts that pair again, so a long pause is not shown as one
//! average.

use std::time::Instant;

use sysinfo::System;

/// How often an open section takes a new reading.
pub const SAMPLE_EVERY: std::time::Duration = std::time::Duration::from_millis(250);
/// A longer gap is a closed section, not a slow frame. The next reading
/// only stores the baseline.
const BASELINE_GAP: std::time::Duration = std::time::Duration::from_millis(1000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SampleStep {
    /// The last reading is still current.
    Keep,
    /// Store counters. There is nothing to show yet.
    Baseline,
    /// Turn the counters since the baseline into a load per core.
    Read,
}

fn sample_step(elapsed: std::time::Duration, primed: bool) -> SampleStep {
    if primed && elapsed < SAMPLE_EVERY {
        SampleStep::Keep
    } else if !primed || elapsed >= BASELINE_GAP {
        SampleStep::Baseline
    } else {
        SampleStep::Read
    }
}

/// What the section draws. `Waiting` is the baseline quarter-second.
#[derive(Clone, Debug, PartialEq)]
pub enum CpuView {
    Waiting,
    /// One load per core, 0–100, in the machine's order.
    Cores(Vec<f32>),
}

/// Per-core load sampled at a fixed interval; `sample()` is what the toolbar draws.
pub struct CpuMeter {
    system: System,
    last: Instant,
    primed: bool,
    /// A `Read` has happened since the last baseline. Empty loads then mean
    /// the machine reported no cores.
    have_read: bool,
    loads: Vec<f32>,
}

impl CpuMeter {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            last: Instant::now(),
            primed: false,
            have_read: false,
            loads: Vec::new(),
        }
    }

    /// The current view. Call this only while the section is open.
    pub fn sample(&mut self) -> CpuView {
        match sample_step(self.last.elapsed(), self.primed) {
            SampleStep::Keep => {}
            SampleStep::Baseline => {
                self.system.refresh_cpu_usage();
                self.last = Instant::now();
                self.primed = true;
                self.have_read = false;
                self.loads.clear();
            }
            SampleStep::Read => {
                self.system.refresh_cpu_usage();
                self.last = Instant::now();
                self.primed = true;
                self.have_read = true;
                self.loads.clear();
                self.loads.extend(self.system.cpus().iter().map(|cpu| cpu.cpu_usage().clamp(0.0, 100.0)));
            }
        }
        if self.have_read {
            CpuView::Cores(self.loads.clone())
        } else {
            CpuView::Waiting
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{sample_step, SampleStep, BASELINE_GAP, SAMPLE_EVERY};
    use std::time::Duration;

    #[test]
    fn the_first_reading_is_a_baseline_and_the_next_is_the_load() {
        assert_eq!(sample_step(Duration::ZERO, false), SampleStep::Baseline);
        assert_eq!(sample_step(SAMPLE_EVERY, true), SampleStep::Read);
        assert_eq!(sample_step(Duration::from_millis(100), true), SampleStep::Keep);
    }

    #[test]
    fn a_closed_section_does_not_come_back_as_one_long_average() {
        assert_eq!(sample_step(BASELINE_GAP, true), SampleStep::Baseline);
        assert_eq!(sample_step(Duration::from_secs(30), true), SampleStep::Baseline);
    }
}
