//! Generic TTL cache — port of `utils/cache/simple_cache.go`
//! (`SimpleCache`/`simpleCache`, backed by Go's `jellydator/ttlcache`).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Options for [`SimpleCache::new`] (`cache.Options`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Maximum number of items (0 = unlimited).
    pub size_limit: u64,
    /// TTL used when none is given to [`SimpleCache::add`].
    pub default_ttl: Duration,
}

/// A single-key load in flight, shared by concurrent misses.
struct Flight<V> {
    done: Mutex<Option<V>>,
    ready: std::sync::Condvar,
}

/// A generic TTL cache with load deduplication (`cache.SimpleCache`).
///
/// In-flight single-flight loads (`Flight` per key).
type Loads<K, V> = HashMap<K, Arc<Flight<Result<V, String>>>>;

/// Unlike the Go original (which spawns a background janitor goroutine),
/// expiry is checked lazily on access and on [`SimpleCache::add`], so no
/// background task can leak in the embedded/server lifecycle.
pub struct SimpleCache<K, V> {
    data: RwLock<HashMap<K, (V, Option<Instant>)>>,
    loads: Mutex<Loads<K, V>>,
    options: Options,
}

impl<K: Eq + Hash + Clone, V: Clone> SimpleCache<K, V> {
    /// Creates a cache with the given options (`cache.NewSimpleCache`).
    #[must_use]
    pub fn new(options: Options) -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
            loads: Mutex::new(HashMap::new()),
            options,
        }
    }

    /// Adds a value with the default TTL (`Add`).
    ///
    /// # Errors
    /// When the value could not be stored (capacity exceeded).
    pub fn add(&self, key: K, value: V) -> Result<(), String> {
        self.add_with_ttl(key, value, self.options.default_ttl)
    }

    /// Adds a value with an explicit TTL (`AddWithTTL`). A zero TTL means the
    /// item never expires (Go `ttlcache.DefaultTTL` with no configured default).
    ///
    /// # Errors
    /// When the capacity limit would be exceeded.
    pub fn add_with_ttl(&self, key: K, value: V, ttl: Duration) -> Result<(), String> {
        {
            let mut data = self.data.write().expect("cache lock");
            if self.options.size_limit > 0
                && u64::try_from(data.len()).unwrap_or(u64::MAX) >= self.options.size_limit
                && !data.contains_key(&key)
            {
                return Err("failed to add item".to_string());
            }
            data.insert(key, (value, ttl_checked_deadline(ttl)));
        }
        self.evict_expired();
        Ok(())
    }

    /// Removes a key (`Remove`).
    pub fn remove(&self, key: &K) {
        self.data.write().expect("cache lock").remove(key);
    }

    /// Returns the value for `key`, or an error when missing/expired (`Get`).
    ///
    /// # Errors
    /// `item not found` when absent or expired.
    pub fn get(&self, key: &K) -> Result<V, String> {
        let guard = self.data.read().expect("cache lock");
        match guard.get(key) {
            Some((v, deadline)) if deadline.is_none_or(|d| d > Instant::now()) => Ok(v.clone()),
            _ => Err("item not found".to_string()),
        }
    }

    /// Loads misses via `loader`, deduplicating concurrent loads of the same
    /// key: one loader call runs, and every waiter shares its result or error
    /// (`GetWithLoader`).
    ///
    /// # Errors
    /// `cache error: loader returned <err>` when the loader fails.
    pub fn get_with_loader(
        &self,
        key: K,
        loader: impl FnOnce(&K) -> Result<(V, Duration), String>,
    ) -> Result<V, String> {
        if let Ok(v) = self.get(&key) {
            return Ok(v);
        }

        let flight;
        {
            let mut loads = self.loads.lock().expect("loads lock");
            if let Some(f) = loads.get(&key) {
                flight = Arc::clone(f);
                drop(loads);
                let mut done = flight.done.lock().expect("flight lock");
                while done.is_none() {
                    done = flight.ready.wait(done).expect("flight lock");
                }
                return done
                    .clone()
                    .expect("flight completed")
                    .map_err(|e| format!("cache error: loader returned {e}"));
            }
            flight = Arc::new(Flight {
                done: Mutex::new(None),
                ready: std::sync::Condvar::new(),
            });
            loads.insert(key.clone(), Arc::clone(&flight));
        }

        let result = if let Ok(v) = self.get(&key) {
            Ok((v, Duration::ZERO))
        } else {
            self.evict_expired();
            loader(&key)
        };
        let outcome = match result {
            Ok((v, ttl)) => {
                let _ = self.add_with_ttl(key.clone(), v.clone(), ttl);
                Ok(v)
            }
            Err(e) => Err(e),
        };

        {
            let mut loads = self.loads.lock().expect("loads lock");
            loads.remove(&key);
        }
        {
            let mut done = flight.done.lock().expect("flight lock");
            *done = Some(outcome.clone());
        }
        flight.ready.notify_all();
        outcome.map_err(|e| format!("cache error: loader returned {e}"))
    }

    /// Returns the non-expired keys (`Keys`).
    #[must_use]
    pub fn keys(&self) -> Vec<K> {
        let guard = self.data.read().expect("cache lock");
        let now = Instant::now();
        guard
            .iter()
            .filter(|(_, (_, d))| d.is_none_or(|d| d > now))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Returns the non-expired values (`Values`).
    #[must_use]
    pub fn values(&self) -> Vec<V> {
        let guard = self.data.read().expect("cache lock");
        let now = Instant::now();
        guard
            .values()
            .filter(|(_, d)| d.is_none_or(|d| d > now))
            .map(|(v, _)| v.clone())
            .collect()
    }

    /// Number of items currently stored (`Len`; may include expired entries
    /// until the next eviction pass, matching ttlcache's `Len`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.read().expect("cache lock").len()
    }

    /// Returns true when no items are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn evict_expired(&self) {
        let now = Instant::now();
        self.data
            .write()
            .expect("cache lock")
            .retain(|_, (_, d)| d.is_none_or(|d| d > now));
    }
}

