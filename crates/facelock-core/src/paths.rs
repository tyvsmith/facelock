use std::path::PathBuf;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/facelock/config.toml";
pub const DEFAULT_SOCKET_PATH: &str = "/run/facelock/facelock.sock";
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

    #[test]
    fn config_path_default() {
        // Clear env var to test default
        unsafe { std::env::remove_var("FACELOCK_CONFIG") };
        assert_eq!(config_path(), PathBuf::from(DEFAULT_CONFIG_PATH));
    }

    #[test]
    fn config_path_env_override() {
        unsafe { std::env::set_var("FACELOCK_CONFIG", "/tmp/test-facelock.toml") };
        assert_eq!(config_path(), PathBuf::from("/tmp/test-facelock.toml"));
        unsafe { std::env::remove_var("FACELOCK_CONFIG") };
    }
}
