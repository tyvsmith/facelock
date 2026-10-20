//! Exclusive lifecycle ownership for destructive CLI maintenance (#233).
//!
//! A Debian package purge runs inside a package transaction that stops the
//! daemon and owns the exclusion interval (`docs/contracts.md`, "Fixed-root
//! purge boundary"). A user-invoked `facelock data purge` has no package
//! manager holding that interval, and stopping `facelock-daemon.service`
//! alone is not enough: `org.facelock.Daemon.service` is a D-Bus activation
//! file, so any `Authenticate` call from any PAM stack re-activates the
//! daemon mid-operation — recreating state directories underneath a
//! traversal that already passed them, or holding a database open while it
//! is unlinked. [`LifecycleLease`] establishes the interval itself:
//!
//! 1. take the canonical `/run/facelock/lifecycle.lock` — the same
//!    never-unlinked lock the source install holds (#221) — with a
//!    non-blocking exclusive `flock` on a validated mode-0600 zero-byte
//!    single-link regular file;
//! 2. prove every existing `org.facelock.Daemon.service` activation
//!    definition delegates to systemd (`SystemdService=`), because a unit
//!    mask cannot inhibit direct D-Bus execution;
//! 3. record the unit's active and enabled state, create the owned
//!    activation barrier — an empty mode-0600 regular file at
//!    `/run/systemd/system.control/facelock-daemon.service`, the mechanism
//!    the source install established — reload the manager and prove
//!    `LoadState=masked`;
//! 4. stop the daemon and prove both `ActiveState=inactive` and that
//!    nothing owns `org.facelock.Daemon` on the system bus.
//!
//! Dropping the lease restores exactly the prior state in reverse order:
//! barrier removal proven against the held descriptor, manager reload,
//! restart only if the daemon was active, then lock release. Enablement is
//! never changed. [`LifecycleLease::release`] does the same with error
//! reporting, and [`LifecycleLease::release_leaving_activation_barred`]
//! keeps the barrier for a caller that uninstalls next (#233's
//! explicit-restore contract).
//!
//! # Why the runtime control barrier and not `systemctl mask`
//!
//! `systemctl mask` writes a persistent symlink under
//! `/etc/systemd/system`. If the purge process is SIGKILLed before cleanup,
//! that symlink survives reboot and the machine stays unable to
//! authenticate by face until someone diagnoses it. The barrier file lives
//! on tmpfs, so the worst SIGKILL outcome is bounded: face authentication
//! stays masked (password PAM fallback is unaffected), the `flock` dies
//! with the process, and a reboot fully recovers on its own. Before reboot,
//! the next lease acquisition adopts a control-path file matching the exact
//! barrier identity (empty, mode 0600, single-link, expected owner) as a
//! stale barrier and removes it on restore; anything else at that path is
//! preserved and reported. Manual recovery is
//! `rm /run/systemd/system.control/facelock-daemon.service &&
//! systemctl daemon-reload`.
//!
//! # Signals
//!
//! HUP, INT and TERM run the same restore before the process dies, guarded
//! non-reentrantly against the normal drop path (the pattern the source
//! install's signal cleanup established). Restoration does not begin until
//! the operation acknowledges: the signal thread raises
//! [`LifecycleLease::interrupt_flag`], then waits, bounded by
//! [`AcquireTuning::signal_ack_timeout`], for
//! [`LifecycleLease::mark_operation_finished`] before unmasking or
//! restarting anything. The composed caller runs the engine polling the
//! flag at each deletion boundary and calls `mark_operation_finished` as
//! soon as the engine returns, so the daemon cannot come back while a
//! cooperating traversal is still deleting. If the acknowledgement never
//! arrives, the wait expires and the process dies with activation still
//! barred — the recoverable SIGKILL state above — because restarting the
//! daemon under an operation that may still be mutating the purge roots is
//! the exact race this lease exists to prevent.

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use facelock_core::dbus_interface::BUS_NAME;

/// The systemd unit whose activation this module inhibits.
pub const UNIT_NAME: &str = "facelock-daemon.service";

/// Largest D-Bus activation definition the delegation proof will read,
/// matching the source-install lifecycle's bound.
const ACTIVATION_MAX_BYTES: u64 = 65536;

/// Why a lease could not be acquired, held, or released. Every variant is a
/// refusal to act without exclusion, and each message says what to do next.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error(
        "systemd is not the running service manager ({0} is missing); \
         without it D-Bus activation of the daemon cannot be inhibited, \
         refusing to proceed"
    )]
    NoSystemd(PathBuf),
    #[error(
        "another facelock lifecycle operation holds {0}; \
         wait for it to finish and retry"
    )]
    LockHeld(PathBuf),
    #[error("{path}: {reason}")]
    Untrusted { path: PathBuf, reason: String },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "{path} does not delegate D-Bus activation to systemd ({reason}); \
         a unit mask cannot inhibit direct D-Bus execution, refusing to proceed"
    )]
    ActivationNotDelegated { path: PathBuf, reason: String },
    #[error("{UNIT_NAME} is in state {state}; {advice}")]
    UnitStateNotAdmissible { state: String, advice: String },
    #[error(
        "{path} exists but is not a facelock activation barrier ({reason}); \
         remove it (then run `systemctl daemon-reload`) or reboot, and retry"
    )]
    ControlPathConflict { path: PathBuf, reason: String },
    #[error(
        "activation barrier at {path} is not effective: LoadState is \
         {load_state}, expected masked; check for a higher-priority unit \
         under /etc/systemd/system.control"
    )]
    BarrierNotEffective { path: PathBuf, load_state: String },
    #[error(
        "{UNIT_NAME} did not reach ActiveState=inactive within {timeout:?} \
         (last state {state}); refusing to proceed without a \
         confirmed-stopped daemon"
    )]
    DaemonNotStopped { state: String, timeout: Duration },
    #[error(
        "{BUS_NAME} still has an owner on the system bus after {timeout:?}; \
         a daemon outside systemd's control is running — stop it, then retry"
    )]
    NameStillOwned { timeout: Duration },
    #[error(
        "system bus is unreachable ({detail}); cannot confirm the daemon \
         released its bus name, refusing to proceed"
    )]
    BusUnreachable { detail: String },
    #[error("systemctl {action} failed: {detail}")]
    Systemctl { action: String, detail: String },
    #[error(
        "could not arm signal-safe restore ({0}); refusing to hold the \
         lease without it"
    )]
    SignalArm(#[source] io::Error),
    #[error("lifecycle restore incomplete: {0}")]
    RestoreIncomplete(String),
}

/// The fixed runtime paths the lease operates on, all resolved under one
/// prefix so tests can run against a fixture root. The system layout uses
/// `/` and expects `root:root` ownership; a rooted layout binds expectations
/// to the invoking identity, mirroring the PAM backup store's rule.
#[derive(Debug, Clone)]
pub struct LifecycleLayout {
    prefix: PathBuf,
    expected_owner: (u32, u32),
}

impl LifecycleLayout {
    /// The production layout: `/` with `root:root` ownership expectations.
    pub fn system() -> Self {
        Self {
            prefix: PathBuf::from("/"),
            expected_owner: (0, 0),
        }
    }

    /// A fixture layout rooted at `prefix`. Ownership expectations bind to
    /// the invoking identity rather than whatever the fixture reports.
    pub fn rooted_at(prefix: impl Into<PathBuf>) -> Self {
        Self {
            prefix: prefix.into(),
            // SAFETY: geteuid/getegid cannot fail.
            expected_owner: (unsafe { libc::geteuid() }, unsafe { libc::getegid() }),
        }
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.prefix.join(relative)
    }

    /// The document check for a booted systemd.
    pub fn systemd_marker(&self) -> PathBuf {
        self.at("run/systemd/system")
    }

    /// Parent of the canonical lifecycle lock.
    pub fn lock_dir(&self) -> PathBuf {
        self.at("run/facelock")
    }

    /// The canonical cross-entrypoint lifecycle lock (never unlinked).
    pub fn lock_path(&self) -> PathBuf {
        self.at("run/facelock/lifecycle.lock")
    }

    /// systemd's runtime control-tier unit directory.
    pub fn control_dir(&self) -> PathBuf {
        self.at("run/systemd/system.control")
    }

