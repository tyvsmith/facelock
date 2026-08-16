//! Per-user enrollment marker files backing `facelock is-enrolled`.
//!
//! Layout (see [`crate::state_layout`], which owns it):
//!
//! ```text
//! /var/lib/facelock/                   0710 root:facelock  traverse-only, not listable
//! /var/lib/facelock/enrolled/          0710 root:facelock  markers only
//! /var/lib/facelock/enrolled/<user>    0600 <user>:<user>
//! ```
//!
//! The markers live **inside** the state directory, and reaching one takes
//! membership of the `facelock` group: both directories grant the group
//! traversal (`g+x`) and everyone else nothing. That is deliberate —
//! `is-enrolled` means *"is face auth operational for me"*, and the group is
//! required to reach the daemon at all, so a caller outside it reads `EACCES`,
//! reports not-enrolled, and that is the **correct** answer, not a failure
//! mode. One `open(2)` answers group-membership and enrollment together.
//!
//! Traverse-only (`0710`, no `r`) means a group member can open its own marker
//! by name but cannot `readdir` the directory, so which *other* accounts have
//! face auth enrolled stays private. Each marker is `0600` and owned by its
//! user, so "am I enrolled?" is answerable by that user and nobody else — the
//! same privacy property as `~/.ssh/authorized_keys`.
//!
//! The marker is a **hint, not authority**; see the module docs in
//! [`crate::commands::is_enrolled`]. Every write is best-effort: a marker that
//! cannot be written must never fail the enrollment (or removal) that produced
//! it.
//!
//! Writes are atomic (temp file + `rename`) so a concurrent read never
//! observes a half-written marker.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use facelock_core::Config;
use facelock_core::fs_security::{ensure_private_dir, write_file};

/// Fallback marker directory when the configured DB path yields no parent.
pub const DEFAULT_MARKER_DIR: &str = "/var/lib/facelock/enrolled";

/// Group-traversable but not listable — see the module docs and the ownership
/// contract on [`crate::state_layout::ENROLLED_DIR_MODE`], which this equals.
pub const MARKER_DIR_MODE: u32 = crate::state_layout::ENROLLED_DIR_MODE;

/// Readable only by the user the marker describes.
pub const MARKER_FILE_MODE: u32 = 0o600;

/// On-disk marker contents: `{"models": N, "updated": "<ISO8601>"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub models: u32,
    pub updated: String,
}

/// What a read of one user's marker found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerState {
    /// Marker present and names at least one model.
    Enrolled(Marker),
    /// No usable marker. `ENOENT` and `EACCES` both land here (§3.3) — an
    /// unreadable marker is indistinguishable from an absent one to the caller,
    /// and neither is an error.
    Absent,
    /// The marker exists but is broken: unparseable, or an I/O failure that is
    /// not "missing" and not "denied".
    Unreadable(String),
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Marker directory for this installation: `enrolled/` inside the state
/// directory.
///
/// Derived rather than hardcoded so a pinned `storage.db_path` keeps its
/// markers alongside it — which is also what lets the tests point an entire
/// installation at a tempdir. For the default
/// `/var/lib/facelock/facelock.db` this yields `/var/lib/facelock/enrolled`.
///
/// [`crate::state_layout::state_dir_for_db`] owns the derivation, so this and
/// the layout that creates the directory cannot disagree about where it is.
pub fn marker_dir(config: &Config) -> PathBuf {
    marker_dir_for_db(Path::new(&config.storage.db_path))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MARKER_DIR))
}

/// The marker directory for a database path, or `None` when the path has no
/// usable state directory to sit in.
fn marker_dir_for_db(db_path: &Path) -> Option<PathBuf> {
    crate::state_layout::state_dir_for_db(db_path)
        .map(|state_dir| state_dir.join(crate::state_layout::ENROLLED_DIR_NAME))
}

/// Marker directory derived from the on-disk config, falling back to
/// [`DEFAULT_MARKER_DIR`] when the config is missing or unreadable.
///
/// Reads a file and nothing else — safe on the unprivileged `is-enrolled` path.
///
/// Deliberate load (D7): `is-enrolled` is dispatched before main's shared
/// config parse precisely so it can tolerate a missing or broken config here.
pub fn marker_dir_or_default() -> PathBuf {
    Config::load()
        .map(|config| marker_dir(&config))
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_MARKER_DIR))
}

