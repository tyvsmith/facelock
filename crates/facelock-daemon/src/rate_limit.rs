use facelock_store::FaceStore;

/// Per-user authentication rate limiter backed by the shared SQLite store.
/// Only failed attempts are recorded, so successful unlocks never consume the
/// budget and daemon restarts do not clear the window.
pub struct RateLimiter {
    max_failures: u32,
    window_secs: u64,
}

impl RateLimiter {
    pub fn new(max_failures: u32, window_secs: u64) -> Self {
        Self {
            max_failures,
            window_secs,
        }
    }

    /// Check if the user is rate-limited. Returns true if auth may proceed.
    pub fn check(&self, store: &FaceStore, user: &str) -> Result<bool, String> {
        // Stale rows do not affect correctness, but opportunistic cleanup keeps
        // the shared table bounded over time.
        let _ = store.cleanup_rate_limit(self.window_secs);
        store
            .check_rate_limit(user, self.max_failures, self.window_secs)
            .map_err(|e| e.to_string())
    }

    /// Record a failed authentication attempt for the user.
    pub fn record_failure(&self, store: &FaceStore, user: &str) -> Result<(), String> {
        store.record_auth_attempt(user).map_err(|e| e.to_string())?;
        let _ = store.cleanup_rate_limit(self.window_secs);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use facelock_store::FaceStore;

    use super::*;

    #[test]
    fn allows_under_limit() {
        let store = FaceStore::open_memory().unwrap();
        let rl = RateLimiter::new(3, 60);
        assert!(rl.check(&store, "alice").unwrap());
        rl.record_failure(&store, "alice").unwrap();
        assert!(rl.check(&store, "alice").unwrap());
        rl.record_failure(&store, "alice").unwrap();
        assert!(rl.check(&store, "alice").unwrap());
    }

    #[test]
    fn blocks_over_limit() {
        let store = FaceStore::open_memory().unwrap();
        let rl = RateLimiter::new(2, 60);
        assert!(rl.check(&store, "bob").unwrap());
        rl.record_failure(&store, "bob").unwrap();
        assert!(rl.check(&store, "bob").unwrap());
        rl.record_failure(&store, "bob").unwrap();
        assert!(!rl.check(&store, "bob").unwrap());
    }

    #[test]
    fn success_does_not_count() {
        let store = FaceStore::open_memory().unwrap();
        let rl = RateLimiter::new(2, 60);
        assert!(rl.check(&store, "alice").unwrap());
        assert!(rl.check(&store, "alice").unwrap());
        assert!(rl.check(&store, "alice").unwrap());
        rl.record_failure(&store, "alice").unwrap();
        rl.record_failure(&store, "alice").unwrap();
        assert!(!rl.check(&store, "alice").unwrap());
    }

    #[test]
    fn separate_users() {
        let store = FaceStore::open_memory().unwrap();
        let rl = RateLimiter::new(1, 60);
        assert!(rl.check(&store, "alice").unwrap());
        rl.record_failure(&store, "alice").unwrap();
        assert!(!rl.check(&store, "alice").unwrap());
        assert!(rl.check(&store, "bob").unwrap());
    }

    #[test]
    fn zero_limit_blocks_all() {
        let store = FaceStore::open_memory().unwrap();
        let rl = RateLimiter::new(0, 60);
        assert!(!rl.check(&store, "alice").unwrap());
    }
}