    /// The owned activation barrier path.
    pub fn barrier_path(&self) -> PathBuf {
        self.at("run/systemd/system.control/facelock-daemon.service")
    }

    /// Every D-Bus system-services directory a `org.facelock.Daemon.service`
    /// activation definition can live in, matching the source-install
    /// lifecycle's list.
    pub fn activation_definition_paths(&self) -> Vec<PathBuf> {
        [
            "etc/dbus-1/system-services/org.facelock.Daemon.service",
            "run/dbus-1/system-services/org.facelock.Daemon.service",
            "usr/local/share/dbus-1/system-services/org.facelock.Daemon.service",
            "usr/share/dbus-1/system-services/org.facelock.Daemon.service",
            "lib/dbus-1/system-services/org.facelock.Daemon.service",
        ]
        .iter()
        .map(|relative| self.at(relative))
        .collect()
    }
}

/// The three unit properties the lease records and proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitProperties {
    pub load_state: String,
    pub active_state: String,
    pub unit_file_state: String,
}

/// The systemd operations the lease needs, behind a seam so the state
/// machine is testable without a manager. Production is [`Systemctl`].
pub trait UnitManager: Send {
    fn show(&self) -> Result<UnitProperties, LifecycleError>;
    fn daemon_reload(&self) -> Result<(), LifecycleError>;
    fn stop(&self) -> Result<(), LifecycleError>;
    fn start(&self) -> Result<(), LifecycleError>;
}

/// The one bus question the lease asks: does anything own the daemon's
/// name right now. `NameHasOwner` never triggers activation.
pub trait BusProbe {
    fn daemon_name_has_owner(&self) -> Result<bool, LifecycleError>;
}

/// Poll bounds for the stop and bus-release proofs, and the signal path's
/// acknowledgement wait. Defaults suit a real systemd; tests shrink them to
/// fail fast.
#[derive(Debug, Clone)]
pub struct AcquireTuning {
    pub stop_timeout: Duration,
    pub bus_timeout: Duration,
    pub poll_interval: Duration,
    /// How long a signal-triggered restore waits for
    /// [`LifecycleLease::mark_operation_finished`] before giving up and
    /// leaving activation barred. The default is two orders of magnitude
    /// above a worst-case single engine deletion step (one quarantine
    /// rename, unlink, and revalidation, milliseconds even with fsync on
    /// rotational media), and well inside systemd's default 90-second stop
    /// timeout, so a TERM delivered during shutdown settles, restored or
    /// deliberately barred, before SIGKILL follows.
    pub signal_ack_timeout: Duration,
}

impl Default for AcquireTuning {
    fn default() -> Self {
        Self {
            stop_timeout: Duration::from_secs(10),
            bus_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(100),
            signal_ack_timeout: Duration::from_secs(5),
        }
    }
}

/// Open `path` with `O_NOFOLLOW | O_CLOEXEC` plus `flags`. `mode` applies
/// only when `flags` carries `O_CREAT`.
fn open_nofollow(path: &Path, flags: libc::c_int, mode: libc::mode_t) -> io::Result<fs::File> {
    let bytes = path.as_os_str().as_bytes();
    let c_path =
        std::ffi::CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `c_path` is NUL-terminated and outlives the call. The returned
    // fd is checked and transferred exactly once into `File`.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            libc::c_uint::from(mode),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `open` returned a new owned descriptor.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

fn io_error(path: &Path, source: io::Error) -> LifecycleError {
    LifecycleError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn untrusted(path: &Path, reason: impl Into<String>) -> LifecycleError {
    LifecycleError::Untrusted {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

/// Open a directory we may have just created, repair a umask-clipped mode
/// on the created branch only, and require the exact trusted identity:
/// expected owner, mode 0755, a real directory reached without following a
/// symlink.
fn open_trusted_dir(
    path: &Path,
    created: bool,
    expected_owner: (u32, u32),
) -> Result<fs::File, LifecycleError> {
    let dir = open_nofollow(path, libc::O_RDONLY | libc::O_DIRECTORY, 0)
        .map_err(|e| io_error(path, e))?;
    let meta = dir.metadata().map_err(|e| io_error(path, e))?;
    if created && meta.mode() & 0o7777 != 0o755 {
        dir.set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(|e| io_error(path, e))?;
    }
    let meta = dir.metadata().map_err(|e| io_error(path, e))?;
    if (meta.uid(), meta.gid()) != expected_owner {
        return Err(untrusted(
            path,
            format!(
                "directory owner is {}:{}, expected {}:{}",
                meta.uid(),
                meta.gid(),
                expected_owner.0,
                expected_owner.1
            ),
        ));
    }
    if meta.mode() & 0o7777 != 0o755 {
        return Err(untrusted(
            path,
            format!(
                "directory mode is {:o}, expected 0755",
                meta.mode() & 0o7777
            ),
        ));
    }
    Ok(dir)
}

/// Take the canonical lifecycle lock: create-or-validate the 0755 lock
/// directory, open the never-unlinked lock file without following a
/// symlink, require the exact 0600 zero-byte single-link identity, take a
/// non-blocking exclusive `flock`, and prove the public name still resolves
/// to the locked inode. The lock is released by closing the returned file
/// and is never unlinked.
fn acquire_lifecycle_lock(layout: &LifecycleLayout) -> Result<fs::File, LifecycleError> {
    let dir_path = layout.lock_dir();
    let created_dir = match fs::DirBuilder::new().mode(0o755).create(&dir_path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(io_error(&dir_path, e)),
    };
    let _dir = open_trusted_dir(&dir_path, created_dir, layout.expected_owner)?;

    let lock_path = layout.lock_path();
    let (lock, created_lock) = match open_nofollow(&lock_path, libc::O_RDWR, 0) {
        Ok(file) => (file, false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let file = open_nofollow(
                &lock_path,
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
            .map_err(|e| io_error(&lock_path, e))?;
            (file, true)
        }
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(untrusted(&lock_path, "is a symlink"));
        }
        Err(e) => return Err(io_error(&lock_path, e)),
    };
    let meta = lock.metadata().map_err(|e| io_error(&lock_path, e))?;
    if created_lock && meta.mode() & 0o7777 != 0o600 {
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| io_error(&lock_path, e))?;
    }
    let meta = lock.metadata().map_err(|e| io_error(&lock_path, e))?;
    if !meta.file_type().is_file()
        || meta.nlink() != 1
        || meta.len() != 0
        || meta.mode() & 0o7777 != 0o600
        || (meta.uid(), meta.gid()) != layout.expected_owner
    {
        return Err(untrusted(
            &lock_path,
            "is not the expected zero-byte single-link mode-0600 lock file",
        ));
    }
    // SAFETY: `lock` is a live descriptor. Closing the file releases the
    // advisory lock.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK)
            || error.raw_os_error() == Some(libc::EAGAIN)
        {
            return Err(LifecycleError::LockHeld(lock_path));
        }
        return Err(io_error(&lock_path, error));
    }
    // The name must still resolve to the inode the lock was taken on;
    // otherwise a replaced file would let two holders coexist.
    let on_disk = fs::symlink_metadata(&lock_path).map_err(|e| io_error(&lock_path, e))?;
    if (on_disk.dev(), on_disk.ino()) != (meta.dev(), meta.ino()) {
        return Err(untrusted(&lock_path, "was replaced during acquisition"));
    }
    Ok(lock)
}

/// Prove one activation definition delegates to systemd. Returns the
/// refusal reason on any grammar the source-install lifecycle would not
/// admit; unknown keys and sections fail closed rather than being guessed
/// about.
fn activation_delegates(text: &str) -> Result<(), String> {
    let mut in_service = false;
    let mut sections = 0u32;
    let mut name: Option<&str> = None;
    let mut systemd_service: Option<&str> = None;
    let mut execs = 0u32;
    let mut users = 0u32;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[D-BUS Service]" {
            sections += 1;
            in_service = true;
            continue;
        }
        if line.starts_with('[') {
            return Err(format!("unexpected section {line}"));
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            return Err(format!("unparseable line {line:?}"));
        };
        if !in_service {
            return Err("key outside [D-BUS Service]".to_string());
        }
        let key = raw_key.trim_end();
        let value = raw_value.trim_start();
        match key {
            "Name" => {
                if name.replace(value).is_some() {
                    return Err("duplicate Name".to_string());
                }
            }
            "SystemdService" => {
                if systemd_service.replace(value).is_some() {
                    return Err("duplicate SystemdService".to_string());
                }
            }
            "Exec" => {
                execs += 1;
                if execs > 1 {
                    return Err("duplicate Exec".to_string());
                }
            }
            "User" => {
                users += 1;
                if users > 1 {
                    return Err("duplicate User".to_string());
                }
            }
            other => return Err(format!("unrecognized key {other}")),
        }
    }
    if sections != 1 {
        return Err(format!(
            "expected exactly one [D-BUS Service] section, found {sections}"
        ));
    }
    match name {
        Some(BUS_NAME) => {}
        Some(other) => return Err(format!("Name is {other}, expected {BUS_NAME}")),
        None => return Err("no Name key".to_string()),
    }
    match systemd_service {
        Some(UNIT_NAME) => Ok(()),
        Some(other) => Err(format!("SystemdService is {other}, expected {UNIT_NAME}")),
        None => {
            Err("no SystemdService key; dbus-daemon would execute the daemon directly".to_string())
        }
    }
}

