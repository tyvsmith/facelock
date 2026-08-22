//! The `/var/lib/facelock` on-disk layout.
//!
//! ```text
//! /var/lib/facelock/            0711 root:root   traverse-only, not listable
//!   facelock.db                 0600 root:root
//!   facelock.db-wal / -shm      0600 root:root
//!   models/                     0755 root:root   public, SHA-256 verified
//!   enrolled/                   0711 root:root   is-enrolled markers only
//!     <user>                    0600 <user>:<user>
//!   pam-backups/                0700 root:root   PAM rollback state
//! ```
//!
//! Three properties are load-bearing (ADR 010):
//!
//! 1. **Traversal for everyone, listing for nobody.** Both directories carry
//!    `--x` for group and other and no `r`: any local user can open a path it
//!    already knows by name — its own `enrolled/<user>` marker, a model file —
//!    but nobody except root can enumerate the directory. Which accounts are
//!    enrolled stays private because each marker is `0600` and owned by its
//!    user, not because of who may enter the directory.
//! 2. **Every secret is locked in its own right.** The database and its
//!    sidecars are `0600 root:root`; nothing under the state directory except
//!    `models/` (public, SHA-256-verified downloads) carries "other" read or
//!    write bits. There is no group: nothing here is group-owned or
//!    group-readable.
//! 3. **Applying the layout is idempotent and never touches data.** It is a
//!    handful of `mkdir`/`chmod`/`chown` calls; the database and models never
//!    move, and nothing here creates, copies, or deletes a database. There is
//!    deliberately no migration machinery to get wrong.
//!
//! The guard tests at the bottom pin all three.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use facelock_core::Config;
use facelock_core::fs_security::ensure_private_dir;

// ---------------------------------------------------------------------------
// Names and modes
// ---------------------------------------------------------------------------

/// Per-user `is-enrolled` markers.
pub const ENROLLED_DIR_NAME: &str = "enrolled";

/// ONNX model files, directly in the state directory.
pub const MODELS_DIR_NAME: &str = "models";

/// Fixed root for PAM rollback backups and their provenance records.
///
/// This state belongs to the PAM writer, not to the configured biometric
/// database. A custom `storage.db_path` therefore cannot relocate the trust
/// root used to recover PAM mutations.
pub const PAM_BACKUPS_DIR: &str = "/var/lib/facelock/pam-backups";

/// `root:root`, traverse-only for everyone else: any local user can open
/// `enrolled/<user>` or a model file by name; nobody but root can list it.
pub const STATE_DIR_MODE: u32 = 0o711;

/// `root:root`. The models are public downloads, SHA-256 verified at load —
/// there is no reason to restrict them.
pub const MODELS_DIR_MODE: u32 = 0o755;

/// Same shape as the state directory. Traversal to a `0600 <user>:<user>`
/// marker is what `facelock is-enrolled` means by "operational for me"; the
/// marker's own mode is what keeps "am I enrolled?" answerable by that user
/// alone. No group is involved (ADR 010).
pub const ENROLLED_DIR_MODE: u32 = 0o711;

/// Backups contain complete PAM service files and are root-only.
pub const PAM_BACKUPS_DIR_MODE: u32 = 0o700;

/// The database and its `-wal`/`-shm` sidecars: `root:root`, no group access.
/// Encrypted biometric templates are read by the daemon (root) only.
pub const DB_FILE_MODE: u32 = 0o600;

const SIDECAR_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

// ---------------------------------------------------------------------------
// Deriving the layout from a config
// ---------------------------------------------------------------------------

/// The state directory implied by a database path: its parent directory.
pub fn state_dir_for_db(db_path: &Path) -> Option<&Path> {
    db_path.parent().filter(|p| !p.as_os_str().is_empty())
}

/// Every path the layout manages from one `storage.db_path`, plus the fixed
/// PAM rollback trust root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLayout {
    pub state_dir: PathBuf,
    /// The configured `daemon.model_dir` when derived from a [`Config`],
    /// otherwise the built-in `<state_dir>/models`.
    pub models_dir: PathBuf,
    pub enrolled_dir: PathBuf,
    pub pam_backups_dir: PathBuf,
    /// The configured `storage.db_path`.
    pub db_path: PathBuf,
}

impl StateLayout {
    /// Derive the layout from a database path, or `None` when the path has no
    /// usable parent (a bare filename, say).
    pub fn from_db_path(db_path: &Path) -> Option<Self> {
        let state_dir = state_dir_for_db(db_path)?.to_path_buf();
        Some(Self {
            models_dir: state_dir.join(MODELS_DIR_NAME),
            enrolled_dir: state_dir.join(ENROLLED_DIR_NAME),
            pam_backups_dir: PathBuf::from(PAM_BACKUPS_DIR),
            db_path: db_path.to_path_buf(),
            state_dir,
        })
    }

