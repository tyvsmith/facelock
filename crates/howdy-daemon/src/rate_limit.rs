use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Per-user authentication rate limiter.
pub struct RateLimiter {
    attempts: HashMap<String, Vec<Instant>>,
    max_attempts: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_attempts: u32, window_secs: u64) -> Self {
        Self {
            attempts: HashMap::new(),
            max_attempts,
            window: Duration::from_secs(window_secs),
        }
    }

    /// Check if the user is rate-limited. If not, records the attempt and returns true.
    /// Returns false if the user has exceeded the limit.
    pub fn check_and_record(&mut self, user: &str) -> bool {
        let now = Instant::now();
        let attempts = self.attempts.entry(user.to_string()).or_default();
        attempts.retain(|t| now.duration_since(*t) < self.window);
        if attempts.len() >= self.max_attempts as usize {
            return false;
        }
        attempts.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_under_limit() {
        let mut rl = RateLimiter::new(3, 60);
        assert!(rl.check_and_record("alice"));
        assert!(rl.check_and_record("alice"));
        assert!(rl.check_and_record("alice"));
    }

    #[test]
    fn blocks_over_limit() {
        let mut rl = RateLimiter::new(2, 60);
        assert!(rl.check_and_record("bob"));
        assert!(rl.check_and_record("bob"));
        assert!(!rl.check_and_record("bob"));
    }

    #[test]
    fn separate_users() {
        let mut rl = RateLimiter::new(1, 60);
        assert!(rl.check_and_record("alice"));
        assert!(!rl.check_and_record("alice"));
        // Different user is unaffected
        assert!(rl.check_and_record("bob"));
    }

    #[test]
    fn zero_limit_blocks_all() {
        let mut rl = RateLimiter::new(0, 60);
        assert!(!rl.check_and_record("alice"));
    }
}