/// Path of one user's marker inside `base`.
///
/// Rejects names that are not a single path component; a marker path is joined
/// from caller-supplied text (`--user`), so `..` or an embedded `/` must never
/// reach the filesystem.
pub fn marker_path(base: &Path, user: &str) -> io::Result<PathBuf> {
    if user.is_empty() || user == "." || user == ".." || user.contains('/') || user.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid user name: {user:?}"),
        ));
    }
    Ok(base.join(user))
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

/// Read one user's marker from `base`. Never panics, never blocks on anything
/// but the local filesystem.
pub fn read_marker_in(base: &Path, user: &str) -> MarkerState {
    let path = match marker_path(base, user) {
        Ok(path) => path,
        Err(e) => return MarkerState::Unreadable(e.to_string()),
    };

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => return read_error_state(&e),
    };

    match serde_json::from_slice::<Marker>(&bytes) {
        // A zero-model marker should not exist, but if it does it means "not
        // enrolled" rather than "broken".
        Ok(marker) if marker.models == 0 => MarkerState::Absent,
        Ok(marker) => MarkerState::Enrolled(marker),
        Err(e) => MarkerState::Unreadable(format!("malformed marker {}: {e}", path.display())),
    }
}

/// Map an I/O failure while reading a marker onto a state.
///
/// Split out so the `EACCES` mapping is testable without needing a filesystem
/// the test user genuinely cannot read (root bypasses mode bits).
fn read_error_state(e: &io::Error) -> MarkerState {
    match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => MarkerState::Absent,
        _ => MarkerState::Unreadable(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Write / delete
// ---------------------------------------------------------------------------

/// Current time as an ISO 8601 / RFC 3339 UTC timestamp.
fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Resolve a system user to `(uid, gid)`, or `None` if no such account exists.
fn resolve_owner(user: &str) -> Option<(u32, u32)> {
    match nix::unistd::User::from_name(user) {
        Ok(Some(u)) => Some((u.uid.as_raw(), u.gid.as_raw())),
        _ => None,
    }
}

/// `chown(2)`. Mirrors the helper in `setup.rs`; duplicated rather than shared
/// because `setup.rs` is owned elsewhere.
#[cfg(unix)]
fn chown_path(path: &Path, uid: u32, gid: u32) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains embedded NUL"))?;
    if unsafe { libc::chown(c_path.as_ptr(), uid, gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn chown_path(_path: &Path, _uid: u32, _gid: u32) -> io::Result<()> {
    Ok(())
}

/// Create or overwrite a user's marker under `base`.
///
/// `owner` is applied to the temp file *before* the rename, so the marker never
/// exists at its final name with the wrong owner. Pass `None` to leave
/// ownership alone — which is what tests running unprivileged do, since `chown`
/// to another account requires root.
pub fn write_marker_in(
    base: &Path,
    user: &str,
    models: u32,
    owner: Option<(u32, u32)>,
) -> io::Result<()> {
    let final_path = marker_path(base, user)?;
    ensure_private_dir(base, MARKER_DIR_MODE)?;

    let marker = Marker {
        models,
        updated: now_iso8601(),
    };
    let body = serde_json::to_vec(&marker).map_err(io::Error::other)?;

    let tmp = base.join(format!(".{user}.{}.tmp", std::process::id()));
    let commit = (|| {
        write_file(&tmp, &body, MARKER_FILE_MODE)?;
        if let Some((uid, gid)) = owner {
            chown_path(&tmp, uid, gid)?;
        }
        fs::rename(&tmp, &final_path)
    })();

    if commit.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    commit
}

/// Delete a user's marker under `base`. A marker that is already gone is success.
pub fn remove_marker_in(base: &Path, user: &str) -> io::Result<()> {
    let path = marker_path(base, user)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Write-or-delete in one call: zero models means "no marker".
pub fn set_marker_in(
    base: &Path,
    user: &str,
    models: u32,
    owner: Option<(u32, u32)>,
) -> io::Result<()> {
    if models == 0 {
        remove_marker_in(base, user)
    } else {
        write_marker_in(base, user, models, owner)
    }
}

// ---------------------------------------------------------------------------
// Lifecycle hooks (§3.5)
// ---------------------------------------------------------------------------

/// Recompute `user`'s model count from the authoritative store and rewrite (or
/// delete) their marker.
///
/// Best-effort by contract: a marker is only a UI hint, so a failure here is
/// logged and swallowed rather than failing the enroll/remove that triggered it.
///
/// Uses the backend the calling command already selected — it may talk to the
/// daemon. **Never call this from `is-enrolled`.**
pub fn refresh(backend: &crate::backend::Backend, config: &Config, user: &str) {
    let Some(models) = count_models(backend, user) else {
        tracing::warn!(
            user,
            "could not read model count; enrollment marker not updated"
        );
        return;
    };
    set(config, user, models);
}

/// Write (or delete, at zero) `user`'s marker for an already-known model count.
///
/// Callers that hold an open store should prefer this over [`refresh`]: the
/// count is recomputed from the authoritative model list they already have,
/// which is the same cost as increment/decrement arithmetic and cannot drift.
///
/// Best-effort: failures are logged, never propagated.
pub fn set(config: &Config, user: &str, models: u32) {
    let base = marker_dir(config);
    if let Err(e) = set_marker_in(&base, user, models, resolve_owner(user)) {
        tracing::warn!(user, error = %e, "failed to update enrollment marker");
    }
}

/// Delete `user`'s marker. Best-effort, same rationale as [`refresh`].
pub fn forget(config: &Config, user: &str) {
    let base = marker_dir(config);
    if let Err(e) = remove_marker_in(&base, user) {
        tracing::warn!(user, error = %e, "failed to remove enrollment marker");
    }
}

/// Count a user's stored models through the caller's selected backend.
///
/// Returns `None` when the count could not be determined — the caller then
/// leaves the existing marker alone rather than guessing.
fn count_models(backend: &crate::backend::Backend, user: &str) -> Option<u32> {
    backend.list_models(user).ok().map(|m| m.len() as u32)
}

// ---------------------------------------------------------------------------
// Reconcile (§3.5)
// ---------------------------------------------------------------------------

/// Rebuild every marker from the authoritative database.
///
/// This is what backfills users who enrolled before markers existed, which is
/// why the feature is safe to ship into an existing install. It runs from
/// privileged `setup` and from daemon startup
/// (`commands::daemon::reconcile_enrollment_markers`) — **never from
/// `is-enrolled`**, which needs DB access it does not have and privileges it
/// must not require.
///
/// Convergence, not migration: every call re-derives the markers from the
/// database rather than replaying recorded steps, so it is idempotent, needs no
/// ordering and keeps no "has this run?" state that a restored backup or a
/// copied database could contradict. `markers_converge_idempotently` is the
/// test that pins it.
///
/// Users present in the DB but absent from `/etc/passwd` (stale rows) are
/// skipped rather than failing the whole reconcile.
pub fn reconcile_all(config: &Config) -> anyhow::Result<()> {
    let base = marker_dir(config);
    let db_path = Path::new(&config.storage.db_path);

    // No database yet: nothing to backfill. Do not create one as a side effect
    // of reconciling — setup calls this before anyone has enrolled.
    if !db_path.exists() {
        ensure_private_dir(&base, MARKER_DIR_MODE)?;
        return Ok(());
    }

    // `open_existing`: reconciling markers is a read of the authoritative
    // database, and must never bring one into being as a side effect.
    let store = facelock_store::FaceStore::open_existing(db_path)
        .map_err(|e| anyhow::anyhow!("failed to open database: {e}"))?;
    let users = store
        .list_users()
        .map_err(|e| anyhow::anyhow!("failed to list enrolled users: {e}"))?;

    ensure_private_dir(&base, MARKER_DIR_MODE)?;

    let mut wanted: Vec<String> = Vec::new();
    for user in users {
        let models = match store.list_models(&user) {
            Ok(models) => models.len() as u32,
            Err(e) => {
                tracing::warn!(user, error = %e, "skipping marker: cannot list models");
                continue;
            }
        };
        if models == 0 {
            continue;
        }
        let Some(owner) = resolve_owner(&user) else {
            tracing::debug!(user, "skipping marker: no such system account");
            continue;
        };
        if let Err(e) = write_marker_in(&base, &user, models, Some(owner)) {
            tracing::warn!(user, error = %e, "failed to write enrollment marker");
        }
        // Kept whatever the write did. `wanted` answers "the database says this
        // user is enrolled", not "the write succeeded" — pruning on a failed
        // write turns *could not refresh* into *deleted a correct marker*, and
        // a caller that reconciles on every start (daemon startup, #137) would
        // then destroy state on the first transient failure.
        wanted.push(user);
    }

    prune_markers_in(&base, &wanted)?;
    Ok(())
}

/// Delete every marker in `base` whose user is not in `keep`.
///
/// Requires read access to the directory, which `0710` grants only to root —
/// exactly the privilege level reconcile already runs at.
fn prune_markers_in(base: &Path, keep: &[String]) -> io::Result<()> {
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Leave interrupted temp files to the next writer.
        if name.starts_with('.') {
            continue;
        }
        if keep.iter().any(|u| u == name) {
            continue;
        }
        if let Err(e) = fs::remove_file(entry.path()) {
            tracing::warn!(marker = name, error = %e, "failed to prune stale enrollment marker");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::PermissionsExt;

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// An all-defaults config; every field is `#[serde(default)]`.
    fn test_config() -> Config {
        Config::parse("").expect("empty config should parse to defaults")
    }

    /// The pin. The marker directory is derived by walking up one level from
    /// `db_path`, and getting that wrong silently points the markers somewhere
    /// no `is-enrolled` will ever look.
    #[test]
    fn a_default_config_yields_the_documented_marker_directory() {
        let config = test_config();
        assert_eq!(
            config.storage.db_path, "/var/lib/facelock/facelock.db",
            "the derivation below depends on this default"
        );
        assert_eq!(
            marker_dir(&config),
            PathBuf::from("/var/lib/facelock/enrolled")
        );
        assert_eq!(marker_dir(&config), PathBuf::from(DEFAULT_MARKER_DIR));
    }

    /// The markers live *inside* the state directory, one level below it.
    #[test]
    fn marker_dir_is_inside_the_state_directory() {
        let config = test_config();
        let markers = marker_dir(&config);
        let state_dir = Path::new("/var/lib/facelock");

        assert!(
            markers.starts_with(state_dir),
            "markers belong under the state dir, got {}",
            markers.display()
        );
        assert_eq!(markers.parent(), Some(state_dir));
    }

    /// A pinned database keeps its markers with it, which is what lets a
    /// test (or an alternate install root) redirect an entire installation.
    #[test]
    fn marker_dir_follows_a_pinned_database() {
        let mut config = test_config();
        config.storage.db_path = "/srv/faces/facelock.db".into();
        assert_eq!(marker_dir(&config), PathBuf::from("/srv/faces/enrolled"));
    }

    #[test]
    fn marker_dir_falls_back_when_db_path_has_no_parent() {
        let mut config = test_config();
        config.storage.db_path = "facelock.db".into();
        assert_eq!(marker_dir(&config), PathBuf::from(DEFAULT_MARKER_DIR));
    }

    #[test]
    fn marker_path_rejects_traversal_and_empty_names() {
        let base = Path::new("/var/lib/facelock/enrolled");
        for bad in ["", ".", "..", "../etc/passwd", "a/b"] {
            let err = marker_path(base, bad).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidInput,
                "expected rejection for {bad:?}"
            );
        }
        assert_eq!(
            marker_path(base, "alice").unwrap(),
            PathBuf::from("/var/lib/facelock/enrolled/alice")
        );
    }

    #[test]
    fn directory_mode_is_0710_and_file_mode_is_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("enrolled");
        write_marker_in(&base, "alice", 2, None).unwrap();

        assert_eq!(
            mode_of(&base),
            0o710,
            "marker dir must be group-traversable, not listable, and closed to other"
        );
        assert_eq!(
            mode_of(&base.join("alice")),
            0o600,
            "marker must be private to its user"
        );
    }

    #[test]
    fn marker_json_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("enrolled");
        write_marker_in(&base, "alice", 3, None).unwrap();

        let raw = fs::read_to_string(base.join("alice")).unwrap();
        assert!(raw.contains("\"models\":3"), "got {raw}");

        match read_marker_in(&base, "alice") {
            MarkerState::Enrolled(marker) => {
                assert_eq!(marker.models, 3);
                // ISO 8601 UTC, e.g. 2026-08-12T17:04:11Z
                assert!(marker.updated.ends_with('Z'), "got {}", marker.updated);
                assert!(marker.updated.contains('T'), "got {}", marker.updated);
                assert_eq!(
                    marker,
                    serde_json::from_str::<Marker>(&raw).unwrap(),
                    "on-disk JSON must round-trip"
                );
            }
            other => panic!("expected Enrolled, got {other:?}"),
        }
    }

    #[test]
    fn missing_marker_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            read_marker_in(tmp.path(), "nobody-at-all"),
            MarkerState::Absent
        );
    }

    #[test]
    fn permission_denied_reads_as_absent_not_error() {
        // ENOENT and EACCES must be indistinguishable to the caller (§3.3).
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(read_error_state(&denied), MarkerState::Absent);
        assert_eq!(read_error_state(&missing), MarkerState::Absent);
    }

    #[test]
    fn other_io_errors_are_unreadable() {
        let broken = io::Error::from(io::ErrorKind::InvalidData);
        assert!(matches!(
            read_error_state(&broken),
            MarkerState::Unreadable(_)
        ));
    }

    #[test]
    fn corrupt_marker_reads_as_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("alice"), b"{not json").unwrap();
        assert!(matches!(
            read_marker_in(tmp.path(), "alice"),
            MarkerState::Unreadable(_)
        ));
    }

    #[test]
    fn zero_model_marker_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("alice"),
            br#"{"models":0,"updated":"2026-08-12T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(read_marker_in(tmp.path(), "alice"), MarkerState::Absent);
    }

    #[test]
    fn set_marker_deletes_at_zero_and_writes_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("enrolled");

        set_marker_in(&base, "alice", 1, None).unwrap();
        assert!(base.join("alice").exists());

        set_marker_in(&base, "alice", 0, None).unwrap();
        assert!(!base.join("alice").exists());

        // Deleting an absent marker is not an error.
        set_marker_in(&base, "alice", 0, None).unwrap();
    }

    #[test]
    fn write_leaves_no_temp_files_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("enrolled");
        write_marker_in(&base, "alice", 1, None).unwrap();
        write_marker_in(&base, "alice", 2, None).unwrap();

        let names: Vec<String> = fs::read_dir(&base)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["alice".to_string()], "got {names:?}");
    }

    #[test]
    fn prune_removes_only_unwanted_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("enrolled");
        write_marker_in(&base, "alice", 1, None).unwrap();
        write_marker_in(&base, "bob", 1, None).unwrap();
        fs::write(base.join(".partial.tmp"), b"x").unwrap();

        prune_markers_in(&base, &["alice".to_string()]).unwrap();

        assert!(base.join("alice").exists(), "kept user must survive");
        assert!(!base.join("bob").exists(), "dropped user must be pruned");
        assert!(
            base.join(".partial.tmp").exists(),
            "dot-files are not markers and must be left alone"
        );
    }

    #[test]
    fn prune_on_missing_directory_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        prune_markers_in(&tmp.path().join("nope"), &[]).unwrap();
    }

    /// Reconcile's DB half, exercised against an in-memory store so it runs
    /// unprivileged: users with models get a marker, users without lose theirs.
    #[test]
    fn reconcile_backfills_and_prunes_from_store_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("enrolled");

        let store = facelock_store::FaceStore::open_memory().unwrap();
        let emb = [0.0f32; 512];
        store.add_model("alice", "front", &emb, "").unwrap();
        store.add_model("alice", "side", &emb, "").unwrap();
        store.add_model("bob", "front", &emb, "").unwrap();

        // Pre-existing marker for a user who will be cleared, plus one for a
        // user who was never in the DB at all.
        write_marker_in(&base, "bob", 1, None).unwrap();
        write_marker_in(&base, "ghost", 1, None).unwrap();
        store.clear_user("bob").unwrap();

        let mut wanted = Vec::new();
        for user in store.list_users().unwrap() {
            let models = store.list_models(&user).unwrap().len() as u32;
            if models == 0 {
                continue;
            }
            write_marker_in(&base, &user, models, None).unwrap();
            wanted.push(user);
        }
        prune_markers_in(&base, &wanted).unwrap();

        match read_marker_in(&base, "alice") {
            MarkerState::Enrolled(m) => assert_eq!(m.models, 2, "count comes from the DB"),
            other => panic!("expected alice enrolled, got {other:?}"),
        }
        assert_eq!(
            read_marker_in(&base, "bob"),
            MarkerState::Absent,
            "a user with no models must lose their marker"
        );
        assert_eq!(
            read_marker_in(&base, "ghost"),
            MarkerState::Absent,
            "a marker with no DB rows at all must be pruned"
        );
    }

    /// End-to-end `reconcile_all` against a real on-disk database.
    ///
    /// Enrolls the *current* account so `resolve_owner` resolves and the `chown`
    /// is a no-op an unprivileged test may perform (chown to your own uid/gid).
    #[test]
    fn reconcile_all_backfills_real_database_and_skips_unknown_accounts() {
        let me = nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .ok()
            .flatten()
            .map(|u| u.name);
        let Some(me) = me else {
            // No passwd entry for the test runner; nothing to assert.
            return;
        };

        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("facelock.db");
        let mut config = test_config();
        config.storage.db_path = db_path.to_string_lossy().into_owned();

        {
            let store = facelock_store::FaceStore::create(&db_path).unwrap();
            let emb = [0.0f32; 512];
            store.add_model(&me, "front", &emb, "").unwrap();
            store.add_model(&me, "side", &emb, "").unwrap();
            // A stale row for an account that no longer exists on the system.
            store
                .add_model("facelock-no-such-account", "front", &emb, "")
                .unwrap();
        }

        let base = marker_dir(&config);
        // A marker left over from a user who has since been cleared.
        write_marker_in(&base, "facelock-stale-user", 1, None).unwrap();

        reconcile_all(&config).unwrap();

        assert_eq!(mode_of(&base), 0o710);
        match read_marker_in(&base, &me) {
            MarkerState::Enrolled(m) => assert_eq!(m.models, 2, "backfilled from the database"),
            other => panic!("expected {me} enrolled, got {other:?}"),
        }
        assert_eq!(mode_of(&base.join(&me)), 0o600);
        assert_eq!(
            read_marker_in(&base, "facelock-no-such-account"),
            MarkerState::Absent,
            "a DB row with no system account must be skipped, not fail the reconcile"
        );
        assert_eq!(
            read_marker_in(&base, "facelock-stale-user"),
            MarkerState::Absent,
            "a marker with no models behind it must be pruned"
        );
    }

    /// The observable state of a marker directory: which markers exist, what
    /// each one claims, and the mode of every entry. Deliberately excludes
    /// `updated` — it is a timestamp, nothing reads it as state, and a rewrite
    /// is expected to move it.
    fn marker_snapshot(base: &Path) -> Vec<(String, MarkerState, u32)> {
        let mut entries: Vec<(String, MarkerState, u32)> = fs::read_dir(base)
            .unwrap()
            .flatten()
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let state = match read_marker_in(base, &name) {
                    MarkerState::Enrolled(m) => MarkerState::Enrolled(Marker {
                        models: m.models,
                        updated: String::new(),
                    }),
                    other => other,
                };
                (name, state, mode_of(&e.path()))
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// **The load-bearing test.** This change ships convergence instead of a
    /// migration, and this assertion is what makes that safe: a second run
    /// re-derives the same answer from the same database, so there is nothing
    /// to record as "already applied" — and therefore no recorded state that a
    /// restored backup, a database copied between machines, or a wiped state
    /// directory could contradict.
    ///
    /// If this ever fails, convergence has become order-dependent and the
    /// callers added for #137 (daemon startup, the oneshot auth path) are no
    /// longer safe to run unconditionally.
    #[test]
    fn markers_converge_idempotently() {
        let me = nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .ok()
            .flatten()
            .map(|u| u.name);
        let Some(me) = me else {
            // No passwd entry for the test runner: reconcile can resolve no
            // owner, so there is no convergence to assert idempotent.
            return;
        };

        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("facelock.db");
        let mut config = test_config();
        config.storage.db_path = db_path.to_string_lossy().into_owned();

        {
            let store = facelock_store::FaceStore::create(&db_path).unwrap();
            let emb = [0.0f32; 512];
            store.add_model(&me, "front", &emb, "").unwrap();
            store.add_model(&me, "side", &emb, "").unwrap();
        }

        let base = marker_dir(&config);
        // Start from the upgrade's actual state: enrolled users, no markers,
        // plus one stale marker for a user who is not in the database.
        write_marker_in(&base, "facelock-stale-user", 1, None).unwrap();

        reconcile_all(&config).unwrap();
        let first = marker_snapshot(&base);

        reconcile_all(&config).unwrap();
        let second = marker_snapshot(&base);

        assert_eq!(
            first, second,
            "a second reconcile must change nothing — that property is what \
             replaces a migration ledger"
        );
        assert_eq!(
            first.len(),
            1,
            "expected exactly {me}'s marker to survive, got {first:?}"
        );
        assert_eq!(first[0].0, me);
        assert!(
            matches!(&first[0].1, MarkerState::Enrolled(m) if m.models == 2),
            "got {:?}",
            first[0].1
        );
        assert_eq!(first[0].2, MARKER_FILE_MODE);
        assert_eq!(mode_of(&base), MARKER_DIR_MODE, "directory mode is stable");
    }

    /// A marker the reconcile could not *write* must survive it.
    ///
    /// The failure is the real one: writing a marker `chown`s it to its user,
    /// which needs `CAP_CHOWN`. A daemon under the shipped systemd unit has a
    /// capability bounding set, so a missing `CAP_CHOWN` makes every marker
    /// write fail with `EPERM` — and if a failed write dropped the user from
    /// the keep-list, the very next daemon start would delete every correct
    /// marker `setup` had written. Reproduced here by `chown`ing to another
    /// account from an unprivileged test, which fails for the same reason.
    #[test]
    fn a_marker_that_cannot_be_written_is_not_pruned() {
        if nix::unistd::Uid::effective().is_root() {
            // Root has CAP_CHOWN, so the write below succeeds and there is no
            // failure to isolate.
            return;
        }
        // Any account that is not the test runner: `chown`ing to it is the
        // privileged operation being denied.
        let other = ["nobody", "daemon", "bin", "root"]
            .into_iter()
            .find(|u| resolve_owner(u).is_some());
        let Some(other) = other else {
            return;
        };

        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("facelock.db");
        let mut config = test_config();
        config.storage.db_path = db_path.to_string_lossy().into_owned();

        {
            let store = facelock_store::FaceStore::create(&db_path).unwrap();
            store.add_model(other, "front", &[0.0f32; 512], "").unwrap();
        }

        // A correct marker already on disk — what `setup` would have left.
        let base = marker_dir(&config);
        write_marker_in(&base, other, 1, None).unwrap();

        reconcile_all(&config).unwrap();

        assert!(
            matches!(read_marker_in(&base, other), MarkerState::Enrolled(m) if m.models == 1),
            "an enrolled user's marker must survive a write this reconcile \
             could not perform"
        );
    }

    #[test]
    fn reconcile_without_a_database_only_creates_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.storage.db_path = tmp
            .path()
            .join("state")
            .join("facelock.db")
            .to_string_lossy()
            .into_owned();

        reconcile_all(&config).unwrap();

        let base = marker_dir(&config);
        assert!(base.is_dir(), "reconcile must create the marker directory");
        assert_eq!(mode_of(&base), 0o710);
        assert!(
            !Path::new(&config.storage.db_path).exists(),
            "reconcile must not create a database as a side effect"
        );
    }
}