    pub fn from_config(config: &Config) -> Option<Self> {
        let mut layout = Self::from_db_path(Path::new(&config.storage.db_path))?;
        layout.models_dir = PathBuf::from(&config.daemon.model_dir);
        Some(layout)
    }

    /// The directories this layout manages, with their modes. All of them are
    /// `root:root`.
    ///
    /// A `model_dir` pinned outside the state directory is excluded: the guard
    /// property is "everything under the state directory carries these modes",
    /// and a path like `/opt/facelock-models` belongs to whoever pinned it.
    fn dir_specs(&self) -> Vec<DirSpec<'_>> {
        let mut specs = vec![DirSpec {
            path: &self.state_dir,
            mode: STATE_DIR_MODE,
        }];
        if self.models_dir.starts_with(&self.state_dir) {
            specs.push(DirSpec {
                path: &self.models_dir,
                mode: MODELS_DIR_MODE,
            });
        }
        specs.push(DirSpec {
            path: &self.enrolled_dir,
            mode: ENROLLED_DIR_MODE,
        });
        specs.push(DirSpec {
            path: &self.pam_backups_dir,
            mode: PAM_BACKUPS_DIR_MODE,
        });
        specs
    }

    /// The database file and its WAL sidecars — tightened when present, never
    /// created.
    fn db_files(&self) -> Vec<PathBuf> {
        let mut files = vec![self.db_path.clone()];
        for suffix in SIDECAR_SUFFIXES {
            let mut name = self.db_path.as_os_str().to_os_string();
            name.push(suffix);
            files.push(PathBuf::from(name));
        }
        files
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirSpec<'a> {
    path: &'a Path,
    mode: u32,
}

// ---------------------------------------------------------------------------
// Applying one path
// ---------------------------------------------------------------------------

