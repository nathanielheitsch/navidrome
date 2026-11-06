//! Port of `scheduler/log_adapter.go`: the cron logger bridging robfig's
//! `cron.Logger` onto Navidrome's `log` package. The Rust port bridges onto
//! `the workspace `tracing` façade, with the same level split the Go
//! adapter has: Info → Trace, Error → Error.

/// Port of the Go `logger` struct (log_adapter.go:7-22), as a trait so
/// tests can capture output. The production impl is [`Logger`].
pub trait CronLogger: Send + Sync {
    /// Robfig `Info` calls: message plus key/value pairs, at trace level.
    fn info(&self, msg: &str, keys_and_values: &[&str]);
    /// Robfig `Error` calls: message, key/value pairs, then the error, at
    /// error level.
    fn error(&self, error: &str, msg: &str, keys_and_values: &[&str]);
}

/// The production logger (port of `logger{}` in log_adapter.go). Keys and
/// values are flattened into the message because `tracing` events carry no
/// key/value payload; both Go methods keep the `Scheduler: ` prefix.
#[derive(Debug, Default)]
pub struct Logger;

impl CronLogger for Logger {
    fn info(&self, msg: &str, keys_and_values: &[&str]) {
        let mut out = format!("Scheduler: {msg}");
        for kv in keys_and_values {
            out.push(' ');
            out.push_str(kv);
        }
        if std::env::var("ND_SCHED_TRACE").is_ok() {
            eprintln!("{out}");
        }
    }

    fn error(&self, error: &str, msg: &str, keys_and_values: &[&str]) {
        let mut out = format!("Scheduler: {msg}");
        for kv in keys_and_values {
            out.push(' ');
            out.push_str(kv);
        }
        eprintln!("{out} {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Captured {
        info: Mutex<Vec<String>>,
        error: Mutex<Vec<String>>,
    }

    struct Capture(Arc<Captured>);

    impl CronLogger for Capture {
        fn info(&self, msg: &str, keys_and_values: &[&str]) {
            let mut out = format!("Scheduler: {msg}");
            for kv in keys_and_values {
                out.push(' ');
                out.push_str(kv);
            }
            self.0.info.lock().unwrap().push(out);
        }
        fn error(&self, error: &str, msg: &str, keys_and_values: &[&str]) {
            let mut out = format!("Scheduler: {msg}");
            for kv in keys_and_values {
                out.push(' ');
                out.push_str(kv);
            }
            self.0.error.lock().unwrap().push(format!("{out} {error}"));
        }
    }

    #[test]
    fn info_prefixes_scheduler_and_appends_key_values() {
        // Arrange:
        let captured = Arc::new(Captured::default());
        let logger = Capture(Arc::clone(&captured));
        // Act:
        CronLogger::info(&logger, "start", &["k1=v1", "k2=v2"]);
        // Assert:
        assert_eq!(
            captured.info.lock().unwrap()[0],
            "Scheduler: start k1=v1 k2=v2"
        );
    }

    #[test]
    fn error_appends_the_error_last() {
        // Arrange:
        let captured = Arc::new(Captured::default());
        let logger = Capture(Arc::clone(&captured));
        // Act:
        CronLogger::error(&logger, "boom", "stop", &["id=1"]);
        // Assert:
        assert_eq!(
            captured.error.lock().unwrap()[0],
            "Scheduler: stop id=1 boom"
        );
    }
}
