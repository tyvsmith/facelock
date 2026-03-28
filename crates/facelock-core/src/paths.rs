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

fn effective_uid_from_status(status: &str) -> Option<u32> {
    status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| {
            let mut fields = line.split_whitespace();
            let _label = fields.next()?;
            let _real = fields.next()?;
            fields.next()?.parse::<u32>().ok()
        })
}

fn is_privileged_effective_uid(effective_uid: Option<u32>) -> bool {
    effective_uid.is_none_or(|euid| euid == 0)
}

fn is_privileged_process() -> bool {
    let effective_uid = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| effective_uid_from_status(&status));
    is_privileged_effective_uid(effective_uid)
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
    use std::sync::Mutex;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    // These tests mutate shared process-wide state (env vars, process overrides)
    // and must not run concurrently. We keep them as separate #[test] functions
    // but serialize them with TEST_MUTEX to avoid races when cargo runs tests in
    #[test]
    fn config_path_default_and_env_override() {
        let _guard = TEST_MUTEX.lock().unwrap();
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
        let _guard = TEST_MUTEX.lock().unwrap();
        let resolved = resolve_config_path(None, Some("/tmp/test-facelock.toml"), true);
        assert_eq!(resolved, PathBuf::from(DEFAULT_CONFIG_PATH));
    }

    #[test]
    fn process_override_beats_env_and_privilege_rules() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let path = PathBuf::from("/tmp/explicit.toml");
        let resolved = resolve_config_path(Some(&path), Some("/tmp/test-facelock.toml"), true);
        assert_eq!(resolved, path);
    }

    #[test]
    fn effective_uid_parser_extracts_euid() {
        let _guard = TEST_MUTEX.lock().unwrap();
        assert_eq!(
            effective_uid_from_status("Name:\tbash\nUid:\t1000\t0\t1000\t1000\n"),
            Some(0)
        );
    }

    #[test]
    fn missing_or_unreadable_uid_fails_safe_to_privileged() {
        let _guard = TEST_MUTEX.lock().unwrap();
        assert_eq!(effective_uid_from_status("Name:\tbash\n"), None);
        assert!(is_privileged_effective_uid(None));
        assert!(is_privileged_effective_uid(Some(0)));
        assert!(!is_privileged_effective_uid(Some(1000)));
    }

    #[test]
    fn config_path_uses_process_override() {
        let _guard = TEST_MUTEX.lock().unwrap();
        clear_process_config_override();
        set_process_config_override(PathBuf::from("/tmp/process-override.toml"));
        assert_eq!(config_path(), PathBuf::from("/tmp/process-override.toml"));
        clear_process_config_override();
    }
}
