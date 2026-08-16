//! thread-safe ttl cache for player stats.
//!
//! tracker.gg only updates every ~5min so we cache hard and never poll.
//! fetches are event-driven (new player shows up in match -> fetch once).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const DEFAULT_TTL: Duration = Duration::from_secs(300);

/// thread-safe map with ttl expiry.
pub struct TtlCache<V: Clone> {
    ttl: Duration,
    data: Mutex<HashMap<String, (Instant, Duration, V)>>,
}

impl<V: Clone> TtlCache<V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            data: Mutex::new(HashMap::new()),
        }
    }

    /// cached val if present and not expired.
    pub fn get(&self, key: &str) -> Option<V> {
        let mut data = self.data.lock().unwrap();
        match data.get(key) {
            Some((ts, entry_ttl, value)) => {
                if ts.elapsed() > *entry_ttl {
                    data.remove(key);
                    None
                } else {
                    Some(value.clone())
                }
            }
            None => None,
        }
    }

    /// store a val, optional per-entry ttl override.
    pub fn set(&self, key: impl Into<String>, value: V, ttl: Option<Duration>) {
        let mut data = self.data.lock().unwrap();
        data.insert(key.into(), (Instant::now(), ttl.unwrap_or(self.ttl), value));
    }

    pub fn invalidate(&self, key: &str) {
        self.data.lock().unwrap().remove(key);
    }

    pub fn clear(&self) {
        self.data.lock().unwrap().clear();
    }

    /// drop expired entries, returns how many got removed.
    pub fn invalidate_old(&self) -> usize {
        let mut data = self.data.lock().unwrap();
        let before = data.len();
        data.retain(|_, (ts, entry_ttl, _)| ts.elapsed() <= *entry_ttl);
        before - data.len()
    }

    pub fn len(&self) -> usize {
        self.data.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
