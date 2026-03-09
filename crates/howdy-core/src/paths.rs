use std::path::PathBuf;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/howdy/config.toml";
pub const DEFAULT_SOCKET_PATH: &str = "/run/howdy/howdy.sock";
pub const DEFAULT_MODEL_DIR: &str = "/var/lib/howdy/models";
pub const DEFAULT_DB_PATH: &str = "/var/lib/howdy/howdy.db";
pub const DEFAULT_SNAPSHOT_DIR: &str = "/var/log/howdy/snapshots";

/// Returns the config file path, respecting `HOWDY_CONFIG` env var.
pub fn config_path() -> PathBuf {
    std::env::var("HOWDY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_default() {
        // Clear env var to test default
        unsafe { std::env::remove_var("HOWDY_CONFIG") };
        assert_eq!(config_path(), PathBuf::from(DEFAULT_CONFIG_PATH));
    }

    #[test]
    fn config_path_env_override() {
        unsafe { std::env::set_var("HOWDY_CONFIG", "/tmp/test-howdy.toml") };
        assert_eq!(config_path(), PathBuf::from("/tmp/test-howdy.toml"));
        unsafe { std::env::remove_var("HOWDY_CONFIG") };
    }
}