/// Every existing activation definition must delegate through
/// `SystemdService=`; a single non-delegating copy would defeat the unit
/// mask. Absent definitions are fine — with none, D-Bus cannot activate the
/// daemon at all and the barrier still blocks a manual `systemctl start`.
fn verify_activation_delegation(layout: &LifecycleLayout) -> Result<(), LifecycleError> {
    for path in layout.activation_definition_paths() {
        let file = match open_nofollow(&path, libc::O_RDONLY, 0) {
            Ok(file) => file,
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ENOTDIR) =>
            {
                continue;
            }
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Err(untrusted(&path, "is a symlink"));
            }
            Err(e) => return Err(io_error(&path, e)),
        };
        let meta = file.metadata().map_err(|e| io_error(&path, e))?;
        if !meta.file_type().is_file() {
            return Err(untrusted(&path, "is not a regular file"));
        }
        if (meta.uid(), meta.gid()) != layout.expected_owner {
            return Err(untrusted(&path, "has an unexpected owner"));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(untrusted(&path, "is group- or world-writable"));
        }
        if meta.len() > ACTIVATION_MAX_BYTES {
            return Err(untrusted(&path, "is larger than 64 KiB"));
        }
        let mut text = String::new();
        {
            use std::io::Read;
            (&file)
                .take(ACTIVATION_MAX_BYTES)
                .read_to_string(&mut text)
                .map_err(|e| io_error(&path, e))?;
        }
        activation_delegates(&text).map_err(|reason| LifecycleError::ActivationNotDelegated {
            path: path.clone(),
            reason,
        })?;
    }
    Ok(())
}

/// The owned activation barrier: a descriptor-held empty file at systemd's
/// runtime control-tier unit path. While present and reloaded, the manager
/// treats the unit as masked, which blocks both D-Bus activation (which
/// delegates via `SystemdService=`) and a manual `systemctl start`.
struct BarrierGuard {
    file: fs::File,
    path: PathBuf,
    created_control_dir: bool,
}

enum ControlEntry {
    Absent,
    Stale(fs::File),
}

/// Classify what is at the control path. An exact barrier identity is a
/// stale barrier from an interrupted lifecycle operation — safe to adopt,
/// because the held lifecycle lock proves no live holder exists. Anything
/// else is preserved and reported.
fn probe_control_path(layout: &LifecycleLayout) -> Result<ControlEntry, LifecycleError> {
    let path = layout.barrier_path();
    let file = match open_nofollow(&path, libc::O_RDWR, 0) {
        Ok(file) => file,
        Err(e)
            if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) =>
        {
            return Ok(ControlEntry::Absent);
        }
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(LifecycleError::ControlPathConflict {
                path,
                reason: "it is a symlink".to_string(),
            });
        }
        Err(e) => return Err(io_error(&path, e)),
    };
    let meta = file.metadata().map_err(|e| io_error(&path, e))?;
    if meta.file_type().is_file()
        && meta.nlink() == 1
        && meta.len() == 0
        && meta.mode() & 0o7777 == 0o600
        && (meta.uid(), meta.gid()) == layout.expected_owner
    {
        return Ok(ControlEntry::Stale(file));
    }
    Err(LifecycleError::ControlPathConflict {
        path,
        reason: format!(
            "found {} of {} bytes, mode {:o}, owner {}:{}",
            if meta.file_type().is_file() {
                "a regular file"
            } else {
                "a non-regular object"
            },
            meta.len(),
            meta.mode() & 0o7777,
            meta.uid(),
            meta.gid()
        ),
    })
}

/// Create the barrier no-clobber and validate its identity.
fn create_barrier(layout: &LifecycleLayout) -> Result<BarrierGuard, LifecycleError> {
    let dir_path = layout.control_dir();
    let created_control_dir = match fs::DirBuilder::new().mode(0o755).create(&dir_path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(io_error(&dir_path, e)),
    };
    let _dir = open_trusted_dir(&dir_path, created_control_dir, layout.expected_owner)?;
    let path = layout.barrier_path();
    let file = match open_nofollow(&path, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(LifecycleError::ControlPathConflict {
                path,
                reason: "it appeared while the barrier was being created".to_string(),
            });
        }
        Err(e) => return Err(io_error(&path, e)),
    };
    let meta = file.metadata().map_err(|e| io_error(&path, e))?;
    if meta.mode() & 0o7777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| io_error(&path, e))?;
    }
    let meta = file.metadata().map_err(|e| io_error(&path, e))?;
    if !meta.file_type().is_file()
        || meta.nlink() != 1
        || meta.len() != 0
        || meta.mode() & 0o7777 != 0o600
        || (meta.uid(), meta.gid()) != layout.expected_owner
    {
        return Err(untrusted(&path, "created barrier lost its identity"));
    }
    Ok(BarrierGuard {
        file,
        path,
        created_control_dir,
    })
}

/// Unlink the barrier only if the public name still resolves to the held
/// descriptor's inode; a replaced object is preserved and reported.
fn remove_barrier(barrier: &BarrierGuard) -> Result<(), String> {
    let held = barrier
        .file
        .metadata()
        .map_err(|e| format!("{}: {e}", barrier.path.display()))?;
    let on_disk = match fs::symlink_metadata(&barrier.path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", barrier.path.display())),
    };
    if (on_disk.dev(), on_disk.ino()) != (held.dev(), held.ino()) {
        return Err(format!(
            "{} was replaced while the lease was held; left in place",
            barrier.path.display()
        ));
    }
    fs::remove_file(&barrier.path).map_err(|e| format!("{}: {e}", barrier.path.display()))
}

/// Everything restore needs, owned by whichever exit path runs first: the
/// normal drop, an explicit release, or the signal thread. `Option::take`
/// on the shared slot makes restoration non-reentrant.
struct RestoreState {
    layout: LifecycleLayout,
    unit: Box<dyn UnitManager>,
    barrier: Option<BarrierGuard>,
    restart: bool,
    lock: fs::File,
}

enum RestoreMode {
    Full,
    LeaveBarred,
}

/// Restore in reverse acquisition order: remove the barrier (proven against
/// the held descriptor), reload the manager and prove the mask is gone,
/// restart the daemon only if it was active at acquisition, then release
/// the lock by closing it — the lock file itself is never unlinked. Every
/// step runs even when an earlier one fails, so a partial failure restores
/// as much as it can and reports the rest.
fn restore(state: RestoreState, mode: RestoreMode) -> Result<(), LifecycleError> {
    let RestoreState {
        layout,
        unit,
        barrier,
        restart,
        lock,
    } = state;
    let mut errors: Vec<String> = Vec::new();
    match mode {
        RestoreMode::Full => {
            if let Some(barrier) = barrier {
                if let Err(reason) = remove_barrier(&barrier) {
                    errors.push(reason);
                }
                let created_dir = barrier.created_control_dir;
                drop(barrier.file);
                if created_dir {
                    // Only an empty directory we created goes away; rmdir
                    // refuses anything else.
                    let _ = fs::remove_dir(layout.control_dir());
                }
                match unit.daemon_reload() {
                    Ok(()) => match unit.show() {
                        Ok(props) if props.load_state == "masked" => {
                            errors.push(format!(
                                "{UNIT_NAME} is still masked after barrier removal; \
                                 check {} and /etc/systemd/system.control",
                                layout.control_dir().display()
                            ));
                        }
                        Ok(_) => {}
                        Err(e) => errors.push(format!("could not verify unmasking: {e}")),
                    },
                    Err(e) => errors.push(format!("daemon-reload after barrier removal: {e}")),
                }
            }
            if restart && let Err(e) = unit.start() {
                errors.push(format!("could not restart {UNIT_NAME}: {e}"));
            }
        }
        RestoreMode::LeaveBarred => {
            if let Some(barrier) = barrier {
                // `warn`, not `info`: the CLI's default `facelock=warn`
                // filter drops info, and this is the only place the user is
                // told where the barrier is and how to undo it. Face
                // authentication stays off until they act on it.
                tracing::warn!(
                    "leaving activation barred at {}; remove it and run \
                     `systemctl daemon-reload` (or reboot) to restore face \
                     authentication",
                    barrier.path.display()
                );
                drop(barrier.file);
            }
        }
    }
    // Last, after every restoration step: closing the descriptor releases
    // the flock for the next lifecycle entrypoint.
    drop(lock);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(LifecycleError::RestoreIncomplete(errors.join("; ")))
    }
}

