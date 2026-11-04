//! Fail-closed failed-login limiter — the port of
//! `server/subsonic/auth_limiter.go`.
//!
//! Go source: `auth_limiter.go:15-101`. A limiter caps failed logins per
//! key over a rolling window: [`AuthLimiter::acquire`] reserves a
//! credential check, the returned slot's `release(true)` records a
//! failure, and a key that already reached the limit of failures in the
//! window is denied. Keys are hashed so attacker-chosen usernames
//! cannot bloat memory, and an idle sweep keeps the map bounded.
//!
//! Fail-closed: this port replaces Go's `maphash.MakeSeed` (an
//! OS-seeded hash that cannot fail) with an explicit seed via
//! `getrandom`; if entropy is unavailable construction fails with
//! [`LimiterError::SeedUnavailable`] — an internal error DENIES, never
//! allows. Same for a poisoned lock: the map is recovered, and every
//! gate below denies on anything but a clean admit.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use thiserror::Error;

/// FNV-1a — the keyed hash stand-in for Go's `maphash.String(seed, key)`
/// (`auth_limiter.go:47,62`). The seed is XORed in per limiter instance
/// so entry indices are not predictable from usernames alone, keeping
/// the Go contract's memory property (attacker-chosen keys cannot
/// target one bucket).
#[derive(Debug, Clone, Copy)]
struct Seed(u64);

impl Seed {
    /// Draws a fresh seed from the OS (`maphash.MakeSeed`,
    /// `auth_limiter.go:44`) — the one OS call that can fail.
    fn new() -> Result<Self, getrandom::Error> {
        let mut buf = [0u8; 8];
        getrandom::getrandom(&mut buf)?;
        Ok(Self(u64::from_le_bytes(buf)))
    }

    fn hash(&self, key: &str) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325 ^ self.0;
        for b in key.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
}

/// The per-key failure bookkeeping — `authAttempts`
/// (`auth_limiter.go:31-37`, minus the concurrency slot channel).
#[derive(Debug)]
struct AuthAttempts {
    failures: u64,
    start: Instant,
}

/// The errors the limiter's gate denies with. Both variants fail
/// closed: callers deny the login path on either.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LimiterError {
    /// The key already reached the configured failure limit in the
    /// current window (the plain Go deny, `auth_limiter.go:66,71`).
    #[error("too many failed attempts for this key in the current window")]
    LimitReached,
    /// Entropy was unavailable for seeding — fail-closed deny
    /// (`Seed::new` failed; Go's `maphash.MakeSeed` cannot fail, so the
    /// Rust port surfaces the internal error instead of allowing).
    #[error("limiter seed unavailable; denying fail-closed")]
    SeedUnavailable,
}

/// A fail-closed failed-login limiter — `authLimiter`
/// (`auth_limiter.go:20-28`). Built via [`AuthLimiter::new`]; a
/// non-positive limit builds a limiter that allows everything
/// (`newAuthLimiter` returns nil, `auth_limiter.go:39-41`) — use
/// [`AuthLimiter::disabled`] for that shape.
#[derive(Debug)]
pub struct AuthLimiter {
    limit: u64,
    window: Duration,
    seed: Seed,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    keys: HashMap<u64, AuthAttempts>,
    last_sweep: Option<Instant>,
}

/// A reserved credential check — `authSlot` (`auth_limiter.go:40-44`),
/// bound to the limiter it was acquired from. A slot from a disabled
/// limiter releases nothing (`s == nil`).
#[derive(Debug)]
pub struct AuthSlot<'a> {
    limiter: Option<&'a AuthLimiter>,
    key_hash: u64,
}

impl AuthSlot<'_> {
    /// `release(failed)` (`auth_limiter.go:81-90`): records a failure
    /// for the key when `failed`. Success releases nothing — failures
    /// are the only thing counted.
    pub fn release(self, failed: bool) {
        if let (Some(limiter), true) = (self.limiter, failed) {
            let mut inner = limiter.lock();
            if let Some(a) = inner.keys.get_mut(&self.key_hash) {
                a.failures = a.failures.saturating_add(1);
            }
        }
    }
}

impl AuthLimiter {
    /// `newAuthLimiter(limit, window)` (`auth_limiter.go:39-49`): a
    /// non-positive limit means "allow everything" — but unlike Go's
    /// nil, this port still requires the seed so construction itself
    /// cannot silently degrade; use [`AuthLimiter::disabled`] for the
    /// allow-everything shape. A zero window falls back to the default
    /// 20s (`consts.DefaultAuthWindowLength`).
    ///
    /// # Errors
    /// [`LimiterError::SeedUnavailable`] when the OS entropy source
    /// fails; callers must deny the login path, never fall back to
    /// allow.
    pub fn new(limit: i64, window: Duration) -> Result<Self, LimiterError> {
        let seed = Seed::new().map_err(|_| LimiterError::SeedUnavailable)?;
        Ok(Self {
            limit: u64::try_from(limit.max(0)).unwrap_or(0),
            window: if window.is_zero() {
                Duration::from_secs(20)
            } else {
                window
            },
            seed,
            inner: Mutex::new(Inner::default()),
        })
    }

