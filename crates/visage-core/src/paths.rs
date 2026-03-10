use std::path::PathBuf;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/visage/config.toml";
pub const DEFAULT_SOCKET_PATH: &str = "/run/visage/visage.sock";
pub const DEFAULT_MODEL_DIR: &str = "/var/lib/visage/models";
pub const DEFAULT_DB_PATH: &str = "/var/lib/visage/visage.db";
pub const DEFAULT_SNAPSHOT_DIR: &str = "/var/log/visage/snapshots";

/// Returns the config file path, respecting `VISAGE_CONFIG` env var.
pub fn config_path() -> PathBuf {
    std::env::var("VISAGE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_default() {
        // Clear env var to test default
        unsafe { std::env::remove_var("VISAGE_CONFIG") };
        assert_eq!(config_path(), PathBuf::from(DEFAULT_CONFIG_PATH));
    }

    #[test]
    fn config_path_env_override() {
        unsafe { std::env::set_var("VISAGE_CONFIG", "/tmp/test-visage.toml") };
        assert_eq!(config_path(), PathBuf::from("/tmp/test-visage.toml"));
        unsafe { std::env::remove_var("VISAGE_CONFIG") };
    }
}
