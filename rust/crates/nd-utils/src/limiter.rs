//! Rate limiting per ID — port of `utils/limiter.go` (`Limiter`, backed by
//! Go's `rate.Sometimes`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A rate limiter that allows a function to be executed at most once per ID
/// and per interval (`utils.Limiter`).
pub struct Limiter {
    /// The per-id interval; zero means the 1-minute default.
    pub interval: Duration,
    inner: Mutex<HashMap<String, Arc<Mutex<Sometimes>>>>,
}

struct Sometimes {
    last: Option<Instant>,
}

impl Sometimes {
    fn do_fn(&mut self, interval: Duration, f: &dyn Fn()) {
        let now = Instant::now();
        if let Some(last) = self.last {
            if now.duration_since(last) < interval {
                return;
            }
        }
        self.last = Some(now);
        f();
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Limiter {
    /// Creates a limiter with the default (zero) interval.
    #[must_use]
    pub fn new() -> Self {
        Self {
            interval: Duration::ZERO,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Executes `f` if the rate limiter for the given `id` allows it
    /// (`Limiter.Do`). Uses [`Self::interval`] or defaults to 1 minute.
    pub fn do_fn(&self, id: &str, f: &dyn Fn()) {
        let interval = if self.interval.is_zero() {
            Duration::from_secs(60) // Default every 1 minute
        } else {
            self.interval
        };
        let limiter = {
            let mut map = self.inner.lock().expect("limiter lock");
            Arc::clone(
                map.entry(id.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(Sometimes { last: None }))),
            )
        };
        limiter.lock().expect("sometimes lock").do_fn(interval, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_the_function_once_per_id_per_interval() {
        let limiter = Limiter::new();
        let count = std::sync::atomic::AtomicUsize::new(0);
        limiter.do_fn("id", &|| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        limiter.do_fn("id", &|| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        // Second call within the default 1-minute interval is suppressed.
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn tracks_ids_independently() {
        let limiter = Limiter::new();
        let count = std::sync::atomic::AtomicUsize::new(0);
        limiter.do_fn("a", &|| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        limiter.do_fn("b", &|| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn honors_the_configured_interval() {
        let limiter = Limiter {
            interval: Duration::from_millis(1),
            ..Limiter::new()
        };
        let count = std::sync::atomic::AtomicUsize::new(0);
        limiter.do_fn("id", &|| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(3));
        limiter.do_fn("id", &|| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
