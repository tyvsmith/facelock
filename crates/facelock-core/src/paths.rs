use std::path::PathBuf;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/facelock/config.toml";
pub const DEFAULT_MODEL_DIR: &str = "/var/lib/facelock/models";
pub const DEFAULT_DB_PATH: &str = "/var/lib/facelock/facelock.db";
pub const DEFAULT_SNAPSHOT_DIR: &str = "/var/log/facelock/snapshots";

/// Returns the config file path, respecting `FACELOCK_CONFIG` env var.
pub fn config_path() -> PathBuf {
    std::env::var("FACELOCK_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests must run sequentially in a single test because they mutate
    // a shared process-wide env var. Separate #[test] functions race when
    // cargo runs tests in parallel.
    #[test]
    fn config_path_default_and_env_override() {
        // Test default (env var not set)
        unsafe { std::env::remove_var("FACELOCK_CONFIG") };
        assert_eq!(config_path(), PathBuf::from(DEFAULT_CONFIG_PATH));

        // Test env var override
        unsafe { std::env::set_var("FACELOCK_CONFIG", "/tmp/test-facelock.toml") };
        assert_eq!(config_path(), PathBuf::from("/tmp/test-facelock.toml"));
        unsafe { std::env::remove_var("FACELOCK_CONFIG") };
    }
}
