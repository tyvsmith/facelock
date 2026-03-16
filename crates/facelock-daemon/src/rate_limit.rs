use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Per-user authentication rate limiter.
/// Only failed attempts count against the limit — successful auths don't
/// consume the budget so normal unlock usage never triggers rate limiting.
pub struct RateLimiter {
    failures: HashMap<String, Vec<Instant>>,
    max_failures: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_failures: u32, window_secs: u64) -> Self {
        Self {
            failures: HashMap::new(),
            max_failures,
            window: Duration::from_secs(window_secs),
        }
    }

    /// Check if the user is rate-limited. Returns true if auth may proceed.
    pub fn check(&mut self, user: &str) -> bool {
        let now = Instant::now();
        let failures = self.failures.entry(user.to_string()).or_default();
        failures.retain(|t| now.duration_since(*t) < self.window);
        failures.len() < self.max_failures as usize
    }

    /// Record a failed authentication attempt for the user.
    pub fn record_failure(&mut self, user: &str) {
        let failures = self.failures.entry(user.to_string()).or_default();
        failures.push(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_under_limit() {
        let mut rl = RateLimiter::new(3, 60);
        assert!(rl.check("alice"));
        rl.record_failure("alice");
        assert!(rl.check("alice"));
        rl.record_failure("alice");
        assert!(rl.check("alice"));
    }

    #[test]
    fn blocks_over_limit() {
        let mut rl = RateLimiter::new(2, 60);
        assert!(rl.check("bob"));
        rl.record_failure("bob");
        assert!(rl.check("bob"));
        rl.record_failure("bob");
        assert!(!rl.check("bob"));
    }

    #[test]
    fn success_does_not_count() {
        let mut rl = RateLimiter::new(2, 60);
        // Two checks without recording failure — should never block
        assert!(rl.check("alice"));
        assert!(rl.check("alice"));
        assert!(rl.check("alice"));
        // Now record failures
        rl.record_failure("alice");
        rl.record_failure("alice");
        assert!(!rl.check("alice"));
    }

    #[test]
    fn separate_users() {
        let mut rl = RateLimiter::new(1, 60);
        assert!(rl.check("alice"));
        rl.record_failure("alice");
        assert!(!rl.check("alice"));
        // Different user is unaffected
        assert!(rl.check("bob"));
    }

    #[test]
    fn zero_limit_blocks_all() {
        let mut rl = RateLimiter::new(0, 60);
        assert!(!rl.check("alice"));
    }
}
