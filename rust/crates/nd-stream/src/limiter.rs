//! Immediate-rejection transcode concurrency limiter.
//!
//! Wired into process creation by [`crate::transcode_body::open_transcode_gated`]:
//! every gated launch acquires here before spawning, and the permit is held by
//! the process owner until a teardown **confirms** the child was reaped — see the
//! permit-lifetime note in `transcode_body.rs` for why that is stricter than Go.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The `Retry-After` value used for rejected transcodes (core/stream/limiter.go:16-19).
pub const RETRY_AFTER_SECONDS: u64 = 5;

/// Rejection from a saturated transcode limiter (core/stream/limiter.go:11-14).
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("too many concurrent transcodes")]
pub struct TooManyTranscodes;

#[derive(Default)]
struct State {
    global: usize,
    users: HashMap<String, usize>,
}

struct Inner {
    global_limit: usize,
    user_limit: usize,
    state: Mutex<State>,
}

/// A shareable limiter. Non-positive limits disable the corresponding cap.
#[derive(Clone)]
pub struct TranscodeLimiter(Arc<Inner>);

/// An acquired slot, returned automatically when dropped.
pub struct TranscodePermit {
    inner: Arc<Inner>,
    user: Option<String>,
}

impl TranscodeLimiter {
    /// Construct independent global and per-user caps (core/stream/limiter.go:37-53).
    #[must_use]
    pub fn new(global_limit: i32, user_limit: i32) -> Self {
        Self(Arc::new(Inner {
            global_limit: global_limit.max(0) as usize,
            user_limit: user_limit.max(0) as usize,
            state: Mutex::new(State::default()),
        }))
    }

    /// Whether either limit is enabled.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.0.global_limit > 0 || self.0.user_limit > 0
    }

    /// Try to acquire immediately; anonymous users bypass the per-user cap.
    /// Mirrors core/stream/limiter.go:86-129 (there is no queue or timeout).
    pub fn acquire(&self, user: &str) -> Result<TranscodePermit, TooManyTranscodes> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let user_active = self.0.user_limit > 0 && !user.is_empty();
        if user_active && state.users.get(user).copied().unwrap_or(0) >= self.0.user_limit {
            return Err(TooManyTranscodes);
        }
        if self.0.global_limit > 0 && state.global >= self.0.global_limit {
            return Err(TooManyTranscodes);
        }
        if user_active {
            *state.users.entry(user.to_owned()).or_default() += 1;
        }
        state.global += 1;
        Ok(TranscodePermit {
            inner: Arc::clone(&self.0),
            user: user_active.then(|| user.to_owned()),
        })
    }
}

impl Drop for TranscodePermit {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.global -= 1;
        if let Some(user) = &self.user {
            let count = state.users.get_mut(user).expect("permit owns a user slot");
            *count -= 1;
            if *count == 0 {
                state.users.remove(user);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::{Barrier, Mutex};
    use std::thread;

    #[test]
    fn caps_and_anonymous_users() {
        let limiter = TranscodeLimiter::new(3, 1);
        let alice = limiter.acquire("alice").unwrap();
        assert!(limiter.acquire("alice").is_err());
        let anonymous = limiter.acquire("").unwrap();
        let anonymous2 = limiter.acquire("").unwrap();
        assert!(limiter.acquire("bob").is_err());
        drop(alice);
        assert!(limiter.acquire("bob").is_ok());
        drop((anonymous, anonymous2));
        assert!(!TranscodeLimiter::new(-1, 0).enabled());
    }

    // Real concurrent attempts retain the permit while a stub process is alive.
    // Raising the cap provides the non-vacuous control requested by issue #15.
    fn contend(cap: i32) -> (usize, usize) {
        const ATTEMPTS: usize = 4;
        let limiter = TranscodeLimiter::new(cap, 0);
        let start = Arc::new(Barrier::new(ATTEMPTS));
        let held = Arc::new(Barrier::new(ATTEMPTS));
        let results = Arc::new(Mutex::new((0, 0)));
        let mut threads = Vec::new();
        for _ in 0..ATTEMPTS {
            let limiter = limiter.clone();
            let start = Arc::clone(&start);
            let held = Arc::clone(&held);
            let results = Arc::clone(&results);
            threads.push(thread::spawn(move || {
                start.wait();
                let permit = limiter.acquire("alice");
                let mut child = permit.as_ref().ok().map(|_| {
                    Command::new("sleep")
                        .arg("30")
                        .stdout(Stdio::null())
                        .spawn()
                        .unwrap()
                });
                {
                    let mut counts = results.lock().unwrap();
                    if permit.is_ok() {
                        counts.0 += 1;
                    } else {
                        counts.1 += 1;
                    }
                }
                held.wait();
                if let Some(child) = &mut child {
                    assert!(child.try_wait().unwrap().is_none());
                    child.kill().unwrap();
                    child.wait().unwrap();
                }
                drop(permit);
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let result = *results.lock().unwrap();
        result
    }

    #[test]
    fn contention_with_live_processes_and_relaxed_control() {
        assert_eq!(contend(3), (3, 1));
        assert_eq!(contend(4), (4, 0));
    }
}