type SharedRestore = Arc<Mutex<Option<RestoreState>>>;

fn take_restore(shared: &SharedRestore) -> Option<RestoreState> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// The acknowledgement channel between the operation and the signal thread.
/// `mark_finished` says nothing is touching the filesystem any more;
/// `wait_finished` blocks a signal-triggered restore until then, bounded.
struct OperationGate {
    finished: Mutex<bool>,
    condvar: Condvar,
}

impl OperationGate {
    fn new() -> Self {
        Self {
            finished: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    fn mark_finished(&self) {
        let mut finished = self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *finished = true;
        self.condvar.notify_all();
    }

    /// Whether the operation acknowledged within `timeout`.
    fn wait_finished(&self, timeout: Duration) -> bool {
        let guard = self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (finished, _) = self
            .condvar
            .wait_timeout_while(guard, timeout, |finished| !*finished)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *finished
    }
}

/// The armed signal watcher: HUP, INT and TERM raise the interrupt flag,
/// wait bounded for the operation's acknowledgement, run the shared restore
/// (full after an acknowledgement, activation left barred without one),
/// then hand the signal its default outcome so the process still dies with
/// the correct wait status.
struct SignalWatch {
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SignalWatch {
    fn arm(
        shared: SharedRestore,
        interrupted: Arc<AtomicBool>,
        gate: Arc<OperationGate>,
        ack_timeout: Duration,
        terminate: Box<dyn Fn(i32) + Send>,
    ) -> io::Result<Self> {
        let mut signals = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGHUP,
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
        ])?;
        let handle = signals.handle();
        let thread = std::thread::Builder::new()
            .name("facelock-lifecycle-signals".to_string())
            .spawn(move || {
                if let Some(signal) = signals.forever().next() {
                    interrupted.store(true, Ordering::SeqCst);
                    // Restoring before the operation stops touching the
                    // filesystem would restart the daemon under a live
                    // traversal — the race the barrier exists to prevent.
                    let acknowledged = gate.wait_finished(ack_timeout);
                    if let Some(state) = take_restore(&shared) {
                        let mode = if acknowledged {
                            RestoreMode::Full
                        } else {
                            tracing::error!(
                                signal,
                                "operation did not acknowledge within {ack_timeout:?}; \
                                 leaving activation barred (recoverable: the next \
                                 lifecycle acquisition adopts the barrier, or reboot)"
                            );
                            RestoreMode::LeaveBarred
                        };
                        if let Err(e) = restore(state, mode) {
                            tracing::error!(signal, "lifecycle restore on signal incomplete: {e}");
                        }
                    }
                    terminate(signal);
                }
            })?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }

    fn disarm(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SignalWatch {
    fn drop(&mut self) {
        self.disarm();
    }
}

/// Exclusive ownership of the daemon lifecycle for the duration of a
/// destructive operation. See the module documentation for the acquisition
/// proofs and the restore contract.
pub struct LifecycleLease {
    shared: SharedRestore,
    interrupted: Arc<AtomicBool>,
    gate: Arc<OperationGate>,
    signal_ack_timeout: Duration,
    signals: Option<SignalWatch>,
    was_active: bool,
    prior_unit_file_state: String,
}

impl std::fmt::Debug for LifecycleLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleLease")
            .field("was_active", &self.was_active)
            .field("prior_unit_file_state", &self.prior_unit_file_state)
            .field("signals_armed", &self.signals.is_some())
            .finish_non_exhaustive()
    }
}

impl LifecycleLease {
    /// Acquire against the live system: `/` layout, `systemctl`, the system
    /// bus, default timeouts, signal-safe restore armed.
    pub fn acquire_system() -> Result<Self, LifecycleError> {
        Self::acquire(
            LifecycleLayout::system(),
            Box::new(Systemctl),
            &SystemBus,
            AcquireTuning::default(),
            true,
        )
    }

    /// Acquire with explicit collaborators. `arm_signals` exists so tests
    /// can drive the signal path deterministically; production callers pass
    /// `true` — the lease refuses to exist without signal-safe restore.
    pub fn acquire(
        layout: LifecycleLayout,
        unit: Box<dyn UnitManager>,
        bus: &dyn BusProbe,
        tuning: AcquireTuning,
        arm_signals: bool,
    ) -> Result<Self, LifecycleError> {
        let marker = layout.systemd_marker();
        if !marker.is_dir() {
            return Err(LifecycleError::NoSystemd(marker));
        }
        let lock = acquire_lifecycle_lock(&layout)?;
        let mut staged = RestoreState {
            layout,
            unit,
            barrier: None,
            restart: false,
            lock,
        };
        let prior = match stage(&mut staged, bus, &tuning) {
            Ok(prior) => prior,
            Err(error) => {
                if let Err(rollback) = restore(staged, RestoreMode::Full) {
                    tracing::error!(
                        "rollback after failed lease acquisition incomplete: {rollback}"
                    );
                }
                return Err(error);
            }
        };
        let mut lease = Self {
            shared: Arc::new(Mutex::new(Some(staged))),
            interrupted: Arc::new(AtomicBool::new(false)),
            gate: Arc::new(OperationGate::new()),
            signal_ack_timeout: tuning.signal_ack_timeout,
            signals: None,
            was_active: prior.active,
            prior_unit_file_state: prior.unit_file_state,
        };
        if arm_signals {
            lease.arm_signals_with(Box::new(|signal| {
                let _ = signal_hook::low_level::emulate_default_handler(signal);
            }))?;
        }
        Ok(lease)
    }

    /// Arm HUP/INT/TERM restoration. `terminate` runs after restore with
    /// the delivered signal; production emulates the default handler so the
    /// process dies with the correct wait status.
    fn arm_signals_with(
        &mut self,
        terminate: Box<dyn Fn(i32) + Send>,
    ) -> Result<(), LifecycleError> {
        match SignalWatch::arm(
            self.shared.clone(),
            self.interrupted.clone(),
            self.gate.clone(),
            self.signal_ack_timeout,
            terminate,
        ) {
            Ok(watch) => {
                self.signals = Some(watch);
                Ok(())
            }
            Err(error) => {
                if let Some(state) = take_restore(&self.shared)
                    && let Err(rollback) = restore(state, RestoreMode::Full)
                {
                    tracing::error!("rollback after failed signal arming incomplete: {rollback}");
                }
                Err(LifecycleError::SignalArm(error))
            }
        }
    }

    /// Whether the daemon was active when the lease was acquired (and will
    /// be restarted on restore).
    pub fn was_active(&self) -> bool {
        self.was_active
    }

    /// The unit's enabled state at acquisition. Recorded only — the lease
    /// never changes enablement.
    pub fn prior_unit_file_state(&self) -> &str {
        &self.prior_unit_file_state
    }

    /// Raised when a signal arrives, before any restoration. The traversal
    /// engine composed on top must poll this at each deletion boundary and
    /// stop when it is set; restoration then waits for
    /// [`Self::mark_operation_finished`], so the restored daemon never
    /// overlaps a cooperating traversal at all.
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        self.interrupted.clone()
    }