/// `chown(2)` to `root:root`.
fn chown_root(path: &Path) -> anyhow::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains embedded NUL: {}", path.display()))?;
    if unsafe { libc::chown(c_path.as_ptr(), 0, 0) } != 0 {
        bail!(
            "failed to chown {} to root:root: {}",
            path.display(),
            io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Create-or-tighten one directory, then optionally make it `root:root`.
/// Shared with setup, which secures its other directories the same way.
///
/// Already-correct directories are left entirely alone rather than re-`chmod`ed
/// and re-`chown`ed: this can run on the PAM path on every authentication, so
/// the steady state must cost one `stat` per path and no writes.
pub(crate) fn apply_dir(path: &Path, mode: u32, enforce_owner: bool) -> anyhow::Result<()> {
    let current = fs::metadata(path).ok().filter(|m| m.is_dir());
    let mode_ok = current
        .as_ref()
        .is_some_and(|m| m.permissions().mode() & 0o7777 == mode);
    let owner_ok = !enforce_owner
        || current
            .as_ref()
            .is_some_and(|m| m.uid() == 0 && m.gid() == 0);

    if !mode_ok {
        ensure_private_dir(path, mode)
            .with_context(|| format!("failed to create or secure {}", path.display()))?;
    }
    if !owner_ok {
        chown_root(path)?;
    }
    Ok(())
}

/// Tighten one file if it exists, then optionally make it `root:root`. Never
/// creates it. Shared with setup, which secures its other files the same way.
pub(crate) fn apply_file(path: &Path, mode: u32, enforce_owner: bool) -> anyhow::Result<()> {
    let Ok(meta) = fs::metadata(path) else {
        return Ok(());
    };
    if !meta.is_file() {
        return Ok(());
    }
    if meta.permissions().mode() & 0o7777 != mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    if enforce_owner && (meta.uid() != 0 || meta.gid() != 0) {
        chown_root(path)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

/// Bring the state directory to the layout in the module docs.
///
/// Idempotent, and touches no data: directories are created or re-`chmod`ed,
/// the database is re-`chmod`ed **only if it already exists**, and nothing is
/// ever moved, copied, or deleted. Run as root it also enforces `root:root`
/// ownership; run unprivileged it applies modes alone.
pub fn apply_layout(layout: &StateLayout) -> anyhow::Result<()> {
    let enforce_owner = nix::unistd::Uid::current().is_root();
    for spec in layout.dir_specs() {
        apply_dir(spec.path, spec.mode, enforce_owner)?;
    }
    for file in layout.db_files() {
        apply_file(&file, DB_FILE_MODE, enforce_owner)?;
    }
    Ok(())
}

/// [`apply_layout`] derived from a config. A config whose `db_path` has no
/// usable parent has no layout to apply and is a no-op.
pub fn ensure_state_layout(config: &Config) -> anyhow::Result<()> {
    match StateLayout::from_config(config) {
        Some(layout) => apply_layout(&layout),
        None => Ok(()),
    }
}

/// Best-effort [`ensure_state_layout`]: applied on the authentication path and
/// in front of every direct-mode store open.
///
/// A failure here must never block the caller: the layout only sets modes,
/// which cannot change what is about to be read. It runs on these paths at all
/// so an install upgraded without re-running `setup` still converges on the
/// documented modes the first time a root invocation comes through.
pub fn ensure_state_layout_best_effort(config: &Config) {
    if let Err(e) = ensure_state_layout(config) {
        tracing::warn!(error = %format!("{e:#}"), "could not fully apply the state directory layout");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config::parse("").expect("empty config should parse to defaults")
    }

    fn layout_at(base: &Path) -> StateLayout {
        let state_dir = base.join("facelock");
        StateLayout {
            models_dir: state_dir.join(MODELS_DIR_NAME),
            enrolled_dir: state_dir.join(ENROLLED_DIR_NAME),
            pam_backups_dir: state_dir.join("pam-backups"),
            db_path: state_dir.join("facelock.db"),
            state_dir,
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    // -----------------------------------------------------------------------
    // Derivation
    // -----------------------------------------------------------------------

    #[test]
    fn default_config_yields_the_documented_layout() {
        let layout = StateLayout::from_config(&test_config()).unwrap();
        assert_eq!(layout.state_dir, Path::new("/var/lib/facelock"));
        assert_eq!(layout.db_path, Path::new("/var/lib/facelock/facelock.db"));
        assert_eq!(layout.models_dir, Path::new("/var/lib/facelock/models"));
        assert_eq!(layout.enrolled_dir, Path::new("/var/lib/facelock/enrolled"));
        assert_eq!(
            layout.pam_backups_dir,
            Path::new("/var/lib/facelock/pam-backups")
        );
    }

    #[test]
    fn a_custom_database_path_does_not_move_pam_backup_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.storage.db_path = tmp
            .path()
            .join("custom-state/facelock.db")
            .to_string_lossy()
            .into_owned();

        let layout = StateLayout::from_config(&config).unwrap();

        assert_eq!(
            layout.pam_backups_dir,
            Path::new("/var/lib/facelock/pam-backups")
        );
        assert_ne!(layout.pam_backups_dir, layout.state_dir.join("pam-backups"));
    }

    #[test]
    fn a_bare_filename_has_no_layout() {
        assert_eq!(StateLayout::from_db_path(Path::new("facelock.db")), None);
    }

    #[test]
    fn a_pinned_model_dir_outside_the_state_dir_is_not_managed() {
        let mut config = test_config();
        config.daemon.model_dir = "/opt/facelock-models".into();
        let layout = StateLayout::from_config(&config).unwrap();
        assert!(
            !layout
                .dir_specs()
                .iter()
                .any(|s| s.path == Path::new("/opt/facelock-models")),
            "a pinned model_dir belongs to whoever pinned it"
        );
    }

    // -----------------------------------------------------------------------
    // The contract: modes, pinned as data
    // -----------------------------------------------------------------------

    /// The documented layout, spelled out. A change to any mode here is a
    /// change to `docs/contracts.md` and every packaging fragment — this test
    /// failing is the reminder.
    #[test]
    fn the_layout_contract_is_the_documented_one() {
        let layout = StateLayout::from_config(&test_config()).unwrap();
        let specs = layout.dir_specs();

        let find = |path: &Path| {
            specs
                .iter()
                .find(|s| s.path == path)
                .copied()
                .unwrap_or_else(|| panic!("{} is not managed", path.display()))
        };

        assert_eq!(find(Path::new("/var/lib/facelock")).mode, 0o711);
        assert_eq!(find(Path::new("/var/lib/facelock/models")).mode, 0o755);
        assert_eq!(find(Path::new("/var/lib/facelock/enrolled")).mode, 0o711);
        assert_eq!(find(Path::new("/var/lib/facelock/pam-backups")).mode, 0o700);
        assert_eq!(DB_FILE_MODE, 0o600, "the database is root-only");
    }

    /// ADR 010: both directories are traversable by everyone (`--x` for
    /// "other") and listable by nobody but root (no `r` for group or other).
    /// Traversal is what lets an unprivileged `is-enrolled` open its own
    /// `0600` marker by name without any group membership.
    #[test]
    fn state_and_enrolled_dirs_are_traversable_by_all_and_listable_by_none() {
        for mode in [STATE_DIR_MODE, ENROLLED_DIR_MODE] {
            assert_eq!(mode & 0o007, 0o001, "other: traverse only");
            assert_eq!(mode & 0o070, 0o010, "group: traverse only, no listing");
            assert_eq!(mode & 0o700, 0o700, "root: everything");
        }
    }

    // -----------------------------------------------------------------------
    // Application
    // -----------------------------------------------------------------------

    #[test]
    fn final_modes_match_the_documented_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = layout_at(tmp.path());
        fs::create_dir_all(&layout.state_dir).unwrap();
        fs::write(&layout.db_path, b"stub").unwrap();

        apply_layout(&layout).unwrap();

        assert_eq!(mode(&layout.state_dir), STATE_DIR_MODE);
        assert_eq!(mode(&layout.models_dir), MODELS_DIR_MODE);
        assert_eq!(mode(&layout.enrolled_dir), ENROLLED_DIR_MODE);
        assert_eq!(mode(&layout.pam_backups_dir), PAM_BACKUPS_DIR_MODE);
        assert_eq!(mode(&layout.db_path), DB_FILE_MODE);
    }

    #[test]
    fn applying_the_layout_twice_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = layout_at(tmp.path());

        apply_layout(&layout).unwrap();
        apply_layout(&layout).unwrap();

        assert_eq!(mode(&layout.state_dir), STATE_DIR_MODE);
        assert_eq!(mode(&layout.enrolled_dir), ENROLLED_DIR_MODE);
        assert_eq!(mode(&layout.pam_backups_dir), PAM_BACKUPS_DIR_MODE);
    }

    #[test]
    fn a_loosened_database_is_retightened() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = layout_at(tmp.path());
        fs::create_dir_all(&layout.state_dir).unwrap();
        fs::write(&layout.db_path, b"stub").unwrap();
        fs::set_permissions(&layout.db_path, fs::Permissions::from_mode(0o640)).unwrap();

        apply_layout(&layout).unwrap();

        assert_eq!(mode(&layout.db_path), 0o600);
    }

    #[test]
    fn apply_layout_never_creates_a_database() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = layout_at(tmp.path());

        apply_layout(&layout).unwrap();

        assert!(!layout.db_path.exists(), "the layout must not create files");
        // And the sidecars stayed absent too.
        assert_eq!(
            fs::read_dir(&layout.state_dir)
                .unwrap()
                .flatten()
                .filter(|e| e.path().is_file())
                .count(),
            0
        );
    }

    #[test]
    fn ensure_state_layout_best_effort_swallows_failures() {
        // A db_path whose parent is a *file* cannot have the layout applied;
        // the auth-path wrapper must absorb that rather than propagate it.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, b"not a directory").unwrap();

        let mut config = test_config();
        config.storage.db_path = blocker.join("facelock.db").to_string_lossy().into_owned();

        ensure_state_layout_best_effort(&config);
    }

    // -----------------------------------------------------------------------
    // The guard: nothing under the state directory is readable or listable by "other"
    // -----------------------------------------------------------------------

    /// Walks the applied tree and asserts the property the layout exists for:
    /// no file under the state directory carries any "other" bit, and no
    /// directory carries "other" read or write — traversal (`--x`) is the
    /// only thing granted, so a local user can open a name it knows and
    /// enumerate nothing. `models/` is the one subtree allowed to carry
    /// "other" bits of its own (public, SHA-256-verified downloads).
    #[test]
    fn nothing_under_the_state_directory_is_readable_or_listable_by_other() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = layout_at(tmp.path());
        fs::create_dir_all(&layout.state_dir).unwrap();
        fs::write(&layout.db_path, b"stub").unwrap();

        apply_layout(&layout).unwrap();
        // Simulate later runtime artifacts.
        fs::write(layout.state_dir.join("facelock.db-wal"), b"stub").unwrap();
        apply_layout(&layout).unwrap();

        assert_eq!(
            mode(&layout.state_dir) & 0o007,
            0o001,
            "the state directory grants 'other' traversal and nothing else"
        );

        fn walk(dir: &Path, allow_other: &dyn Fn(&Path) -> bool) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let other = fs::metadata(&path).unwrap().permissions().mode() & 0o007;
                if !allow_other(&path) {
                    let allowed = if path.is_dir() { 0o001 } else { 0o000 };
                    assert_eq!(
                        other & !allowed,
                        0,
                        "{} is readable/writable by 'other'. The state directory holds \
                         biometric data: files carry no 'other' bits and directories \
                         at most traverse (models/ is the only exception — public data).",
                        path.display()
                    );
                }
                if path.is_dir() {
                    walk(&path, allow_other);
                }
            }
        }
        let models_dir = layout.models_dir.clone();
        walk(&layout.state_dir, &|p: &Path| p.starts_with(&models_dir));
    }
}
