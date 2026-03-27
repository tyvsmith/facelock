use std::path::PathBuf;
use std::sync::RwLock;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/facelock/config.toml";
pub const DEFAULT_MODEL_DIR: &str = "/var/lib/facelock/models";
pub const DEFAULT_DB_PATH: &str = "/var/lib/facelock/facelock.db";
pub const DEFAULT_SNAPSHOT_DIR: &str = "/var/log/facelock/snapshots";

static PROCESS_CONFIG_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

fn resolve_config_path(
    process_override: Option<&PathBuf>,
    env_override: Option<&str>,
    is_privileged: bool,
) -> PathBuf {
    if let Some(path) = process_override {
        return path.clone();
    }

    if !is_privileged {
        if let Some(path) = env_override {
            return PathBuf::from(path);
        }
    }

    PathBuf::from(DEFAULT_CONFIG_PATH)
}

fn process_config_override() -> Option<PathBuf> {
    PROCESS_CONFIG_OVERRIDE
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

fn is_privileged_process() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };

    status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| {
            let mut fields = line.split_whitespace();
            let _label = fields.next()?;
            let _real = fields.next()?;
            fields.next()?.parse::<u32>().ok()
        })
        .is_some_and(|euid| euid == 0)
}

/// Set a process-local config override path.
/// This is preferred over environment variables for privileged commands.
pub fn set_process_config_override(path: PathBuf) {
    if let Ok(mut guard) = PROCESS_CONFIG_OVERRIDE.write() {
        *guard = Some(path);
    }
}

/// Clear the process-local config override path.
pub fn clear_process_config_override() {
    if let Ok(mut guard) = PROCESS_CONFIG_OVERRIDE.write() {
        *guard = None;
    }
}

/// Returns the config file path.
///
/// Resolution order:
/// 1. Process-local override set by the CLI (`--config`)
/// 2. `FACELOCK_CONFIG` env var for unprivileged processes only
/// 3. Default path
pub fn config_path() -> PathBuf {
    let env_override = std::env::var("FACELOCK_CONFIG").ok();
    let process_override = process_config_override();
    resolve_config_path(
        process_override.as_ref(),
        env_override.as_deref(),
        is_privileged_process(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests must run sequentially in a single test because they mutate
    // a shared process-wide env var. Separate #[test] functions race when
    // cargo runs tests in parallel.
    #[test]
    fn config_path_default_and_env_override() {
        unsafe { std::env::remove_var("FACELOCK_CONFIG") };
        clear_process_config_override();
        let resolved = resolve_config_path(None, None, false);
        assert_eq!(resolved, PathBuf::from(DEFAULT_CONFIG_PATH));

        let resolved = resolve_config_path(None, Some("/tmp/test-facelock.toml"), false);
        assert_eq!(resolved, PathBuf::from("/tmp/test-facelock.toml"));

        unsafe { std::env::set_var("FACELOCK_CONFIG", "/tmp/test-facelock.toml") };
        unsafe { std::env::remove_var("FACELOCK_CONFIG") };
    }

    #[test]
    fn privileged_process_ignores_env_override() {
        let resolved = resolve_config_path(None, Some("/tmp/test-facelock.toml"), true);
        assert_eq!(resolved, PathBuf::from(DEFAULT_CONFIG_PATH));
    }

    #[test]
    fn process_override_beats_env_and_privilege_rules() {
        let path = PathBuf::from("/tmp/explicit.toml");
        let resolved = resolve_config_path(Some(&path), Some("/tmp/test-facelock.toml"), true);
        assert_eq!(resolved, path);
    }

    #[test]
    fn config_path_uses_process_override() {
        clear_process_config_override();
        set_process_config_override(PathBuf::from("/tmp/process-override.toml"));
        assert_eq!(config_path(), PathBuf::from("/tmp/process-override.toml"));
        clear_process_config_override();
    }
}