    /// Acknowledge that the operation has stopped touching the filesystem.
    /// The composing CLI calls this immediately after the engine returns,
    /// on every path, interrupted or not. A signal-triggered restore blocks
    /// on this acknowledgement (bounded by
    /// [`AcquireTuning::signal_ack_timeout`]); if the bound expires the
    /// process dies with activation left barred instead of restarting the
    /// daemon under an operation that may still be running. Idempotent.
    pub fn mark_operation_finished(&self) {
        self.gate.mark_finished();
    }

    /// Restore the prior state and report any step that could not be
    /// undone. Dropping the lease performs the same restore, logging
    /// instead of returning.
    pub fn release(mut self) -> Result<(), LifecycleError> {
        // Releasing implies the operation is done; a signal thread already
        // waiting must wake now or the disarm join below would stall.
        self.gate.mark_finished();
        if let Some(mut watch) = self.signals.take() {
            watch.disarm();
        }
        match take_restore(&self.shared) {
            Some(state) => restore(state, RestoreMode::Full),
            None => Ok(()),
        }
    }

    /// Release the lock but leave activation barred, for a caller that
    /// uninstalls next (#233: inhibition holds "until the caller completes
    /// uninstall or explicitly restores service state"). The daemon stays
    /// stopped and masked; the returned path names the barrier to remove —
    /// or a reboot clears it, since it lives on tmpfs.
    pub fn release_leaving_activation_barred(mut self) -> Result<PathBuf, LifecycleError> {
        self.gate.mark_finished();
        if let Some(mut watch) = self.signals.take() {
            watch.disarm();
        }
        match take_restore(&self.shared) {
            Some(state) => {
                let path = state.layout.barrier_path();
                restore(state, RestoreMode::LeaveBarred)?;
                Ok(path)
            }
            None => Err(LifecycleError::RestoreIncomplete(
                "lease was already released".to_string(),
            )),
        }
    }
}

impl Drop for LifecycleLease {
    fn drop(&mut self) {
        self.gate.mark_finished();
        if let Some(mut watch) = self.signals.take() {
            watch.disarm();
        }
        if let Some(state) = take_restore(&self.shared)
            && let Err(e) = restore(state, RestoreMode::Full)
        {
            tracing::error!("lifecycle restore on drop incomplete: {e}");
        }
    }
}

struct PriorState {
    active: bool,
    unit_file_state: String,
}

/// Which recorded states are admissible for taking the lease. Mirrors the
/// source install: loaded active/inactive state (or a unit that does not
/// exist), plus — only when a stale barrier was adopted — the masked state
/// that barrier explains.
fn admit(props: &UnitProperties, adopted_stale_barrier: bool) -> Result<(), LifecycleError> {
    match props.load_state.as_str() {
        "loaded" | "not-found" => {}
        "masked" if adopted_stale_barrier => {}
        "masked" => {
            return Err(LifecycleError::UnitStateNotAdmissible {
                state: "masked".to_string(),
                advice: "the unit is masked outside facelock's control; \
                         inspect /run/systemd/system.control and \
                         /etc/systemd/system.control for a foreign \
                         control-tier file and remove it, then run \
                         `systemctl daemon-reload` and retry (`systemctl \
                         unmask` clears an /etc mask symlink, not a \
                         control-tier mask)"
                    .to_string(),
            });
        }
        other => {
            return Err(LifecycleError::UnitStateNotAdmissible {
                state: other.to_string(),
                advice: "unrecognized LoadState; resolve the unit's state, then retry".to_string(),
            });
        }
    }
    match props.active_state.as_str() {
        "active" | "inactive" => Ok(()),
        "failed" => Err(LifecycleError::UnitStateNotAdmissible {
            state: "failed".to_string(),
            advice: format!("run `systemctl reset-failed {UNIT_NAME}`, then retry"),
        }),
        other => Err(LifecycleError::UnitStateNotAdmissible {
            state: other.to_string(),
            advice: "wait for the transition to settle, then retry".to_string(),
        }),
    }
}

/// The acquisition state machine, mutating `staged` as it goes so a
/// failure at any step hands the accumulated state to `restore`.
fn stage(
    staged: &mut RestoreState,
    bus: &dyn BusProbe,
    tuning: &AcquireTuning,
) -> Result<PriorState, LifecycleError> {
    verify_activation_delegation(&staged.layout)?;
    let adopted = match probe_control_path(&staged.layout)? {
        ControlEntry::Absent => None,
        ControlEntry::Stale(file) => {
            let path = staged.layout.barrier_path();
            tracing::warn!(
                "adopting stale activation barrier at {} left by an \
                 interrupted lifecycle operation; it will be removed on restore",
                path.display()
            );
            Some(BarrierGuard {
                file,
                path,
                created_control_dir: false,
            })
        }
    };
    let props = staged.unit.show()?;
    admit(&props, adopted.is_some())?;
    let prior = PriorState {
        active: props.active_state == "active",
        unit_file_state: props.unit_file_state,
    };
    staged.barrier = Some(match adopted {
        Some(barrier) => barrier,
        None => create_barrier(&staged.layout)?,
    });
    staged.unit.daemon_reload()?;
    let shown = staged.unit.show()?;
    if shown.load_state != "masked" {
        return Err(LifecycleError::BarrierNotEffective {
            path: staged.layout.barrier_path(),
            load_state: shown.load_state,
        });
    }
    if prior.active {
        // Set before the stop so a rollback of a half-stopped daemon still
        // restarts it.
        staged.restart = true;
        staged.unit.stop()?;
    }
    wait_until_inactive(staged.unit.as_ref(), tuning)?;
    wait_until_name_released(bus, tuning)?;
    Ok(prior)
}

fn wait_until_inactive(
    unit: &dyn UnitManager,
    tuning: &AcquireTuning,
) -> Result<(), LifecycleError> {
    let deadline = Instant::now() + tuning.stop_timeout;
    loop {
        let props = unit.show()?;
        if props.active_state == "inactive" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::DaemonNotStopped {
                state: props.active_state,
                timeout: tuning.stop_timeout,
            });
        }
        std::thread::sleep(tuning.poll_interval);
    }
}

fn wait_until_name_released(
    bus: &dyn BusProbe,
    tuning: &AcquireTuning,
) -> Result<(), LifecycleError> {
    let deadline = Instant::now() + tuning.bus_timeout;
    loop {
        if !bus.daemon_name_has_owner()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::NameStillOwned {
                timeout: tuning.bus_timeout,
            });
        }
        std::thread::sleep(tuning.poll_interval);
    }
}

/// Production [`UnitManager`]: `systemctl`, matching how the rest of the
/// CLI drives systemd.
pub struct Systemctl;

impl Systemctl {
    fn run(&self, args: &[&str]) -> Result<std::process::Output, LifecycleError> {
        std::process::Command::new("systemctl")
            .args(args)
            .output()
            .map_err(|e| LifecycleError::Systemctl {
                action: args.join(" "),
                detail: e.to_string(),
            })
    }

    fn run_expect_success(&self, args: &[&str]) -> Result<(), LifecycleError> {
        let output = self.run(args)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(LifecycleError::Systemctl {
                action: args.join(" "),
                detail: format!(
                    "{}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            })
        }
    }
}

/// Parse `systemctl show` output for the three requested properties.
/// Missing, duplicate, or unrequested properties fail closed, mirroring the
/// source install's malformed/duplicate-property abort.
fn parse_show_output(text: &str) -> Result<UnitProperties, String> {
    let mut load_state: Option<String> = None;
    let mut active_state: Option<String> = None;
    let mut unit_file_state: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("unparseable line {line:?}"));
        };
        let slot = match key {
            "LoadState" => &mut load_state,
            "ActiveState" => &mut active_state,
            "UnitFileState" => &mut unit_file_state,
            other => return Err(format!("unexpected property {other}")),
        };
        if slot.replace(value.to_string()).is_some() {
            return Err(format!("duplicate property {key}"));
        }
    }
    match (load_state, active_state, unit_file_state) {
        (Some(load_state), Some(active_state), Some(unit_file_state)) => Ok(UnitProperties {
            load_state,
            active_state,
            unit_file_state,
        }),
        _ => Err("missing property in systemctl show output".to_string()),
    }
}