/// A zero TTL means the item never expires: no deadline is stored at all
/// (Go `ttlcache` treats a zero/default TTL as non-expiring).
fn ttl_checked_deadline(ttl: Duration) -> Option<Instant> {
    (ttl > Duration::ZERO).then(|| Instant::now() + ttl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn add_and_get_round_trip() {
        let cache: SimpleCache<String, String> = SimpleCache::new(Options::default());
        cache
            .add("key".to_string(), "value".to_string())
            .expect("add");
        assert_eq!(cache.get(&"key".to_string()).expect("get"), "value");
    }

    #[test]
    fn add_with_ttl_expires() {
        let cache: SimpleCache<String, String> = SimpleCache::new(Options::default());
        cache
            .add_with_ttl(
                "key".to_string(),
                "value".to_string(),
                Duration::from_millis(10),
            )
            .expect("add");
        std::thread::sleep(Duration::from_millis(50));
        assert!(cache.get(&"key".to_string()).is_err());
    }

    #[test]
    fn get_with_loader_loads() {
        let cache: SimpleCache<String, String> = SimpleCache::new(Options::default());
        let v = cache
            .get_with_loader("key".to_string(), |k| {
                Ok((format!("{}=value", k), Duration::from_secs(60)))
            })
            .expect("load");
        assert_eq!(v, "key=value");
    }

    #[test]
    fn get_with_loader_returns_loader_errors() {
        let cache: SimpleCache<String, String> = SimpleCache::new(Options::default());
        let err = cache
            .get_with_loader("key".to_string(), |_| Err("some error".to_string()))
            .expect_err("must fail");
        assert!(err.contains("some error"), "{err}");
    }

    #[test]
    fn get_with_loader_suppresses_concurrent_loads() {
        let cache = Arc::new(SimpleCache::<String, String>::new(Options::default()));
        let calls = Arc::new(AtomicU32::new(0));
        let mut threads = Vec::new();
        let barrier = Arc::new(std::sync::Barrier::new(10));
        for _ in 0..10 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                cache.get_with_loader("key".to_string(), |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    Ok(("value".to_string(), Duration::from_secs(60)))
                })
            }));
        }
        for t in threads {
            t.join().expect("thread").expect("loaded");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "loader must run exactly once"
        );
    }

    #[test]
    fn remove_drops_the_entry() {
        let cache: SimpleCache<String, String> = SimpleCache::new(Options::default());
        cache.add("k".to_string(), "v".to_string()).expect("add");
        cache.remove(&"k".to_string());
        assert!(cache.get(&"k".to_string()).is_err());
    }

    #[test]
    fn keys_values_len_track_contents() {
        let cache: SimpleCache<u32, u32> = SimpleCache::new(Options::default());
        cache.add(1, 10).expect("add");
        cache
            .add_with_ttl(2, 20, Duration::from_millis(10))
            .expect("add");
        std::thread::sleep(Duration::from_millis(50));
        let keys = cache.keys();
        assert_eq!(keys, [1]);
        assert_eq!(cache.values(), [10]);
        // Eviction on add clears the expired entry.
        cache.add(3, 30).expect("add");
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn size_limit_is_enforced() {
        let cache: SimpleCache<u32, u32> = SimpleCache::new(Options {
            size_limit: 2,
            default_ttl: Duration::ZERO,
        });
        cache.add(1, 1).expect("add");
        cache.add(2, 2).expect("add");
        assert!(cache.add(3, 3).is_err());
        // Overwriting an existing key is still allowed.
        cache.add(1, 11).expect("add");
        assert_eq!(cache.get(&1).expect("get"), 11);
    }
}
