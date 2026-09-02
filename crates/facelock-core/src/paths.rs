use std::path::{Component, Path, PathBuf};
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

    if !is_privileged && let Some(path) = env_override {
        return PathBuf::from(path);
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

/// The config path this process reads, when it is not the default file.
///
/// `None` when [`config_path`] resolves to [`DEFAULT_CONFIG_PATH`], including
/// an override that spells the default another way (through a symlink or a
/// `..` component). `Some` is the answer to a question the packaged daemon
/// forces: its unit runs bare `facelock daemon`, which reads only the default
/// file, so a command running under any other file cannot treat that daemon
/// as configured the way it is (#314).
pub fn non_default_config_override() -> Option<PathBuf> {
    non_default_override_of(&config_path())
}

fn non_default_override_of(effective: &Path) -> Option<PathBuf> {
    if names_a_different_file(effective, Path::new(DEFAULT_CONFIG_PATH)) {
        Some(effective.to_path_buf())
    } else {
        None
    }
}

/// Whether two spellings name different files, decided on the filesystem as
/// far as it exists and on the spelling past that. Setup runs before the
/// config file, and on a from-source install before its directory, exists;
/// what is still unresolvable after both fails closed as "different".
fn names_a_different_file(a: &Path, b: &Path) -> bool {
    resolved_identity(a) != resolved_identity(b)
}

/// The deepest existing ancestor, resolved on the filesystem, with the
/// remaining components applied lexically. A component that does not exist
/// cannot be a symlink, so folding its `.` and `..` is exactly what the
/// kernel will do once it exists; `..` out of the resolved part goes to the
/// real parent, as it does through a symlinked directory, and `..` at the
/// root stays at the root, as the kernel resolves it. Only a spelling with no
/// existing ancestor at all, a relative path whose first component is
/// missing, is returned as spelled, which compares unequal to anything
/// resolved: the fail-closed default for what cannot be answered.
fn resolved_identity(path: &Path) -> PathBuf {
    let components: Vec<Component<'_>> = path.components().collect();
    for split in (1..=components.len()).rev() {
        let prefix: PathBuf = components[..split].iter().collect();
        let Ok(mut real) = prefix.canonicalize() else {
            continue;
        };
        for component in &components[split..] {
            match component {
                Component::Normal(name) => real.push(name),
                Component::CurDir => {}
                // `pop` is false only at the root, where `..` stays put.
                Component::ParentDir => {
                    real.pop();
                }
                Component::RootDir | Component::Prefix(_) => return path.to_path_buf(),
            }
        }
        return real;
    }
    path.to_path_buf()
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

    /// The database and models sit directly in the state directory. Pinned
    /// because the enrollment-marker directory is derived by walking *up* from
    /// `db_path`, and a change here silently moves the markers.
    #[test]
    fn state_paths_sit_directly_in_the_state_directory() {
        let state_dir = Path::new("/var/lib/facelock");
        assert_eq!(PathBuf::from(DEFAULT_DB_PATH).parent(), Some(state_dir));
        assert_eq!(PathBuf::from(DEFAULT_MODEL_DIR).parent(), Some(state_dir));
    }

    #[test]
    fn config_path_uses_process_override() {
        let _guard = TEST_MUTEX.lock().unwrap();
        clear_process_config_override();
        set_process_config_override(PathBuf::from("/tmp/process-override.toml"));
        assert_eq!(config_path(), PathBuf::from("/tmp/process-override.toml"));
        clear_process_config_override();
    }

    // -----------------------------------------------------------------------
    // Non-default override (#314): the identity question setup and backend
    // selection ask. Two spellings of one file are the same file; a file that
    // does not exist yet is compared through the directory that will hold it.
    // -----------------------------------------------------------------------

    #[test]
    fn no_override_is_not_a_non_default_override() {
        let _guard = TEST_MUTEX.lock().unwrap();
        unsafe { std::env::remove_var("FACELOCK_CONFIG") };
        clear_process_config_override();
        assert_eq!(non_default_config_override(), None);
    }

    #[test]
    fn override_naming_the_default_path_is_not_non_default() {
        let _guard = TEST_MUTEX.lock().unwrap();
        clear_process_config_override();
        set_process_config_override(PathBuf::from(DEFAULT_CONFIG_PATH));
        assert_eq!(non_default_config_override(), None);
        clear_process_config_override();
    }

    #[test]
    fn override_naming_another_file_is_reported_as_set() {
        let _guard = TEST_MUTEX.lock().unwrap();
        clear_process_config_override();
        let path = PathBuf::from("/tmp/facelock-non-default-override.toml");
        set_process_config_override(path.clone());
        assert_eq!(non_default_config_override(), Some(path));
        clear_process_config_override();
    }

    /// The same file reached through a symlinked directory is the default,
    /// not an override: the daemon would read exactly this file.
    #[test]
    fn symlinked_spelling_of_the_same_file_is_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir(&etc).unwrap();
        let default = etc.join("config.toml");
        std::fs::write(&default, "").unwrap();
        std::os::unix::fs::symlink(&etc, dir.path().join("link")).unwrap();

        let via_link = dir.path().join("link").join("config.toml");
        assert!(!names_a_different_file(&via_link, &default));
        assert!(!names_a_different_file(
            &etc.join("..").join("etc").join("config.toml"),
            &default
        ));
        assert!(names_a_different_file(&etc.join("other.toml"), &default));
    }

    /// Setup runs before the config file exists. A missing file is compared
    /// through its directory, so `--config /etc/facelock/../facelock/config.toml`
    /// on a fresh install is still the default.
    #[test]
    fn missing_file_is_compared_through_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir(&etc).unwrap();
        let default = etc.join("config.toml");
        assert!(!default.exists());

        let dotted = etc.join("..").join("etc").join("config.toml");
        assert!(!names_a_different_file(&dotted, &default));
        assert!(names_a_different_file(&etc.join("other.toml"), &default));

        let ghost = dir.path().join("ghost").join("config.toml");
        assert!(!names_a_different_file(&ghost, &ghost));
        assert!(names_a_different_file(&ghost, &default));
    }

    /// A from-source install runs the identity check before the base flow
    /// creates the config directory. With no `/etc/facelock` to resolve, the
    /// `..` spelling of the default must still be the default: the deepest
    /// existing ancestor is resolved on the filesystem and the rest of the
    /// spelling is normalized lexically, which is safe because a component
    /// that does not exist cannot be a symlink.
    #[test]
    fn dotted_spelling_is_the_default_even_before_the_config_dir_exists() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        assert!(!etc.exists());
        let default = etc.join("config.toml");

        let dotted = etc.join("..").join("etc").join("config.toml");
        assert!(!names_a_different_file(&dotted, &default));
        let cur = etc.join(".").join("config.toml");
        assert!(!names_a_different_file(&cur, &default));
        // Through a missing sibling: `ghost/../etc` is `etc`, since `ghost`
        // cannot be a symlink to anywhere.
        let via_ghost = dir
            .path()
            .join("ghost")
            .join("..")
            .join("etc")
            .join("config.toml");
        assert!(!names_a_different_file(&via_ghost, &default));
        assert!(names_a_different_file(&etc.join("other.toml"), &default));

        // `..` out of a resolved directory goes to that directory's real
        // parent, as the kernel would: through a symlinked directory that
        // exists, `link/../etc` lands beside the link's target.
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        let via_link = dir
            .path()
            .join("link")
            .join("..")
            .join("etc")
            .join("config.toml");
        assert!(!names_a_different_file(&via_link, &default));
    }

    /// `..` at the root stays at the root, as the kernel resolves `/..`, so
    /// climbing past a missing top-level directory lands where the kernel
    /// would once it exists.
    #[test]
    fn dot_dot_at_the_root_stays_at_the_root() {
        let plain = Path::new("/facelock-no-such-root/config.toml");
        assert!(!names_a_different_file(
            Path::new("/../facelock-no-such-root/config.toml"),
            plain
        ));
        assert!(!names_a_different_file(
            Path::new("/facelock-no-such-root/../../facelock-no-such-root/config.toml"),
            plain
        ));
    }

    /// What has no existing ancestor to resolve from fails closed: a relative
    /// spelling whose first component is missing is compared as written, so
    /// two lexically equal spellings of it still count as different.
    #[test]
    fn a_spelling_with_no_existing_ancestor_fails_closed() {
        let plain = Path::new("facelock-no-such-dir/config.toml");
        let dotted = Path::new("facelock-no-such-dir/../facelock-no-such-dir/config.toml");
        assert!(!plain.exists() && !Path::new("facelock-no-such-dir").exists());
        assert!(names_a_different_file(dotted, plain));
        assert!(!names_a_different_file(plain, plain));
    }

    /// An unprivileged process reads `FACELOCK_CONFIG`, so that is an override
    /// too; a privileged one ignores it, so for root only `--config` counts.
    #[test]
    fn env_override_counts_only_where_config_path_honours_it() {
        let _guard = TEST_MUTEX.lock().unwrap();
        clear_process_config_override();
        let env = PathBuf::from("/tmp/facelock-env-override.toml");
        let resolved = resolve_config_path(None, Some("/tmp/facelock-env-override.toml"), false);
        assert_eq!(non_default_override_of(&resolved), Some(env));
        let resolved = resolve_config_path(None, Some("/tmp/facelock-env-override.toml"), true);
        assert_eq!(non_default_override_of(&resolved), None);
    }
}
