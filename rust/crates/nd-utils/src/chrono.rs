//! A simple stopwatch — port of `utils/chrono` (`meter.go`).

use std::time::{Duration, Instant};

/// A simple stopwatch.
#[derive(Debug, Default)]
pub struct Meter {
    elapsed: Duration,
    mark: Option<Instant>,
}

impl Meter {
    /// Starts the meter.
    pub fn start(&mut self) {
        self.mark = Some(Instant::now());
    }

    /// Stops the meter and returns the accumulated elapsed time. Stopping
    /// without a start returns the previously accumulated time.
    pub fn stop(&mut self) -> Duration {
        let Some(mark) = self.mark else {
            return self.elapsed;
        };
        self.elapsed += mark.elapsed();
        self.mark = None;
        self.elapsed
    }

    /// Returns the total elapsed time, including a running interval.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        let mut elapsed = self.elapsed;
        if let Some(mark) = self.mark {
            elapsed += mark.elapsed();
        }
        elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_returns_the_elapsed_time() {
        let mut meter = Meter::default();
        meter.start();
        std::thread::sleep(Duration::from_millis(50));
        let elapsed = meter.stop();
        assert!(elapsed >= Duration::from_millis(30), "elapsed {elapsed:?}");
        assert!(elapsed <= Duration::from_millis(200), "elapsed {elapsed:?}");
    }

    #[test]
    fn stop_accumulates_across_cycles() {
        let mut meter = Meter::default();
        meter.start();
        std::thread::sleep(Duration::from_millis(50));
        let first = meter.stop();
        meter.start();
        std::thread::sleep(Duration::from_millis(50));
        let total = meter.stop();
        assert!(total > first, "total {total:?} first {first:?}");
        assert!(total >= first * 3 / 2);
        assert!(total <= first * 3);
        assert!(total >= Duration::from_millis(60));
    }

    #[test]
    fn elapsed_excludes_stopped_intervals() {
        let mut meter = Meter::default();
        meter.start();
        std::thread::sleep(Duration::from_millis(50));
        meter.stop();

        // Should not count the time the meter was stopped
        std::thread::sleep(Duration::from_millis(50));

        meter.start();
        std::thread::sleep(Duration::from_millis(50));
        meter.stop();

        let elapsed = meter.elapsed();
        assert!(elapsed >= Duration::from_millis(60), "elapsed {elapsed:?}");
        assert!(elapsed <= Duration::from_millis(300), "elapsed {elapsed:?}");
    }

    #[test]
    fn elapsed_includes_a_running_interval() {
        let mut meter = Meter::default();
        meter.start();
        std::thread::sleep(Duration::from_millis(50));
        let elapsed = meter.elapsed();
        assert!(elapsed >= Duration::from_millis(30), "elapsed {elapsed:?}");
        assert!(elapsed <= Duration::from_millis(200), "elapsed {elapsed:?}");
    }

    #[test]
    fn stop_without_start_returns_accumulated() {
        let mut meter = Meter::default();
        meter.start();
        std::thread::sleep(Duration::from_millis(10));
        let first = meter.stop();
        assert_eq!(meter.stop(), first);
        assert_eq!(meter.elapsed(), first);
    }
}