impl UnitManager for Systemctl {
    fn show(&self) -> Result<UnitProperties, LifecycleError> {
        // Parsed from stdout regardless of exit status: systemd versions
        // disagree on the exit code for a not-found unit but all print the
        // requested properties.
        let output = self.run(&[
            "show",
            UNIT_NAME,
            "-p",
            "LoadState",
            "-p",
            "ActiveState",
            "-p",
            "UnitFileState",
        ])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_show_output(&stdout).map_err(|reason| LifecycleError::Systemctl {
            action: format!("show {UNIT_NAME}"),
            detail: format!(
                "{reason} ({}: {})",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }

    fn daemon_reload(&self) -> Result<(), LifecycleError> {
        self.run_expect_success(&["daemon-reload"])
    }

    fn stop(&self) -> Result<(), LifecycleError> {
        self.run_expect_success(&["stop", UNIT_NAME])
    }

    fn start(&self) -> Result<(), LifecycleError> {
        self.run_expect_success(&["start", UNIT_NAME])
    }
}

/// Production [`BusProbe`]: `NameHasOwner` on the system bus, the one bus
/// question that never triggers activation (the same probe backend
/// selection uses).
pub struct SystemBus;

impl BusProbe for SystemBus {
    fn daemon_name_has_owner(&self) -> Result<bool, LifecycleError> {
        let unreachable = |detail: String| LifecycleError::BusUnreachable { detail };
        zbus::block_on(async {
            let connection = zbus::Connection::system()
                .await
                .map_err(|e| unreachable(e.to_string()))?;
            let proxy = zbus::fdo::DBusProxy::new(&connection)
                .await
                .map_err(|e| unreachable(e.to_string()))?;
            let name: zbus::names::BusName<'_> = BUS_NAME
                .try_into()
                .map_err(|e: zbus::Error| unreachable(e.to_string()))?;
            proxy
                .name_has_owner(name)
                .await
                .map_err(|e| unreachable(e.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI32;

    const DELEGATING_ACTIVATION: &str = include_str!("../../../dbus/org.facelock.Daemon.service");

    struct MockInner {
        barrier: PathBuf,
        load_state: Mutex<String>,
        active_state: Mutex<String>,
        unit_file_state: String,
        stop_leaves_active: bool,
        reload_never_masks: bool,
        fail_start: bool,
        log: Mutex<Vec<String>>,
    }

    #[derive(Clone)]
    struct MockSystemd(Arc<MockInner>);

    impl MockSystemd {
        fn new(layout: &LifecycleLayout, active: bool) -> Self {
            Self::with(layout, active, |_| {})
        }

        fn with(
            layout: &LifecycleLayout,
            active: bool,
            adjust: impl FnOnce(&mut MockInner),
        ) -> Self {
            let mut inner = MockInner {
                barrier: layout.barrier_path(),
                load_state: Mutex::new("loaded".to_string()),
                active_state: Mutex::new(if active { "active" } else { "inactive" }.to_string()),
                unit_file_state: "enabled".to_string(),
                stop_leaves_active: false,
                reload_never_masks: false,
                fail_start: false,
                log: Mutex::new(Vec::new()),
            };
            adjust(&mut inner);
            Self(Arc::new(inner))
        }

        fn log(&self) -> Vec<String> {
            self.0.log.lock().expect("mock log").clone()
        }

        fn masked_by_crash(&self) {
            *self.0.load_state.lock().expect("mock state") = "masked".to_string();
        }
    }

    impl UnitManager for MockSystemd {
        fn show(&self) -> Result<UnitProperties, LifecycleError> {
            Ok(UnitProperties {
                load_state: self.0.load_state.lock().expect("mock state").clone(),
                active_state: self.0.active_state.lock().expect("mock state").clone(),
                unit_file_state: self.0.unit_file_state.clone(),
            })
        }

        fn daemon_reload(&self) -> Result<(), LifecycleError> {
            self.0
                .log
                .lock()
                .expect("mock log")
                .push("daemon-reload".to_string());
            if !self.0.reload_never_masks {
                let masked = self.0.barrier.symlink_metadata().is_ok();
                *self.0.load_state.lock().expect("mock state") =
                    if masked { "masked" } else { "loaded" }.to_string();
            }
            Ok(())
        }

        fn stop(&self) -> Result<(), LifecycleError> {
            self.0
                .log
                .lock()
                .expect("mock log")
                .push("stop".to_string());
            if !self.0.stop_leaves_active {
                *self.0.active_state.lock().expect("mock state") = "inactive".to_string();
            }
            Ok(())
        }

        fn start(&self) -> Result<(), LifecycleError> {
            self.0
                .log
                .lock()
                .expect("mock log")
                .push("start".to_string());
            if self.0.fail_start {
                return Err(LifecycleError::Systemctl {
                    action: "start".to_string(),
                    detail: "injected failure".to_string(),
                });
            }
            *self.0.active_state.lock().expect("mock state") = "active".to_string();
            Ok(())
        }
    }

    struct MockBus {
        owned: AtomicBool,
    }

    impl MockBus {
        fn released() -> Self {
            Self {
                owned: AtomicBool::new(false),
            }
        }

        fn owned() -> Self {
            Self {
                owned: AtomicBool::new(true),
            }
        }
    }

    impl BusProbe for MockBus {
        fn daemon_name_has_owner(&self) -> Result<bool, LifecycleError> {
            Ok(self.owned.load(Ordering::SeqCst))
        }
    }

    fn fast_tuning() -> AcquireTuning {
        AcquireTuning {
            stop_timeout: Duration::from_millis(50),
            bus_timeout: Duration::from_millis(50),
            poll_interval: Duration::from_millis(5),
            signal_ack_timeout: Duration::from_secs(10),
        }
    }

    /// Signal tests raise real signals, and every armed watcher in the
    /// process sees every raise. Serialize them so one test's raise cannot
    /// consume another watcher's single signal.
    fn signal_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn fixture() -> (tempfile::TempDir, LifecycleLayout) {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = LifecycleLayout::rooted_at(dir.path());
        let marker = layout.systemd_marker();
        fs::create_dir_all(&marker).expect("systemd marker");
        // A restrictive test umask must not fail the trusted-directory
        // checks the lease performs on directories it did not create.
        for ancestor in [marker.as_path(), dir.path()] {
            let mut current = Some(ancestor);
            while let Some(path) = current {
                if path.starts_with(dir.path()) {
                    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                        .expect("permissions");
                }
                current = path.parent();
            }
        }
        (dir, layout)
    }

    fn write_activation(layout: &LifecycleLayout, content: &str) -> PathBuf {
        let path = layout.at("usr/share/dbus-1/system-services/org.facelock.Daemon.service");
        let parent = path.parent().expect("parent");
        fs::create_dir_all(parent).expect("activation dir");
        fs::write(&path, content).expect("activation file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("permissions");
        path
    }

    fn acquire(
        layout: &LifecycleLayout,
        unit: &MockSystemd,
        bus: &MockBus,
    ) -> Result<LifecycleLease, LifecycleError> {
        LifecycleLease::acquire(
            layout.clone(),
            Box::new(unit.clone()),
            bus,
            fast_tuning(),
            false,
        )
    }

    fn mode_of(path: &Path) -> u32 {
        fs::symlink_metadata(path).expect("metadata").mode() & 0o7777
    }

    #[test]
    fn acquire_locks_masks_and_stops_an_active_daemon() {
        let (_dir, layout) = fixture();
        write_activation(&layout, DELEGATING_ACTIVATION);
        let unit = MockSystemd::new(&layout, true);
        let bus = MockBus::released();

        let lease = acquire(&layout, &unit, &bus).expect("acquire");
        assert!(lease.was_active());
        assert_eq!(lease.prior_unit_file_state(), "enabled");
        assert_eq!(mode_of(&layout.lock_path()), 0o600);
        assert_eq!(mode_of(&layout.barrier_path()), 0o600);
        assert_eq!(
            fs::symlink_metadata(layout.barrier_path())
                .expect("barrier")
                .len(),
            0
        );
        assert_eq!(unit.log(), ["daemon-reload", "stop"]);
    }

    #[test]
    fn drop_restores_an_active_daemon_and_removes_the_barrier() {
        let (_dir, layout) = fixture();
        write_activation(&layout, DELEGATING_ACTIVATION);
        let unit = MockSystemd::new(&layout, true);
        let bus = MockBus::released();

        drop(acquire(&layout, &unit, &bus).expect("acquire"));

        assert!(!layout.barrier_path().exists());
        assert_eq!(
            unit.log(),
            ["daemon-reload", "stop", "daemon-reload", "start"]
        );
        // The lock file is never unlinked, and the flock is released: a
        // fresh acquisition succeeds.
        assert!(layout.lock_path().exists());
        drop(acquire(&layout, &unit, &bus).expect("reacquire"));
    }

    #[test]
    fn drop_leaves_a_previously_inactive_daemon_stopped() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, false);
        let bus = MockBus::released();

        let lease = acquire(&layout, &unit, &bus).expect("acquire");
        assert!(!lease.was_active());
        drop(lease);

        assert!(!unit.log().contains(&"stop".to_string()));
        assert!(!unit.log().contains(&"start".to_string()));
    }

    #[test]
    fn a_second_acquisition_fails_while_the_lease_is_held() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, false);
        let bus = MockBus::released();

        let _lease = acquire(&layout, &unit, &bus).expect("acquire");
        let second = MockSystemd::new(&layout, false);
        match acquire(&layout, &second, &bus) {
            Err(LifecycleError::LockHeld(path)) => assert_eq!(path, layout.lock_path()),
            other => panic!("expected LockHeld, got {other:?}"),
        }
        // The contender must not have touched the daemon.
        assert!(second.log().is_empty());
    }

    #[test]
    fn acquisition_fails_without_a_booted_systemd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = LifecycleLayout::rooted_at(dir.path());
        let unit = MockSystemd::new(&layout, false);
        match acquire(&layout, &unit, &MockBus::released()) {
            Err(LifecycleError::NoSystemd(_)) => {}
            other => panic!("expected NoSystemd, got {other:?}"),
        }
        assert!(unit.log().is_empty());
    }

    #[test]
    fn a_non_delegating_activation_definition_fails_closed() {
        let (_dir, layout) = fixture();
        write_activation(
            &layout,
            "[D-BUS Service]\nName=org.facelock.Daemon\nExec=/usr/bin/facelock daemon\nUser=root\n",
        );
        let unit = MockSystemd::new(&layout, true);
        match acquire(&layout, &unit, &MockBus::released()) {
            Err(LifecycleError::ActivationNotDelegated { reason, .. }) => {
                assert!(reason.contains("SystemdService"), "reason: {reason}");
            }
            other => panic!("expected ActivationNotDelegated, got {other:?}"),
        }
        // Fail-closed means fail before mutation: no barrier, daemon untouched.
        assert!(!layout.barrier_path().exists());
        assert!(unit.log().is_empty());
        // And the lock was released for the next attempt.
        write_activation(&layout, DELEGATING_ACTIVATION);
        drop(acquire(&layout, &unit, &MockBus::released()).expect("reacquire"));
    }

    #[test]
    fn the_shipped_activation_definition_delegates() {
        assert_eq!(activation_delegates(DELEGATING_ACTIVATION), Ok(()));
    }

    #[test]
    fn activation_grammar_failures_are_named() {
        for (content, needle) in [
            (
                "[D-BUS Service]\nName=other.Name\nSystemdService=facelock-daemon.service\n",
                "Name is",
            ),
            (
                "[D-BUS Service]\nName=org.facelock.Daemon\nSystemdService=other.service\n",
                "SystemdService is",
            ),
            ("Name=org.facelock.Daemon\n", "outside"),
            (
                "[D-BUS Service]\n[Other]\nName=org.facelock.Daemon\n",
                "unexpected section",
            ),
            (
                "[D-BUS Service]\nName=org.facelock.Daemon\nName=org.facelock.Daemon\n",
                "duplicate Name",
            ),
            (
                "[D-BUS Service]\nName=org.facelock.Daemon\nUnknownKey=x\n",
                "unrecognized key",
            ),
            (
                "[D-BUS Service]\nName=org.facelock.Daemon\nnot a key value line\n",
                "unparseable",
            ),
        ] {
            let result = activation_delegates(content);
            let reason = result.expect_err(content);
            assert!(reason.contains(needle), "{content:?} -> {reason}");
        }
    }

    #[test]
    fn a_symlinked_lock_path_fails_closed() {
        let (_dir, layout) = fixture();
        fs::create_dir_all(layout.lock_dir()).expect("lock dir");
        fs::set_permissions(layout.lock_dir(), fs::Permissions::from_mode(0o755))
            .expect("permissions");
        std::os::unix::fs::symlink("/dev/null", layout.lock_path()).expect("symlink");
        let unit = MockSystemd::new(&layout, false);
        match acquire(&layout, &unit, &MockBus::released()) {
            Err(LifecycleError::Untrusted { reason, .. }) => {
                assert!(reason.contains("symlink"), "reason: {reason}");
            }
            other => panic!("expected Untrusted, got {other:?}"),
        }
    }

    #[test]
    fn a_stale_barrier_from_a_killed_process_is_adopted_and_cleared() {
        let (_dir, layout) = fixture();
        // Simulate a SIGKILL mid-purge: the dead process left the barrier
        // on disk (flock died with it), the manager still shows masked.
        fs::create_dir_all(layout.control_dir()).expect("control dir");
        fs::set_permissions(layout.control_dir(), fs::Permissions::from_mode(0o755))
            .expect("permissions");
        fs::write(layout.barrier_path(), b"").expect("stale barrier");
        fs::set_permissions(layout.barrier_path(), fs::Permissions::from_mode(0o600))
            .expect("permissions");
        let unit = MockSystemd::new(&layout, false);
        unit.masked_by_crash();

        let lease = acquire(&layout, &unit, &MockBus::released()).expect("recovery acquire");
        drop(lease);

        // Recovery: the stale barrier is gone and the unit is loadable again.
        assert!(!layout.barrier_path().exists());
        assert_eq!(unit.show().expect("show").load_state, "loaded");
    }

    #[test]
    fn a_masked_unit_without_a_stale_barrier_fails_closed() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, false);
        unit.masked_by_crash();
        match acquire(&layout, &unit, &MockBus::released()) {
            Err(LifecycleError::UnitStateNotAdmissible { state, .. }) => {
                assert_eq!(state, "masked");
            }
            other => panic!("expected UnitStateNotAdmissible, got {other:?}"),
        }
        assert!(!layout.barrier_path().exists());
    }

    #[test]
    fn a_foreign_control_file_fails_closed() {
        let (_dir, layout) = fixture();
        fs::create_dir_all(layout.control_dir()).expect("control dir");
        fs::set_permissions(layout.control_dir(), fs::Permissions::from_mode(0o755))
            .expect("permissions");
        fs::write(layout.barrier_path(), b"[Unit]\n").expect("foreign file");
        let unit = MockSystemd::new(&layout, false);
        match acquire(&layout, &unit, &MockBus::released()) {
            Err(LifecycleError::ControlPathConflict { .. }) => {}
            other => panic!("expected ControlPathConflict, got {other:?}"),
        }
        // The foreign object is preserved, and the daemon was not touched.
        assert!(layout.barrier_path().exists());
        assert!(unit.log().is_empty());
    }

    #[test]
    fn an_ineffective_barrier_rolls_back() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::with(&layout, true, |inner| inner.reload_never_masks = true);
        match acquire(&layout, &unit, &MockBus::released()) {
            Err(LifecycleError::BarrierNotEffective { load_state, .. }) => {
                assert_eq!(load_state, "loaded");
            }
            other => panic!("expected BarrierNotEffective, got {other:?}"),
        }
        // Rollback removed the barrier and never stopped the daemon.
        assert!(!layout.barrier_path().exists());
        assert!(!unit.log().contains(&"stop".to_string()));
        assert_eq!(unit.show().expect("show").active_state, "active");
    }

    #[test]
    fn a_daemon_that_will_not_stop_fails_closed_and_rolls_back() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::with(&layout, true, |inner| inner.stop_leaves_active = true);
        match acquire(&layout, &unit, &MockBus::released()) {
            Err(LifecycleError::DaemonNotStopped { state, .. }) => assert_eq!(state, "active"),
            other => panic!("expected DaemonNotStopped, got {other:?}"),
        }
        // Rollback removed the barrier and restarted what it had stopped.
        assert!(!layout.barrier_path().exists());
        assert!(unit.log().contains(&"start".to_string()));
    }