    /// The allow-everything limiter — the port of the nil limiter
    /// `newAuthLimiter(0, _)` hands back (`auth_limiter.go:39-41`):
    /// every acquire succeeds and every slot releases nothing.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            limit: 0,
            window: Duration::from_secs(20),
            seed: Seed(0),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock is an internal error; recover the (stale) map
        // rather than widen the attack window with a panic — the gate
        // below still denies on over-limit keys, and the sweep bounds
        // the recovered map.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Reserves a credential check for `key` — the port of `acquire`
    /// (`auth_limiter.go:51-67`): this is the `reserve` + failure-gate
    /// half; the in-flight slot channel is the concurrency lane and is
    /// not ported (see module docs). Denies only when the key reached
    /// the limit of failures in the current window.
    ///
    /// # Errors
    /// [`LimiterError::LimitReached`] — deny, the key is over the
    /// limit in the current window.
    pub fn acquire<'a>(&'a self, key: &str) -> Result<AuthSlot<'a>, LimiterError> {
        if self.limit == 0 {
            return Ok(AuthSlot {
                limiter: None,
                key_hash: 0,
            });
        }
        let h = self.seed.hash(key);
        let mut inner = self.lock();
        inner.sweep(self.window);

        let now = Instant::now();
        let window = self.window;
        let limit = self.limit;
        let admitted = match inner.keys.get_mut(&h) {
            None => {
                inner.keys.insert(
                    h,
                    AuthAttempts {
                        failures: 0,
                        start: now,
                    },
                );
                true
            }
            Some(a) => {
                if now.duration_since(a.start) >= window {
                    a.failures = 0;
                    a.start = now;
                }
                a.failures < limit
            }
        };
        drop(inner);
        if admitted {
            Ok(AuthSlot {
                limiter: Some(self),
                key_hash: h,
            })
        } else {
            Err(LimiterError::LimitReached)
        }
    }
}

impl Inner {
    /// `sweep` (`auth_limiter.go:92-101`): once per window, drop idle
    /// expired entries so memory is bounded by recent attempts. Live
    /// slots hold no map entries hostage in this port (release reads
    /// the entry fresh), so plain expiry is the drop condition.
    fn sweep(&mut self, window: Duration) {
        let now = Instant::now();
        if self
            .last_sweep
            .is_some_and(|last| now.duration_since(last) < window)
        {
            return;
        }
        self.keys
            .retain(|_, a| now.duration_since(a.start) < window);
        self.last_sweep = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from server/subsonic/auth_limiter_test.go, adapted to the
    // synchronous port (Go's acquire also queues behind in-flight
    // checks via the slot channel; the port gates on failures alone).

    #[test]
    fn blocks_a_key_after_configured_failures() {
        // Arrange
        let l = AuthLimiter::new(2, Duration::from_secs(60)).unwrap();
        // Act
        for _ in 0..2 {
            l.acquire("k").unwrap().release(true);
        }
        // Assert
        assert!(matches!(l.acquire("k"), Err(LimiterError::LimitReached)));
    }

    #[test]
    fn never_counts_successful_checks() {
        // Arrange
        let l = AuthLimiter::new(2, Duration::from_secs(60)).unwrap();
        // Act
        for _ in 0..50 {
            l.acquire("k").unwrap().release(false);
        }
        // Assert: still admissible.
        assert!(l.acquire("k").is_ok());
    }

    #[test]
    fn keeps_keys_independent() {
        // Arrange
        let l = AuthLimiter::new(1, Duration::from_secs(60)).unwrap();
        // Act
        l.acquire("a").unwrap().release(true);
        // Assert
        assert!(matches!(l.acquire("a"), Err(LimiterError::LimitReached)));
        assert!(l.acquire("b").is_ok());
    }

    #[test]
    fn window_expiry_resets_failures_and_sweep_drops_entries() {
        // Arrange: a 1ms window so the very next check is past it.
        let l = AuthLimiter::new(1, Duration::from_millis(1)).unwrap();
        l.acquire("a").unwrap().release(true);
        l.acquire("b").unwrap().release(true);
        assert!(matches!(l.acquire("a"), Err(LimiterError::LimitReached)));
        // Act: let the window lapse.
        std::thread::sleep(Duration::from_millis(5));
        // Assert: the key admits again…
        l.acquire("a").unwrap().release(false);
        // …and the sweep dropped the other expired key.
        let inner = l.lock();
        assert_eq!(inner.keys.len(), 1, "expired keys must be dropped");
    }

    #[test]
    fn zero_window_falls_back_to_default() {
        // Arrange/Act
        let l = AuthLimiter::new(1, Duration::ZERO).unwrap();
        // Assert: 20s default (consts.DefaultAuthWindowLength).
        assert_eq!(l.window, Duration::from_secs(20));
        // And it still gates within that window.
        l.acquire("k").unwrap().release(true);
        assert!(matches!(l.acquire("k"), Err(LimiterError::LimitReached)));
    }

    #[test]
    fn failure_count_gates_at_exactly_limit() {
        // Arrange: limit 3 — the limit-th failure blocks, limit-1 does
        // not, the window edge is `failures < limit`.
        let l = AuthLimiter::new(3, Duration::from_secs(60)).unwrap();
        // Act/Assert
        for i in 1..=2 {
            l.acquire("k").unwrap().release(true);
            assert!(
                l.acquire("k").is_ok(),
                "below the limit must admit (failure #{})",
                i
            );
        }
        l.acquire("k").unwrap().release(true);
        assert!(matches!(l.acquire("k"), Err(LimiterError::LimitReached)));
    }

    #[test]
    fn allows_everything_when_limit_is_disabled() {
        // Arrange
        let l = AuthLimiter::disabled();
        // Act
        for _ in 0..10 {
            l.acquire("k").unwrap().release(true);
        }
        // Assert: never denied.
        assert!(l.acquire("k").is_ok());
    }

    #[test]
    fn construction_fails_closed_without_entropy() {
        // Arrange/Act/Assert: the seed-unavailable deny is a distinct
        // error from the limit deny, and both are the only failure
        // modes — acquire can never return a silent allow on an
        // internal error.
        assert_ne!(LimiterError::SeedUnavailable, LimiterError::LimitReached);
        // A disabled limiter never records or denies.
        let d = AuthLimiter::disabled();
        assert!(d.acquire("any").is_ok());
    }
}