    #[test]
    fn a_still_owned_bus_name_fails_closed_and_rolls_back() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, true);
        match acquire(&layout, &unit, &MockBus::owned()) {
            Err(LifecycleError::NameStillOwned { .. }) => {}
            other => panic!("expected NameStillOwned, got {other:?}"),
        }
        assert!(!layout.barrier_path().exists());
        assert!(unit.log().contains(&"start".to_string()));
    }

    #[test]
    fn a_panic_unwind_still_restores() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, true);
        let bus = MockBus::released();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = acquire(&layout, &unit, &bus).expect("acquire");
            panic!("mid-operation failure");
        }));
        assert!(result.is_err());
        assert!(!layout.barrier_path().exists());
        assert!(unit.log().contains(&"start".to_string()));
    }

    #[test]
    fn release_reports_a_failed_restart() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::with(&layout, true, |inner| inner.fail_start = true);
        let lease = acquire(&layout, &unit, &MockBus::released()).expect("acquire");
        match lease.release() {
            Err(LifecycleError::RestoreIncomplete(detail)) => {
                assert!(detail.contains("restart"), "detail: {detail}");
            }
            other => panic!("expected RestoreIncomplete, got {other:?}"),
        }
        // The barrier still came down even though the restart failed.
        assert!(!layout.barrier_path().exists());
    }

    #[test]
    fn release_leaving_activation_barred_keeps_the_barrier() {
        let (_dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, true);
        let lease = acquire(&layout, &unit, &MockBus::released()).expect("acquire");
        let barred_at = lease
            .release_leaving_activation_barred()
            .expect("barred release");
        assert_eq!(barred_at, layout.barrier_path());
        assert!(layout.barrier_path().exists());
        assert!(!unit.log().contains(&"start".to_string()));
        // The next lifecycle operation adopts the deliberate barrier the
        // same way it adopts a crashed one, and restores from it.
        let next = MockSystemd::new(&layout, false);
        next.masked_by_crash();
        drop(acquire(&layout, &next, &MockBus::released()).expect("adopting acquire"));
        assert!(!layout.barrier_path().exists());
    }

    #[test]
    fn a_signal_restores_and_then_terminates() {
        let _serial = signal_test_guard();
        let (_dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, true);
        let bus = MockBus::released();
        let mut lease = acquire(&layout, &unit, &bus).expect("acquire");
        let interrupt = lease.interrupt_flag();
        let delivered = Arc::new(AtomicI32::new(0));
        let recorded = delivered.clone();
        lease
            .arm_signals_with(Box::new(move |signal| {
                recorded.store(signal, Ordering::SeqCst);
            }))
            .expect("arm signals");

        // No operation is in flight, and the caller has said so.
        lease.mark_operation_finished();
        signal_hook::low_level::raise(signal_hook::consts::SIGHUP).expect("raise");
        let deadline = Instant::now() + Duration::from_secs(5);
        while delivered.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "signal restore never ran");
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(
            delivered.load(Ordering::SeqCst),
            signal_hook::consts::SIGHUP
        );
        assert!(interrupt.load(Ordering::SeqCst));
        assert!(!layout.barrier_path().exists());
        assert!(unit.log().contains(&"start".to_string()));

        // The normal drop path is now a no-op: restoration is non-reentrant.
        drop(lease);
        let starts = unit.log().iter().filter(|entry| *entry == "start").count();
        assert_eq!(starts, 1);
    }

    #[test]
    fn a_signal_waits_for_the_operation_to_acknowledge() {
        let _serial = signal_test_guard();
        let (_dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, true);
        let bus = MockBus::released();
        let mut lease = acquire(&layout, &unit, &bus).expect("acquire");
        let interrupt = lease.interrupt_flag();
        let delivered = Arc::new(AtomicI32::new(0));
        let recorded = delivered.clone();
        lease
            .arm_signals_with(Box::new(move |signal| {
                recorded.store(signal, Ordering::SeqCst);
            }))
            .expect("arm signals");

        signal_hook::low_level::raise(signal_hook::consts::SIGHUP).expect("raise");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !interrupt.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "interrupt flag never raised");
            std::thread::sleep(Duration::from_millis(5));
        }

        // The flag is up but nothing has acknowledged: restoration must not
        // have started. Generous delay, asserting absence.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!unit.log().contains(&"start".to_string()));
        assert!(layout.barrier_path().exists());
        assert_eq!(delivered.load(Ordering::SeqCst), 0);

        // The acknowledgement releases the restore, then the terminate.
        lease.mark_operation_finished();
        let deadline = Instant::now() + Duration::from_secs(5);
        while delivered.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "signal restore never ran");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!layout.barrier_path().exists());
        assert!(unit.log().contains(&"start".to_string()));
    }

    #[test]
    fn a_non_polling_operation_never_races_a_restarted_daemon() {
        // The defect this pins down: restoring on a signal without waiting
        // for the operation once restarted the daemon while a non-polling
        // operation kept mutating the filesystem (299 of 300 steps ran
        // after the daemon was back). With the acknowledgement gate the
        // wait expires and activation stays barred, so the count is zero.
        let _serial = signal_test_guard();
        let (dir, layout) = fixture();
        let unit = MockSystemd::new(&layout, true);
        let bus = MockBus::released();
        let mut tuning = fast_tuning();
        tuning.signal_ack_timeout = Duration::from_millis(30);
        let mut lease =
            LifecycleLease::acquire(layout.clone(), Box::new(unit.clone()), &bus, tuning, false)
                .expect("acquire");
        let delivered = Arc::new(AtomicI32::new(0));
        let recorded = delivered.clone();
        lease
            .arm_signals_with(Box::new(move |signal| {
                recorded.store(signal, Ordering::SeqCst);
            }))
            .expect("arm signals");

        // A runaway operation: ignores the interrupt flag, never
        // acknowledges, touches the filesystem at every step.
        let op_unit = unit.clone();
        let scratch = dir.path().join("op-scratch");
        fs::create_dir_all(&scratch).expect("scratch");
        let operation = std::thread::spawn(move || {
            let mut steps_after_daemon_start = 0u32;
            for step in 0..300u32 {
                if op_unit.log().contains(&"start".to_string()) {
                    steps_after_daemon_start += 1;
                }
                let probe = scratch.join(format!("step-{step}"));
                fs::write(&probe, b"").expect("step write");
                fs::remove_file(&probe).expect("step unlink");
                std::thread::sleep(Duration::from_millis(1));
            }
            steps_after_daemon_start
        });

        signal_hook::low_level::raise(signal_hook::consts::SIGHUP).expect("raise");
        let races = operation.join().expect("operation thread");
        let deadline = Instant::now() + Duration::from_secs(5);
        while delivered.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "signal path never terminated");
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(races, 0);
        assert!(!unit.log().contains(&"start".to_string()));
        // Activation stayed barred: the SIGKILL-equivalent recoverable state.
        assert!(layout.barrier_path().exists());
        assert!(lease.interrupt_flag().load(Ordering::SeqCst));
        drop(lease);

        // Recovery: the next acquisition adopts the retained barrier.
        let next = MockSystemd::new(&layout, false);
        next.masked_by_crash();
        drop(acquire(&layout, &next, &MockBus::released()).expect("adopting acquire"));
        assert!(!layout.barrier_path().exists());
    }

    #[test]
    fn show_output_parsing_is_strict() {
        let ok = parse_show_output("LoadState=loaded\nActiveState=active\nUnitFileState=enabled\n")
            .expect("parse");
        assert_eq!(ok.load_state, "loaded");
        assert_eq!(ok.active_state, "active");
        assert_eq!(ok.unit_file_state, "enabled");
        // An empty value is a value: a not-found unit prints UnitFileState=.
        let not_found =
            parse_show_output("LoadState=not-found\nActiveState=inactive\nUnitFileState=\n")
                .expect("parse");
        assert_eq!(not_found.unit_file_state, "");
        for (text, needle) in [
            ("LoadState=loaded\nActiveState=active\n", "missing"),
            (
                "LoadState=loaded\nLoadState=masked\nActiveState=active\nUnitFileState=\n",
                "duplicate",
            ),
            ("garbage\n", "unparseable"),
            (
                "LoadState=loaded\nActiveState=active\nUnitFileState=\nOther=x\n",
                "unexpected",
            ),
        ] {
            let reason = parse_show_output(text).expect_err(text);
            assert!(reason.contains(needle), "{text:?} -> {reason}");
        }
    }
}
