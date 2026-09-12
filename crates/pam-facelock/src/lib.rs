// PAM module for facelock face authentication.
//
// Dependency rule: libc, toml, serde, zbus ONLY. No facelock-core.
// This is security-critical code: never panic across FFI, always fail gracefully,
// all auth attempts logged to syslog.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString};
use std::panic;
use std::path::{Path, PathBuf};

use serde::Deserialize;

// The oneshot exit-code -> PAM mapping, in its own dependency-free file so
// `facelock-cli`'s test suite can `include!` the same source and pin it
// against the binary's emission table (see the file header).
mod oneshot_exit;

// The lid resolver behind `abort_if_lid_closed`, dependency-free so
// `facelock-daemon` can `include!` the same source (see the file header).
mod lid;

// ---------------------------------------------------------------------------
// PAM constants
// ---------------------------------------------------------------------------

// Aliased from `oneshot_exit` rather than redeclared: the values live once,
// and a target where `libc::c_int` is not `i32` would fail to compile here
// instead of silently forking the two files.
const PAM_SUCCESS: libc::c_int = oneshot_exit::PAM_SUCCESS;
const PAM_AUTH_ERR: libc::c_int = oneshot_exit::PAM_AUTH_ERR;
const PAM_AUTHINFO_UNAVAIL: libc::c_int = oneshot_exit::PAM_AUTHINFO_UNAVAIL;
const PAM_IGNORE: libc::c_int = oneshot_exit::PAM_IGNORE;

// PAM conversation message styles
const PAM_TEXT_INFO: libc::c_int = 4;

// PAM item types for conversation
const PAM_CONV: libc::c_int = 5;

// Syslog constants
const LOG_AUTH: libc::c_int = 4 << 3; // LOG_AUTH facility
const LOG_INFO: libc::c_int = 6;
const LOG_WARNING: libc::c_int = 4;

// Default config path
const DEFAULT_CONFIG_PATH: &str = "/etc/facelock/config.toml";
const DEFAULT_TIMEOUT_SECS: u32 = 5;

/// The binary spawned for oneshot authentication. Hardcoded: a configurable
/// path would let anyone who can influence the config select which binary a
/// root PAM context executes (E7/N9). It and its ancestors are still checked
/// for root ownership and non-writability before spawning
/// ([`verify_binary_trust`]) — the constant says which binary, the check says
/// whether that binary is still trustworthy.
const AUTH_BIN: &str = "/usr/bin/facelock";

// D-Bus constants
const DBUS_BUS_NAME: &str = "org.facelock.Daemon";
const DBUS_OBJECT_PATH: &str = "/org/facelock/Daemon";
const DBUS_INTERFACE_NAME: &str = "org.facelock.Daemon";

// ---------------------------------------------------------------------------
// Configuration (minimal subset, inline)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PamConfig {
    #[serde(default)]
    daemon: PamDaemonConfig,
    #[serde(default)]
    security: PamSecurityConfig,
    #[serde(default)]
    recognition: PamRecognitionConfig,
    #[serde(default)]
    notification: PamNotificationConfig,
}

#[derive(Deserialize)]
struct PamDaemonConfig {
    /// "daemon" (default) or "oneshot"
    #[serde(default = "default_mode")]
    mode: String,
    // The oneshot binary path is NOT configurable — see `AUTH_BIN`. A leftover
    // `auth_bin` key in an existing config is silently ignored (serde default).
}

impl Default for PamDaemonConfig {
    fn default() -> Self {
        Self {
            mode: default_mode(),
        }
    }
}

#[derive(Deserialize)]
struct PamSecurityConfig {
    #[serde(default)]
    disabled: bool,
    #[serde(default = "default_true")]
    abort_if_ssh: bool,
    #[serde(default = "default_true")]
    abort_if_lid_closed: bool,
    /// Parsed from config but used by the daemon, not the PAM module directly.
    /// Included so the PAM config parser accepts the field without error.
    #[serde(default)]
    #[allow(dead_code)]
    suppress_unknown: bool,
    #[serde(default)]
    pam_policy: Option<PamPolicyConfig>,
}

impl Default for PamSecurityConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            abort_if_ssh: true,
            abort_if_lid_closed: true,
            suppress_unknown: false,
            pam_policy: None,
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // Variants used via deserialization
enum PamNotificationMode {
    Off,
    /// The default mode, named once in [`PamNotificationConfig::default`] and
    /// nowhere else. It must equal facelock-core's `NotificationMode`
    /// default.
    ///
    /// This enum deliberately has no `Default` impl. It carried a second
    /// answer to the same question and the two drifted (`Both` here,
    /// `Terminal` there), undetected because `terminal()` treats Both and
    /// Terminal identically. Deleting it is not just tidying: with no
    /// `Default`, re-adding `#[serde(default)]` to the `mode` field does not
    /// compile, so the drift cannot come back the way it arrived. The one
    /// remaining route — `#[serde(default = "some_fn")]`, which needs no
    /// `Default` — is caught by
    /// `notification_section_present_with_keys_omitted_equals_absent`.
    ///
    /// facelock-core's mirror of this enum KEEPS its `Default`, deliberately:
    /// two sibling enums there (`SnapshotMode`, `EncryptionMethod`) still
    /// have live field-level defaults, so removing only this one would be a
    /// locally-invisible asymmetry. The two crates must agree on the default
    /// *value*, not on how each spells it. Do not "restore" this impl for
    /// symmetry with core.
    Terminal,
    Desktop,
    Both,
}

/// Private mirror of the `[notification]` table in facelock-core's
/// `NotificationConfig` — the canonical schema. The dependency ceiling
/// (libc/toml/serde/zbus only) forbids sharing the types; keep field names
/// and defaults in lockstep by hand. The flags gate the same event
/// vocabulary as `facelock_core::notify::NotifyEvent`: `notify_prompt` ↔
/// Scanning, `notify_on_success` ↔ Success. (This module renders them as
/// PAM conversation text; desktop delivery is the daemon's job.)
///
/// `#[serde(default)]` sits on the container, not on the fields: every
/// omitted key falls back to the [`Default`] impl below, so "section present,
/// key omitted" and "section absent" produce the same values by construction
/// instead of by two lists happening to agree. They did not agree once (D9),
/// and the same shape in facelock-core's `NotificationConfig` shipped the
/// same class of bug.
#[derive(Deserialize)]
#[serde(default)]
struct PamNotificationConfig {
    mode: PamNotificationMode,
    notify_prompt: bool,
    notify_on_success: bool,
}

impl Default for PamNotificationConfig {
    fn default() -> Self {
        Self {
            mode: PamNotificationMode::Terminal,
            notify_prompt: true,
            notify_on_success: true,
        }
    }
}

impl PamNotificationConfig {
    fn terminal(&self) -> bool {
        matches!(
            self.mode,
            PamNotificationMode::Terminal | PamNotificationMode::Both
        )
    }
}

#[derive(Deserialize)]
struct PamPolicyConfig {
    #[serde(default)]
    allowed_services: Vec<String>,
    #[serde(default)]
    denied_services: Vec<String>,
}

#[derive(Deserialize)]
struct PamRecognitionConfig {
    #[serde(default = "default_timeout")]
    timeout_secs: u32,
}

impl Default for PamRecognitionConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_timeout(),
        }
    }
}

fn default_timeout() -> u32 {
    DEFAULT_TIMEOUT_SECS
}

fn default_mode() -> String {
    "daemon".to_string()
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Gettext (i18n) support
// ---------------------------------------------------------------------------

/// Text domain for PAM module translations.
const GETTEXT_DOMAIN: &[u8] = b"pam_facelock\0";

unsafe extern "C" {
    safe fn dgettext(
        domainname: *const libc::c_char,
        msgid: *const libc::c_char,
    ) -> *const libc::c_char;
}

/// Translate a message using gettext. Falls back to the original English string
/// if no translation is available or if gettext fails.
fn gettext(msgid: &str) -> String {
    let Ok(c_msgid) = CString::new(msgid) else {
        return msgid.to_string();
    };

    let result = dgettext(GETTEXT_DOMAIN.as_ptr().cast(), c_msgid.as_ptr());
    if result.is_null() {
        return msgid.to_string();
    }
    unsafe { CStr::from_ptr(result) }
        .to_str()
        .unwrap_or(msgid)
        .to_string()
}

// ---------------------------------------------------------------------------
// Syslog logging
// ---------------------------------------------------------------------------

/// Log an authentication event to syslog (LOG_AUTH facility).
///
/// Format: pam_facelock(<service>): <result> for user <username>
fn log_auth(service: &str, result: &str, user: &str, level: libc::c_int) {
    // Use a static ident string to avoid dangling pointer issues.
    // openlog requires the ident pointer to remain valid until closelog.
    let ident = b"pam_facelock\0";
    let msg = match CString::new(format!(
        "pam_facelock({}): {} for user {}",
        service, result, user
    )) {
        Ok(s) => s,
        Err(_) => return, // Can't log if message contains NUL bytes
    };

    unsafe {
        libc::openlog(ident.as_ptr().cast(), 0, LOG_AUTH);
        // syslog with %s format to avoid format string injection
        let fmt = b"%s\0";
        libc::syslog(level, fmt.as_ptr().cast(), msg.as_ptr());
        libc::closelog();
    }
}

// ---------------------------------------------------------------------------
// PAM FFI helpers
// ---------------------------------------------------------------------------

/// Get a PAM item string (service name, user, etc.)
unsafe fn pam_get_item_str(pamh: *mut libc::c_void, item_type: libc::c_int) -> Option<String> {
    unsafe extern "C" {
        safe fn pam_get_item(
            pamh: *mut libc::c_void,
            item_type: libc::c_int,
            item: *mut *const libc::c_void,
        ) -> libc::c_int;
    }

    let mut item: *const libc::c_void = std::ptr::null();
    let ret = pam_get_item(pamh, item_type, &mut item);
    if ret != PAM_SUCCESS || item.is_null() {
        return None;
    }
    // Safety: pam_get_item returns a valid C string pointer when ret == PAM_SUCCESS
    let cstr = unsafe { CStr::from_ptr(item.cast()) };
    cstr.to_str().ok().map(|s| s.to_string())
}

/// Get the PAM service name (e.g. "sudo", "login")
unsafe fn pam_get_service(pamh: *mut libc::c_void) -> Option<String> {
    const PAM_SERVICE: libc::c_int = 1;
    unsafe { pam_get_item_str(pamh, PAM_SERVICE) }
}

/// Get the PAM username
unsafe fn pam_get_user(pamh: *mut libc::c_void) -> Option<String> {
    unsafe extern "C" {
        safe fn pam_get_user(
            pamh: *mut libc::c_void,
            user: *mut *const libc::c_char,
            prompt: *const libc::c_char,
        ) -> libc::c_int;
    }

    let mut user_ptr: *const libc::c_char = std::ptr::null();
    let ret = pam_get_user(pamh, &mut user_ptr, std::ptr::null());
    if ret != PAM_SUCCESS || user_ptr.is_null() {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(user_ptr) };
    cstr.to_str().ok().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// PAM conversation (user feedback)
// ---------------------------------------------------------------------------

/// PAM message structure for the conversation function.
#[repr(C)]
struct PamMessage {
    msg_style: libc::c_int,
    msg: *const libc::c_char,
}

/// PAM response structure returned by the conversation function.
#[repr(C)]
struct PamResponse {
    resp: *mut libc::c_char,
    resp_retcode: libc::c_int,
}

/// PAM conversation structure obtained via pam_get_item(PAM_CONV).
#[repr(C)]
struct PamConv {
    conv: Option<
        unsafe extern "C" fn(
            num_msg: libc::c_int,
            msg: *mut *const PamMessage,
            resp: *mut *mut PamResponse,
            appdata_ptr: *mut libc::c_void,
        ) -> libc::c_int,
    >,
    appdata_ptr: *mut libc::c_void,
}

/// Send an informational text message to the user via PAM conversation.
/// Fire-and-forget: errors are silently ignored.
unsafe fn pam_info(pamh: *mut libc::c_void, message: &str) {
    let msg_cstr = match CString::new(message) {
        Ok(s) => s,
        Err(_) => return,
    };

    // Get the conversation function
    let mut conv_ptr: *const libc::c_void = std::ptr::null();
    {
        unsafe extern "C" {
            safe fn pam_get_item(
                pamh: *mut libc::c_void,
                item_type: libc::c_int,
                item: *mut *const libc::c_void,
            ) -> libc::c_int;
        }
        let ret = pam_get_item(pamh, PAM_CONV, &mut conv_ptr);
        if ret != PAM_SUCCESS || conv_ptr.is_null() {
            return;
        }
    }

    let conv = unsafe { &*(conv_ptr as *const PamConv) };
    let conv_fn = match conv.conv {
        Some(f) => f,
        None => return,
    };

    let pam_msg = PamMessage {
        msg_style: PAM_TEXT_INFO,
        msg: msg_cstr.as_ptr(),
    };
    let msg_ptr: *const PamMessage = &pam_msg;
    let mut resp_ptr: *mut PamResponse = std::ptr::null_mut();

    unsafe {
        let _ = conv_fn(
            1,
            &msg_ptr as *const _ as *mut _,
            &mut resp_ptr,
            conv.appdata_ptr,
        );
    }

    // Free response if conversation allocated one (unlikely for TEXT_INFO)
    if !resp_ptr.is_null() {
        unsafe {
            let resp = &*resp_ptr;
            if !resp.resp.is_null() {
                libc::free(resp.resp.cast());
            }
            libc::free(resp_ptr.cast());
        }
    }
}

// ---------------------------------------------------------------------------
// Environment / hardware checks
// ---------------------------------------------------------------------------

/// Check if we're in an SSH session by reading /proc/self/environ
fn is_ssh_session() -> bool {
    std::fs::read("/proc/self/environ")
        .map(|data| {
            data.split(|&b| b == 0)
                .any(|var| var.starts_with(b"SSH_CONNECTION=") || var.starts_with(b"SSH_TTY="))
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// D-Bus daemon authentication
// ---------------------------------------------------------------------------

/// Parsed auth response from daemon.
enum AuthResponse {
    /// Face matched with given similarity score
    Matched {
        #[allow(dead_code)]
        similarity: f32,
    },
    /// Face not matched, and the daemon gave no face-seen signal — either it
    /// saw nobody, or it predates the `-4` sentinel.
    NoMatch { similarity: f32 },
    /// Face not matched, and the daemon explicitly reports that its detector
    /// did see a face (`model_id == -4`).
    NoMatchFaceSeen,
    /// No enrolled models and suppress_unknown is enabled.
    /// PAM should return PAM_AUTHINFO_UNAVAIL to let the stack try the next module.
    Suppressed,
    /// Daemon returned an error
    Error { message: String },
}

/// Structured timeout classification for zbus errors.
///
/// Matches the concrete variants zbus uses for method-call timeouts
/// (`InputOutput` with `ErrorKind::TimedOut` from `method_timeout`) and the
/// D-Bus daemon's own reply-timeout errors, instead of fragile substring
/// matching on the display string.
fn is_timeout_zbus_error(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::InputOutput(io_err) => io_err.kind() == std::io::ErrorKind::TimedOut,
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.NoReply"
                | "org.freedesktop.DBus.Error.Timeout"
                | "org.freedesktop.DBus.Error.TimedOut"
        ),
        zbus::Error::FDO(fdo_err) => is_timeout_fdo_error(fdo_err),
        _ => false,
    }
}

/// Timeout classification for `zbus::fdo::Error` (returned by the
/// `org.freedesktop.DBus` proxy methods used for peer verification).
fn is_timeout_fdo_error(err: &zbus::fdo::Error) -> bool {
    matches!(
        err,
        zbus::fdo::Error::NoReply(_) | zbus::fdo::Error::Timeout(_) | zbus::fdo::Error::TimedOut(_)
    )
}

fn classify_zbus_error(prefix: &str, err: &zbus::Error) -> String {
    if is_timeout_zbus_error(err) {
        format!("dbus_timeout: {err}")
    } else {
        format!("{prefix}: {err}")
    }
}

fn classify_fdo_error(prefix: &str, err: &zbus::fdo::Error) -> String {
    if is_timeout_fdo_error(err) {
        format!("dbus_timeout: {err}")
    } else {
        format!("{prefix}: {err}")
    }
}

/// Map a raw D-Bus `Authenticate` reply onto an [`AuthResponse`].
///
/// Sentinel `model_id` values (with `matched == false`):
/// - `-2`: recoverable daemon error, `label` carries the error message
///   (rate limited, IR required, camera/storage failure).
/// - `-3`: suppressed (no enrolled models + `suppress_unknown`).
/// - `-4`: no match, and the daemon's detector did see a face.
fn parse_auth_reply(matched: bool, model_id: i32, label: String, similarity: f64) -> AuthResponse {
    if matched {
        return AuthResponse::Matched {
            similarity: similarity as f32,
        };
    }
    match model_id {
        -2 => AuthResponse::Error { message: label },
        -3 => AuthResponse::Suppressed,
        -4 => AuthResponse::NoMatchFaceSeen,
        _ => AuthResponse::NoMatch {
            similarity: similarity as f32,
        },
    }
}

/// Verify that the current owner of `org.facelock.Daemon` is running as root
/// (UID 0) and return its unique bus name.
///
/// Trust boundary: the D-Bus policy file is supposed to prevent unprivileged
/// processes from owning the daemon name, but the PAM module must not have a
/// single point of failure on that file. The caller pins the `Authenticate`
/// call to the returned unique name, so the owner cannot change between the
/// UID check and the method call.
fn verify_daemon_peer(
    connection: &zbus::blocking::Connection,
) -> Result<zbus::names::OwnedUniqueName, String> {
    let dbus_proxy = zbus::blocking::fdo::DBusProxy::new(connection)
        .map_err(|e| classify_zbus_error("dbus_proxy_failed", &e))?;

    let bus_name = zbus::names::BusName::try_from(DBUS_BUS_NAME)
        .map_err(|e| format!("dbus_proxy_failed: invalid bus name: {e}"))?;

    // Resolve the owner, triggering D-Bus activation first if the daemon is
    // not running yet (GetNameOwner alone does not activate services).
    let owner = match dbus_proxy.get_name_owner(bus_name.clone()) {
        Ok(owner) => owner,
        Err(_) => {
            let well_known = zbus::names::WellKnownName::try_from(DBUS_BUS_NAME)
                .map_err(|e| format!("dbus_proxy_failed: invalid bus name: {e}"))?;
            dbus_proxy
                .start_service_by_name(well_known, 0)
                .map_err(|e| classify_fdo_error("dbus_connect_failed", &e))?;
            dbus_proxy
                .get_name_owner(bus_name)
                .map_err(|e| classify_fdo_error("dbus_connect_failed", &e))?
        }
    };

    let owner_uid = dbus_proxy
        .get_connection_unix_user(zbus::names::BusName::from(owner.inner().clone()))
        .map_err(|e| classify_fdo_error("dbus_call_failed", &e))?;

    if owner_uid != 0 {
        // Refuse to trust the reply of a non-root peer. The caller falls
        // through (oneshot fallback / password), never PAM_SUCCESS.
        return Err(format!(
            "dbus_untrusted_peer: {DBUS_BUS_NAME} owned by uid {owner_uid} (require root)"
        ));
    }

    Ok(owner)
}

/// Authenticate via D-Bus system bus to the facelock daemon.
///
/// Runs the blocking D-Bus work on a worker thread with an overall deadline,
/// so that even connection establishment against a hung bus cannot stall the
/// PAM stack (zbus `method_timeout` only bounds method calls, not connect).
fn daemon_authenticate(config: &PamConfig, user: &str) -> Result<AuthResponse, String> {
    // Method timeout = recognition timeout + buffer for camera open/warmup/model load
    let method_timeout_secs = config.recognition.timeout_secs as u64 + 5;
    // Overall deadline adds a further buffer for connection establishment,
    // D-Bus activation, and peer verification round-trips.
    let overall_timeout = std::time::Duration::from_secs(method_timeout_secs + 5);

    let (tx, rx) = std::sync::mpsc::channel();
    let user_owned = user.to_string();
    std::thread::Builder::new()
        .name("pam-facelock-dbus".into())
        .spawn(move || {
            let _ = tx.send(daemon_authenticate_blocking(
                method_timeout_secs,
                &user_owned,
            ));
        })
        .map_err(|e| format!("dbus_call_failed: worker thread spawn failed: {e}"))?;

    match rx.recv_timeout(overall_timeout) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // The worker is stuck (most likely in connection establishment).
            // Abandon it; never block the PAM stack.
            Err("dbus_timeout: connection or method call exceeded overall deadline".into())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("dbus_call_failed: worker thread terminated unexpectedly".into())
        }
    }
}

fn daemon_authenticate_blocking(
    method_timeout_secs: u64,
    user: &str,
) -> Result<AuthResponse, String> {
    let connection = zbus::blocking::connection::Builder::system()
        .map_err(|e| format!("dbus_connect_failed: {e}"))?
        .method_timeout(std::time::Duration::from_secs(method_timeout_secs))
        .build()
        .map_err(|e| format!("dbus_connect_failed: {e}"))?;

    // Verify the daemon peer is root and pin the call to its unique name.
    let daemon_peer = verify_daemon_peer(&connection)?;

    let proxy = zbus::blocking::Proxy::new(
        &connection,
        zbus::names::BusName::from(daemon_peer.inner().clone()),
        DBUS_OBJECT_PATH,
        DBUS_INTERFACE_NAME,
    )
    .map_err(|e| classify_zbus_error("dbus_proxy_failed", &e))?;

    // D-Bus method returns (matched: bool, model_id: i32, label: String, similarity: f64)
    let reply: (bool, i32, String, f64) = proxy
        .call("Authenticate", &(user,))
        .map_err(|e: zbus::Error| classify_zbus_error("dbus_call_failed", &e))?;

    let (matched, model_id, label, similarity) = reply;
    Ok(parse_auth_reply(matched, model_id, label, similarity))
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

fn load_config() -> Result<PamConfig, String> {
    let content = read_trusted_config(DEFAULT_CONFIG_PATH)?;

    toml::from_str(&content).map_err(|e| format!("parse config: {e}"))
}

/// Read a config file only if it is trustworthy: the file and every parent
/// directory must be root-owned and not group- or world-writable. Anything
/// else is rejected and the module fails closed (falls through to password).
///
/// The file's own check uses `fstat` on the opened descriptor so the
/// validated inode is exactly the one whose contents are read (no TOCTOU
/// between a path-based stat and the read).
fn read_trusted_config(path: &str) -> Result<String, String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| format!("read config: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = file
            .metadata()
            .map_err(|e| format!("failed to stat config {path}: {e}"))?;
        validate_root_owned_nonwritable(
            metadata.uid(),
            metadata.permissions().mode(),
            &format!("config {path}"),
        )?;

        let mut current = Path::new(path).parent();
        while let Some(dir) = current {
            let meta = std::fs::metadata(dir)
                .map_err(|e| format!("failed to stat config parent {}: {e}", dir.display()))?;
            validate_root_owned_nonwritable(
                meta.uid(),
                meta.permissions().mode(),
                &format!("config parent dir {}", dir.display()),
            )?;
            current = dir.parent();
        }
    }

    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| format!("read config: {e}"))?;
    Ok(content)
}

/// Establish that `path` is a binary this module may exec as root.
///
/// The trust is entirely in the filesystem checks: resolve symlinks, then
/// require the target *and every ancestor directory* to be root-owned and not
/// group- or world-writable. Nothing else about the path is validated —
/// `AUTH_BIN` is a compiled-in absolute constant (there has been no
/// configurable `auth_bin` since N9), and `canonicalize` returns an absolute
/// path regardless, so the walk below establishes trust on its own.
///
/// Returns the canonical path to exec, or the reason it is not trusted.
fn verify_binary_trust(path: &str) -> Result<PathBuf, String> {
    let canonical = Path::new(path)
        .canonicalize()
        .map_err(|e| format!("failed to resolve {path}: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = std::fs::metadata(&canonical)
            .map_err(|e| format!("failed to stat {}: {e}", canonical.display()))?;

        validate_root_owned_nonwritable(
            metadata.uid(),
            metadata.permissions().mode(),
            &format!("auth binary {}", canonical.display()),
        )?;

        let mut current = canonical.parent();
        while let Some(dir) = current {
            let meta = std::fs::metadata(dir)
                .map_err(|e| format!("failed to stat parent dir {}: {e}", dir.display()))?;
            validate_root_owned_nonwritable(
                meta.uid(),
                meta.permissions().mode(),
                &format!("parent dir {}", dir.display()),
            )?;

            current = dir.parent();
        }
    }

    Ok(canonical)
}

#[cfg(unix)]
fn validate_root_owned_nonwritable(uid: u32, mode: u32, label: &str) -> Result<(), String> {
    if uid != 0 {
        return Err(format!("{label} must be owned by root"));
    }
    if mode & 0o022 != 0 {
        return Err(format!("{label} must not be group- or world-writable"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// One-shot authentication (daemonless)
// ---------------------------------------------------------------------------

/// Build the sanitized environment for the spawned oneshot child.
///
/// Allow-list only: `SSH_CONNECTION`/`SSH_TTY` are forwarded because the
/// child re-runs its own SSH-abort check (`facelock auth`). `PATH` is pinned
/// to trusted system directories. Everything else — `LD_*`, `XDG_*`,
/// `DBUS_*`, etc. — is dropped; the notification path constructs its own
/// session-bus address from the target UID and needs no inherited XDG vars.
fn oneshot_child_env(
    parent: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    const KEEP: &[&str] = &["SSH_CONNECTION", "SSH_TTY"];

    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = parent
        .filter(|(key, _)| KEEP.iter().any(|keep| key.as_os_str() == *keep))
        .collect();
    env.push(("PATH".into(), "/usr/bin:/bin".into()));
    env
}

/// How long an overrunning one-shot child gets to exit on SIGTERM before it
/// is killed. Generous: the child's own cancel check runs once per captured
/// frame, so it normally exits in well under one frame time.
const ONESHOT_TERM_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Poll interval while waiting out [`ONESHOT_TERM_GRACE`].
const ONESHOT_TERM_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// How long a SIGKILLed child is polled for before the blocking reap.
///
/// SIGKILL is uncatchable, so a child that received one is already dying and
/// this normally expires after a single poll; the bound exists so the reap
/// below is never the first thing that waits.
const ONESHOT_KILL_REAP_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

/// The three things the termination sequence needs from a child process.
///
/// A trait rather than a `std::process::Child` in the signature purely so the
/// sequence itself — the part that is auth-adjacent policy, not mechanism —
/// can be unit-tested without spawning anything.
trait Terminable {
    /// Send a signal to the child.
    fn signal(&mut self, signal: libc::c_int) -> std::io::Result<()>;
    /// Has it exited yet? Non-blocking.
    fn has_exited(&mut self) -> std::io::Result<bool>;
    /// Block until it is reaped. Only ever called after a SIGKILL that the
    /// kernel accepted, so the wait is bounded by the kernel, not by us.
    fn reap(&mut self) -> std::io::Result<()>;
}

impl Terminable for std::process::Child {
    fn signal(&mut self, signal: libc::c_int) -> std::io::Result<()> {
        // SAFETY: `kill` with a pid we own and a valid signal number. A dead
        // child yields ESRCH, which is reported as an error, never UB.
        let rc = unsafe { libc::kill(self.id() as libc::pid_t, signal) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn has_exited(&mut self) -> std::io::Result<bool> {
        self.try_wait().map(|status| status.is_some())
    }

    fn reap(&mut self) -> std::io::Result<()> {
        // `wait` returns the cached status immediately if a `try_wait` above
        // already reaped it, so calling this after the poll loop is free.
        self.wait().map(|_| ())
    }
}

/// Stop an overrunning one-shot child: **SIGTERM, wait, then SIGKILL**.
///
/// The module used to send SIGKILL straight away. A killed `facelock auth`
/// never runs `Drop`, so the camera is closed by the kernel without a
/// STREAMOFF and, on hardware whose IR emitter is under explicit XU control,
/// the LED stays on. SIGTERM gives the child the chance to end its scan and
/// clean up; SIGKILL is the backstop for a child wedged in a driver call,
/// where leaking the emitter beats hanging the PAM stack.
///
/// Returns the signals actually sent, in order — which is what the tests
/// assert on.
fn terminate_oneshot_child<T: Terminable>(
    child: &mut T,
    grace: std::time::Duration,
) -> Vec<libc::c_int> {
    let mut sent = Vec::with_capacity(2);
    if child.signal(libc::SIGTERM).is_ok() {
        sent.push(libc::SIGTERM);
    }

    let deadline = std::time::Instant::now() + grace;
    loop {
        // An error here means we cannot tell whether it exited, which is
        // treated as "not yet" — the grace period still bounds the wait, and
        // the SIGKILL below is harmless against a dead child.
        if child.has_exited().unwrap_or(false) {
            return sent;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(ONESHOT_TERM_POLL.min(grace));
    }

    if child.signal(libc::SIGKILL).is_ok() {
        sent.push(libc::SIGKILL);
        // Reap it. A child killed here is dead within microseconds, but until
        // somebody waits on it, it is a zombie occupying a PID slot for as
        // long as this PAM host lives — and the host may be a long-running
        // screen locker, not a `sudo` that exits a second later. The old
        // single non-blocking `try_wait` at the call site was a coin flip on
        // whether the child had been scheduled off yet.
        //
        // Blocking is safe *only* because SIGKILL was accepted: it cannot be
        // caught, blocked or ignored, so the kernel bounds this wait. When the
        // signal could not be delivered at all this branch is skipped and
        // nothing blocks — the case the original comment was worried about.
        let deadline = std::time::Instant::now() + ONESHOT_KILL_REAP_WAIT;
        while !child.has_exited().unwrap_or(false) {
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(ONESHOT_TERM_POLL.min(ONESHOT_KILL_REAP_WAIT));
        }
        let _ = child.reap();
    }
    sent
}

/// Run facelock auth as a subprocess for daemonless authentication.
///
/// The child's exit code carries the rejection class and is mapped by
/// [`oneshot_exit::classify`], class for class with the daemon transport:
/// 0 = matched, 1 = no match, 2 = error / no opinion, 3 = rate limited,
/// 4 = suppressed, 5 = all frames dark, anything else = no opinion. The
/// table is frozen in docs/contracts.md ("facelock auth Exit Codes").
fn run_oneshot_auth(service: &str, user: &str, config: &PamConfig) -> libc::c_int {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    let timeout_secs = config.recognition.timeout_secs as u64 + 3; // buffer for model load

    let auth_bin = match verify_binary_trust(AUTH_BIN) {
        Ok(path) => path,
        Err(e) => {
            log_auth(
                service,
                &format!("error: untrusted_auth_binary: {e}"),
                user,
                LOG_WARNING,
            );
            return PAM_IGNORE;
        }
    };

    let mut command = Command::new(&auth_bin);
    command
        .arg("auth")
        .arg("--user")
        .arg(user)
        .arg("--config")
        .arg(DEFAULT_CONFIG_PATH)
        // Sanitize the child environment: the PAM caller may be fully root
        // (no AT_SECURE), so inherited LD_PRELOAD/LD_* would be honored by
        // the dynamic linker in the spawned root child. Start from an empty
        // environment and re-add only the vetted allow-list.
        .env_clear()
        .envs(oneshot_child_env(std::env::vars_os()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    // Tie the child's life to this PAM host's. If the host is killed — a
    // screen locker's PAM helper torn down on unlock, a killed `sudo` — the
    // kernel sends the child SIGTERM, which `facelock auth` handles by
    // ending its scan and running `Drop` (STREAMOFF, IR emitter off). Without
    // it the child is reparented to init and keeps the camera on, alone, for
    // the rest of its timeout: the one-shot analogue of the daemon's caller
    // watch (ADR 008 §7).
    //
    // Read before the fork so the closure has something to compare against.
    //
    // SAFETY: `getpid` is a plain syscall with no preconditions.
    let expected_parent_pid = unsafe { libc::getpid() };

    // SAFETY: `pre_exec` runs in the forked child between fork and exec,
    // where only async-signal-safe work is allowed. `prctl`, `getppid` and
    // `raise` are single syscalls with no allocation, no locks and no libc
    // state — they are the operations `pre_exec` exists for, and the closure
    // captures only a `pid_t` by copy, so it allocates nothing either.
    // PDEATHSIG is cleared by exec only for setuid binaries; `facelock` is not
    // one (its trust is verified above), so it survives into the execed image.
    unsafe {
        command.pre_exec(move || {
            // The result is deliberately ignored: losing the parent-death
            // signal costs the promptness, never the authentication — the
            // child still exits on its own timeout, and the kill sequence
            // below still applies.
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            // PDEATHSIG fires on the parent's death, not on already being an
            // orphan — so a host that died in the window between `fork` and
            // the `prctl` above has already been mourned and nothing will
            // ever arrive. That leaves exactly what PDEATHSIG exists to
            // prevent: a `facelock auth` reparented to init, holding the
            // camera and the IR emitter alone for the rest of its timeout,
            // with no PAM host left to run the kill sequence. Closing the
            // window means asking once, after the fact, whether we are still
            // the child we thought we were.
            if libc::getppid() != expected_parent_pid {
                libc::raise(libc::SIGTERM);
            }
            Ok(())
        });
    }

    let result = command.spawn();

    let mut child = match result {
        Ok(c) => c,
        Err(e) => {
            log_auth(
                service,
                &format!("error: oneshot_spawn: {e}"),
                user,
                LOG_WARNING,
            );
            return PAM_IGNORE;
        }
    };

    // Wait with timeout
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // A child killed by a signal has no exit code; read it as 2
                // ("error / no opinion"), the frozen catch-all — an abandoned
                // attempt is not a decision about this face.
                let code = status.code().unwrap_or(2);
                let exit = oneshot_exit::classify(code);
                let severity = if exit.warn { LOG_WARNING } else { LOG_INFO };
                match exit.label {
                    Some(label) => log_auth(service, label, user, severity),
                    None => log_auth(
                        service,
                        &format!("error: oneshot_exit={code}"),
                        user,
                        severity,
                    ),
                }
                return exit.pam_code;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // SIGTERM first, SIGKILL only if it will not go. A
                    // SIGKILL skips the child's `Drop`, so the V4L2 stream is
                    // torn down by the kernel with no STREAMOFF and an
                    // XU-controlled IR emitter is left lit until something
                    // else turns it off (ADR 008 §7).
                    // Reaps the child itself: a child that exits on SIGTERM is
                    // reaped by the poll, and one that had to be killed is
                    // waited for there (both bounded — see the function).
                    terminate_oneshot_child(&mut child, ONESHOT_TERM_GRACE);
                    log_auth(service, "timeout (oneshot)", user, LOG_WARNING);
                    return PAM_AUTH_ERR;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                log_auth(
                    service,
                    &format!("error: oneshot_wait: {e}"),
                    user,
                    LOG_WARNING,
                );
                return PAM_IGNORE;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Core authentication logic
// ---------------------------------------------------------------------------

/// Map a recoverable daemon error message onto a PAM return code plus a
/// syslog reason. Every branch degrades (PAM_AUTH_ERR / PAM_IGNORE); a
/// daemon error can never map to PAM_SUCCESS.
fn pam_code_for_daemon_error(message: &str) -> (libc::c_int, &'static str) {
    if message.contains("rate_limit") || message.contains("rate limited") {
        // Deliberate failure, not fall-through: the user has exhausted the
        // face-auth budget; the password modules still run after us.
        (PAM_AUTH_ERR, "rate_limited")
    } else if message.contains("IR camera required") || message.contains("ir_required") {
        (PAM_IGNORE, "ir_required")
    } else if message == "cancelled" {
        // The attempt was abandoned, not refused: the caller's connection
        // went away, the system is suspending, or `ReleaseCamera` arrived.
        // The daemon has no opinion about this face, so abstain and let the
        // password modules run — which is what the user who just typed a
        // password expects. Matched exactly (not `contains`) so an arbitrary
        // error message that happens to mention cancelling cannot claim it.
        // The string is frozen protocol; see docs/contracts.md.
        (PAM_IGNORE, "cancelled")
    } else {
        (PAM_IGNORE, "error: internal")
    }
}

/// Map an unmatched authentication onto a PAM return code.
///
/// `face_seen` is the daemon's explicit answer to "did the detector see
/// anybody?", carried by the `model_id == -4` sentinel. Reading it off
/// `similarity` instead is what this exists to stop: the score is redacted to
/// `0.0` for every non-root caller, so for a user-run locker (hyprlock) a
/// genuine face-seen non-match was indistinguishable from an empty frame and
/// wrongly abstained (#108's N12, deferred to #109 and never carried).
///
/// `similarity` is only consulted when the reply carries no face-seen signal,
/// which for a current daemon means no face was seen — the same answer. It is
/// the fallback for a daemon older than the `-4` sentinel, where it reproduces
/// the previous behavior exactly.
///
/// Never PAM_SUCCESS: an unmatched attempt either fails or abstains.
fn pam_code_for_no_match(face_seen: bool, similarity: f32) -> libc::c_int {
    if face_seen {
        // The daemon looked at a face and said no. That is a decision, so
        // fail rather than abstain.
        return PAM_AUTH_ERR;
    }
    if similarity == 0.0 {
        PAM_IGNORE
    } else {
        PAM_AUTH_ERR
    }
}

fn identify(pamh: *mut libc::c_void) -> libc::c_int {
    // 0. Get PAM service name for logging
    let service = unsafe { pam_get_service(pamh) }.unwrap_or_else(|| "unknown".to_string());

    // We need the username early for logging, but pam_get_user may fail.
    // We'll get it after config loading so we can log pre-flight failures
    // with at least the service name.

    // 1. Read minimal config
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            log_auth(&service, &format!("error: config: {e}"), "?", LOG_WARNING);
            return PAM_IGNORE;
        }
    };

    // 2. Pre-flight checks
    if config.security.disabled {
        log_auth(&service, "disabled", "?", LOG_INFO);
        return PAM_IGNORE;
    }

    if config.security.abort_if_ssh && is_ssh_session() {
        log_auth(&service, "ssh_abort", "?", LOG_INFO);
        return PAM_IGNORE;
    }

    if config.security.abort_if_lid_closed && lid::is_lid_closed() {
        log_auth(&service, "lid_closed", "?", LOG_INFO);
        return PAM_IGNORE;
    }

    // Check service-specific policy
    if let Some(ref policy) = config.security.pam_policy {
        // If allowed_services is non-empty, only allow those
        if !policy.allowed_services.is_empty()
            && !policy.allowed_services.iter().any(|s| s == &service)
        {
            log_auth(&service, "service_denied", "?", LOG_INFO);
            return PAM_IGNORE;
        }
        // If service is in denied list, skip
        if policy.denied_services.iter().any(|s| s == &service) {
            log_auth(&service, "service_denied", "?", LOG_INFO);
            return PAM_IGNORE;
        }
    }

    // 3. Get PAM username
    let user = match unsafe { pam_get_user(pamh) } {
        Some(u) => u,
        None => {
            log_auth(&service, "error: no_user", "?", LOG_WARNING);
            return PAM_IGNORE;
        }
    };

    // 4. Display scanning notice
    if config.notification.notify_prompt && config.notification.terminal() {
        unsafe { pam_info(pamh, &gettext("Identifying face...")) };
    }

    // 5. Oneshot mode: run facelock-auth directly, skip D-Bus
    if config.daemon.mode == "oneshot" {
        let result = run_oneshot_auth(&service, &user, &config);
        if result == PAM_SUCCESS
            && config.notification.notify_on_success
            && config.notification.terminal()
        {
            unsafe { pam_info(pamh, &gettext("Face recognized.")) };
        }
        return result;
    }

    // 6. Daemon mode: connect via D-Bus, fall back to oneshot if unavailable
    // Desktop notifications are sent by the daemon itself (setpriv +
    // notify-send into the user's session bus); the auth_attempted signal
    // carries no notification duty. PAM only provides terminal feedback via
    // pam_info().
    match daemon_authenticate(&config, &user) {
        Ok(AuthResponse::Matched { .. }) => {
            log_auth(&service, "success", &user, LOG_INFO);
            if config.notification.notify_on_success && config.notification.terminal() {
                unsafe { pam_info(pamh, &gettext("Face recognized.")) };
            }
            PAM_SUCCESS
        }
        Ok(AuthResponse::NoMatchFaceSeen) => {
            log_auth(&service, "no_match", &user, LOG_INFO);
            pam_code_for_no_match(true, 0.0)
        }
        Ok(AuthResponse::NoMatch { similarity }) => {
            log_auth(&service, "no_match", &user, LOG_INFO);
            pam_code_for_no_match(false, similarity)
        }
        Ok(AuthResponse::Suppressed) => {
            log_auth(&service, "suppressed (no enrolled models)", &user, LOG_INFO);
            PAM_AUTHINFO_UNAVAIL
        }
        Ok(AuthResponse::Error { message }) => {
            // The daemon answered with a recoverable error (model_id == -2).
            // It is running and made a decision, so never fall back to a
            // fresh root oneshot attempt (which would bypass daemon-side
            // state such as rate limiting). Degrade to password instead.
            let (code, reason) = pam_code_for_daemon_error(&message);
            log_auth(&service, reason, &user, LOG_WARNING);
            code
        }
        Err(e) => {
            // D-Bus connection/call failed -- fall back to oneshot mode
            if e.contains("dbus_timeout") {
                log_auth(&service, "timeout", &user, LOG_WARNING);
                return PAM_AUTH_ERR;
            }
            log_auth(
                &service,
                &format!("dbus_failed: {e}, falling back to oneshot"),
                &user,
                LOG_WARNING,
            );
            let result = run_oneshot_auth(&service, &user, &config);
            if result == PAM_SUCCESS
                && config.notification.notify_on_success
                && config.notification.terminal()
            {
                unsafe { pam_info(pamh, &gettext("Face recognized.")) };
            }
            result
        }
    }
}

// ---------------------------------------------------------------------------
// Exported PAM entry points
// ---------------------------------------------------------------------------

/// Main authentication entry point.
///
/// Safety: Called by PAM framework. We use catch_unwind to prevent panics
/// from crossing the FFI boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut libc::c_void,
    _flags: libc::c_int,
    _argc: libc::c_int,
    _argv: *const *const libc::c_char,
) -> libc::c_int {
    // catch_unwind at FFI boundary: any panic -> PAM_IGNORE
    match panic::catch_unwind(panic::AssertUnwindSafe(|| identify(pamh))) {
        Ok(code) => code,
        Err(_) => {
            // Panic occurred -- log and fail gracefully
            log_auth("unknown", "error: internal_panic", "?", LOG_WARNING);
            PAM_IGNORE
        }
    }
}

/// Credential management (no-op, always succeeds).
///
/// Safety: Called by PAM framework.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut libc::c_void,
    _flags: libc::c_int,
    _argc: libc::c_int,
    _argv: *const *const libc::c_char,
) -> libc::c_int {
    PAM_SUCCESS
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The oneshot table, row for row (docs/contracts.md, "facelock auth
    /// Exit Codes"). The cross-crate half — that each code a rejection class
    /// makes the binary emit lands on the daemon transport's PAM code — is
    /// pinned in `facelock-cli`'s commands/auth.rs, which `include!`s this
    /// crate's `oneshot_exit.rs`.
    #[test]
    fn oneshot_exit_mapping_is_the_contract_table() {
        assert_eq!(oneshot_exit::classify(0).pam_code, PAM_SUCCESS);
        assert_eq!(oneshot_exit::classify(1).pam_code, PAM_AUTH_ERR);
        assert_eq!(oneshot_exit::classify(2).pam_code, PAM_IGNORE);
        assert_eq!(oneshot_exit::classify(3).pam_code, PAM_AUTH_ERR);
        assert_eq!(oneshot_exit::classify(4).pam_code, PAM_AUTHINFO_UNAVAIL);
        assert_eq!(oneshot_exit::classify(5).pam_code, PAM_IGNORE);
    }

    /// Invariant 3 of the exit-code contract: a code this build does not
    /// know abstains, so a binary newer than the module (and a signal death,
    /// which the caller reads as 2) degrades to fall-through and can never
    /// harden or soften the stack. The label is `None` so the syslog line
    /// names the raw code.
    #[test]
    fn unknown_oneshot_exit_codes_abstain() {
        for code in [-1, 6, 7, 9, 25, 42, 101, 255] {
            let exit = oneshot_exit::classify(code);
            assert_eq!(exit.pam_code, PAM_IGNORE, "exit {code}");
            assert!(exit.label.is_none(), "exit {code} must log its raw value");
        }
    }

    /// Never PAM_SUCCESS except exit 0: an exit *status* is attacker-visible
    /// but not attacker-choosable (the binary's trust is verified before the
    /// spawn); still, the mapping itself must make a wrong success
    /// impossible.
    #[test]
    fn only_exit_zero_authenticates() {
        for code in -128..=512 {
            let exit = oneshot_exit::classify(code);
            if code == 0 {
                assert_eq!(exit.pam_code, PAM_SUCCESS);
            } else {
                assert_ne!(
                    exit.pam_code, PAM_SUCCESS,
                    "exit {code} may not authenticate"
                );
            }
        }
    }

    #[test]
    fn test_is_ssh_session_fallback() {
        // In a test environment without SSH, should return false
        // (reads /proc/self/environ which exists but won't have SSH vars in test)
        // This is a soft check -- it depends on the test environment
        let _ = is_ssh_session();
    }

    #[test]
    fn test_config_parsing_minimal() {
        let toml_str = r#"
[daemon]
mode = "daemon"
"#;
        let config: PamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.mode, "daemon");
        assert!(!config.security.disabled);
        assert!(config.security.abort_if_ssh);
        assert!(config.security.abort_if_lid_closed);
        assert_eq!(config.recognition.timeout_secs, DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn test_config_parsing_full() {
        let toml_str = r#"
[daemon]
mode = "oneshot"

[security]
disabled = true
abort_if_ssh = false
abort_if_lid_closed = false

[security.pam_policy]
allowed_services = ["sudo"]
denied_services = ["sshd"]

[recognition]
timeout_secs = 10
"#;
        let config: PamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.mode, "oneshot");
        assert!(config.security.disabled);
        assert!(!config.security.abort_if_ssh);
        assert!(!config.security.abort_if_lid_closed);
        assert_eq!(config.recognition.timeout_secs, 10);

        let policy = config.security.pam_policy.unwrap();
        assert_eq!(policy.allowed_services, vec!["sudo"]);
        assert_eq!(policy.denied_services, vec!["sshd"]);
    }

    #[test]
    fn test_config_defaults() {
        let toml_str = "";
        let config: PamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.mode, "daemon");
        assert!(!config.security.disabled);
        assert!(config.security.abort_if_ssh);
        assert_eq!(config.recognition.timeout_secs, DEFAULT_TIMEOUT_SECS);
    }

    /// D9 drift pin: a `[notification]` section present with every key
    /// omitted (serde field defaults) must parse identically to no section
    /// at all (`PamNotificationConfig::default()`). The mode enum's
    /// `#[default]` had drifted to `Both` while the struct default said
    /// `Terminal` — undetected because `terminal()` treats both identically.
    #[test]
    fn notification_section_present_with_keys_omitted_equals_absent() {
        let present: PamConfig = toml::from_str("[notification]\n").unwrap();
        let absent: PamConfig = toml::from_str("").unwrap();
        let default = PamNotificationConfig::default();
        for config in [&present.notification, &absent.notification] {
            assert_eq!(config.mode, default.mode);
            assert_eq!(config.notify_prompt, default.notify_prompt);
            assert_eq!(config.notify_on_success, default.notify_on_success);
        }
    }

    /// Conformance with the canonical schema, in the only form the
    /// dependency ceiling allows: parse the same shipped template
    /// facelock-core ships and require the effective notification settings
    /// to equal our defaults. The template has an active `[notification]`
    /// header with every key commented — exactly the shape that exposed the
    /// default drift — and any future template default that this crate does
    /// not mirror will fail here visibly.
    #[test]
    fn shipped_template_notification_settings_equal_defaults() {
        let template = include_str!("../../../config/facelock.toml");
        let config: PamConfig = toml::from_str(template).unwrap();
        let default = PamNotificationConfig::default();
        assert_eq!(config.notification.mode, default.mode);
        assert_eq!(config.notification.notify_prompt, default.notify_prompt);
        assert_eq!(
            config.notification.notify_on_success,
            default.notify_on_success
        );
        assert!(config.notification.terminal());
    }

    #[test]
    fn test_config_ignores_unknown_sections() {
        // PAM config should silently ignore sections it doesn't know about
        let toml_str = r#"
[device]
path = "/dev/video0"

[daemon]
mode = "daemon"

[storage]
db_path = "/tmp/test.db"
"#;
        let config: PamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.mode, "daemon");
    }

    #[test]
    fn config_with_removed_auth_bin_key_still_parses() {
        // N9 removed the `auth_bin` key. Existing installs may still have it
        // in /etc/facelock/config.toml; it must be ignored, never an error.
        let toml_str = r#"
[daemon]
mode = "oneshot"
auth_bin = "/usr/local/bin/evil"
"#;
        let config: PamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.mode, "oneshot");
    }

    /// An absent binary is untrusted, not "assume it's fine": the module
    /// falls through to the next PAM module rather than exec'ing something
    /// that appears later at that path.
    #[test]
    fn verify_binary_trust_rejects_missing_paths() {
        let err = verify_binary_trust("/definitely/missing/facelock").unwrap_err();
        assert!(err.contains("failed to resolve"));
    }

    #[cfg(unix)]
    #[test]
    fn validate_root_owned_nonwritable_rejects_group_writable_mode() {
        let err = validate_root_owned_nonwritable(0, 0o100775, "auth binary /usr/bin/facelock")
            .unwrap_err();
        assert!(err.contains("group- or world-writable"));
    }

    #[cfg(unix)]
    #[test]
    fn validate_root_owned_nonwritable_rejects_non_root_owner() {
        let err = validate_root_owned_nonwritable(1000, 0o100755, "auth binary /usr/bin/facelock")
            .unwrap_err();
        assert!(err.contains("owned by root"));
    }

    // --- Plan 05: oneshot child environment sanitization ---

    fn env_pairs(pairs: &[(&str, &str)]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)))
            .collect()
    }

    #[test]
    fn oneshot_child_env_drops_ld_preload_and_keeps_ssh_vars() {
        let parent = env_pairs(&[
            ("LD_PRELOAD", "/tmp/evil.so"),
            ("LD_LIBRARY_PATH", "/tmp"),
            ("SSH_CONNECTION", "192.0.2.1 1111 192.0.2.2 22"),
            ("SSH_TTY", "/dev/pts/3"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/tmp/bus"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("PATH", "/tmp/evil:/usr/bin"),
        ]);
        let child = oneshot_child_env(parent.into_iter());

        let keys: Vec<&str> = child.iter().map(|(k, _)| k.to_str().unwrap()).collect();
        assert!(!keys.contains(&"LD_PRELOAD"));
        assert!(!keys.contains(&"LD_LIBRARY_PATH"));
        assert!(!keys.contains(&"DBUS_SESSION_BUS_ADDRESS"));
        assert!(!keys.contains(&"XDG_RUNTIME_DIR"));
        assert!(keys.contains(&"SSH_CONNECTION"));
        assert!(keys.contains(&"SSH_TTY"));
    }

    #[test]
    fn oneshot_child_env_pins_path() {
        let parent = env_pairs(&[("PATH", "/tmp/evil:/usr/bin")]);
        let child = oneshot_child_env(parent.into_iter());
        let path = child
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.to_str().unwrap().to_string())
            .unwrap();
        assert_eq!(path, "/usr/bin:/bin");
    }

    #[test]
    fn oneshot_child_env_without_ssh_vars_is_path_only() {
        let parent = env_pairs(&[("HOME", "/root"), ("TERM", "xterm")]);
        let child = oneshot_child_env(parent.into_iter());
        assert_eq!(child.len(), 1);
        assert_eq!(child[0].0, "PATH");
    }

    // --- Plan 05: config trust validation ---

    #[test]
    fn read_trusted_config_missing_file_is_an_error() {
        let err = read_trusted_config("/definitely/missing/facelock-config.toml").unwrap_err();
        assert!(err.contains("read config"));
    }

    #[cfg(unix)]
    #[test]
    fn read_trusted_config_rejects_untrusted_file() {
        // Create a config in a tmp dir. When running unprivileged the file is
        // not root-owned; when running as root the world-writable tmp parent
        // fails the parent-directory check. Either way it must be rejected.
        let dir = std::env::temp_dir().join(format!("pam-facelock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[daemon]\nmode = \"daemon\"\n").unwrap();

        let result = read_trusted_config(path.to_str().unwrap());
        std::fs::remove_dir_all(&dir).ok();

        let err = result.unwrap_err();
        assert!(
            err.contains("owned by root") || err.contains("group- or world-writable"),
            "unexpected error: {err}"
        );
    }

    // --- Plan 05: daemon reply decoding (dead -2 contract made live) ---

    #[test]
    fn parse_auth_reply_decodes_recoverable_error() {
        match parse_auth_reply(false, -2, "rate limited".into(), 0.0) {
            AuthResponse::Error { message } => assert_eq!(message, "rate limited"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn parse_auth_reply_decodes_suppressed() {
        assert!(matches!(
            parse_auth_reply(false, -3, String::new(), 0.0),
            AuthResponse::Suppressed
        ));
    }

    #[test]
    fn parse_auth_reply_decodes_match() {
        assert!(matches!(
            parse_auth_reply(true, 4, "me".into(), 0.93),
            AuthResponse::Matched { .. }
        ));
    }

    #[test]
    fn parse_auth_reply_decodes_no_match() {
        match parse_auth_reply(false, -1, String::new(), 0.4) {
            AuthResponse::NoMatch { similarity } => assert!((similarity - 0.4).abs() < 1e-6),
            _ => panic!("expected NoMatch"),
        }
    }

    #[test]
    fn parse_auth_reply_decodes_face_seen_no_match() {
        assert!(matches!(
            parse_auth_reply(false, -4, String::new(), 0.0),
            AuthResponse::NoMatchFaceSeen
        ));
    }

    /// The defect: a non-root caller's score is redacted to 0.0 by the daemon
    /// (PR #108's N12), so "we saw a face and it did not match" and "the
    /// camera saw nobody" arrived as the same reply and both abstained. A
    /// user-run locker (hyprlock) is exactly that caller.
    ///
    /// Both replies below carry `similarity == 0.0`, as a redacted caller's
    /// always do; only the sentinel separates them, and only the sentinel is
    /// consulted.
    #[test]
    fn a_redacted_face_seen_no_match_is_not_read_as_an_empty_frame() {
        let face_seen = parse_auth_reply(false, -4, String::new(), 0.0);
        let no_face = parse_auth_reply(false, -1, String::new(), 0.0);

        let code_for = |response: &AuthResponse| match response {
            AuthResponse::NoMatchFaceSeen => pam_code_for_no_match(true, 0.0),
            AuthResponse::NoMatch { similarity } => pam_code_for_no_match(false, *similarity),
            _ => panic!("expected a non-match reply"),
        };

        assert_eq!(
            code_for(&face_seen),
            PAM_AUTH_ERR,
            "the daemon looked at a face and said no — that is a decision"
        );
        assert_eq!(
            code_for(&no_face),
            PAM_IGNORE,
            "the camera saw nobody — abstain"
        );
    }

    /// The pre-`-4` fallback, which a daemon older than the sentinel still
    /// exercises: `-1` plus an unredacted (root-caller) score reproduces the
    /// previous behavior exactly, so an upgrade window cannot change any
    /// existing decision.
    #[test]
    fn no_match_without_a_face_seen_signal_falls_back_to_the_score() {
        assert_eq!(pam_code_for_no_match(false, 0.0), PAM_IGNORE);
        assert_eq!(pam_code_for_no_match(false, 0.31), PAM_AUTH_ERR);
    }

    #[test]
    fn no_match_never_maps_to_success() {
        for (face_seen, similarity) in [(true, 0.0), (true, 0.9), (false, 0.0), (false, 0.42)] {
            assert_ne!(
                pam_code_for_no_match(face_seen, similarity),
                PAM_SUCCESS,
                "fail-open for face_seen={face_seen} similarity={similarity}"
            );
        }
    }

    #[test]
    fn parse_auth_reply_error_sentinel_requires_unmatched() {
        // A matched=true reply always wins over sentinel model ids: the
        // daemon never sends this, but decoding must not invent an error.
        assert!(matches!(
            parse_auth_reply(true, -2, "x".into(), 0.9),
            AuthResponse::Matched { .. }
        ));
    }

    // --- Plan 05: daemon error → PAM code routing ---

    #[test]
    fn daemon_error_rate_limited_maps_to_auth_err() {
        let (code, reason) = pam_code_for_daemon_error("rate limited");
        assert_eq!(code, PAM_AUTH_ERR);
        assert_eq!(reason, "rate_limited");
    }

    #[test]
    fn daemon_error_ir_required_maps_to_ignore() {
        let (code, reason) =
            pam_code_for_daemon_error("IR camera required for authentication. ...");
        assert_eq!(code, PAM_IGNORE);
        assert_eq!(reason, "ir_required");
    }

    /// A child that records what it was sent and exits when told to.
    struct FakeChild {
        sent: Vec<libc::c_int>,
        exits_on_term: bool,
        /// Whether the sequence blocked on a wait — the difference between
        /// reaping the child and leaving a zombie behind.
        reaped: bool,
        /// Delivery failure, for the child that cannot be signalled at all.
        signals_fail: bool,
    }

    impl FakeChild {
        fn new(exits_on_term: bool) -> Self {
            Self {
                sent: Vec::new(),
                exits_on_term,
                reaped: false,
                signals_fail: false,
            }
        }
    }

    impl Terminable for FakeChild {
        fn signal(&mut self, signal: libc::c_int) -> std::io::Result<()> {
            if self.signals_fail {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            self.sent.push(signal);
            Ok(())
        }

        fn has_exited(&mut self) -> std::io::Result<bool> {
            // A SIGKILLed child is gone by the next poll; a SIGTERMed one only
            // if it is the well-behaved kind.
            if self.sent.contains(&libc::SIGKILL) {
                return Ok(true);
            }
            Ok(self.exits_on_term && self.sent.contains(&libc::SIGTERM))
        }

        fn reap(&mut self) -> std::io::Result<()> {
            self.reaped = true;
            Ok(())
        }
    }

    /// The ordering that matters: a well-behaved child is asked to stop, not
    /// killed. A SIGKILL skips `Drop`, so the V4L2 stream is never told to
    /// STREAMOFF and an XU-controlled IR emitter is left lit.
    #[test]
    fn a_child_that_exits_on_sigterm_is_never_killed() {
        let mut child = FakeChild::new(true);
        let sent = terminate_oneshot_child(&mut child, std::time::Duration::from_millis(50));
        assert_eq!(sent, vec![libc::SIGTERM]);
        assert!(
            !sent.contains(&libc::SIGKILL),
            "a child that exited on SIGTERM must not be killed"
        );
    }

    /// The backstop: a child wedged in a driver call is killed after the
    /// grace period. Leaking the emitter beats hanging the PAM stack.
    #[test]
    fn a_wedged_child_is_killed_after_the_grace_period() {
        let grace = std::time::Duration::from_millis(60);
        let mut child = FakeChild::new(false);
        let started = std::time::Instant::now();
        let sent = terminate_oneshot_child(&mut child, grace);
        assert_eq!(
            sent,
            vec![libc::SIGTERM, libc::SIGKILL],
            "SIGTERM must come first, and SIGKILL only after it"
        );
        assert!(
            started.elapsed() >= grace,
            "the child was killed before its grace period ran out"
        );
    }

    /// ...and it is *reaped*, not merely killed.
    ///
    /// A single non-blocking `try_wait` after the kill was a coin flip on
    /// whether the child had been scheduled off yet; when it lost, the PAM
    /// host held a zombie for the rest of its own life — and that host can be
    /// a screen locker that lives for days, not a `sudo` that exits in a
    /// second. Blocking here is safe because SIGKILL cannot be caught, so the
    /// kernel bounds the wait.
    #[test]
    fn a_killed_child_is_reaped_rather_than_left_a_zombie() {
        let mut child = FakeChild::new(false);
        let sent = terminate_oneshot_child(&mut child, std::time::Duration::from_millis(20));
        assert!(sent.contains(&libc::SIGKILL));
        assert!(child.reaped, "the killed child was never waited for");
    }

    /// The child that exits on its own is reaped by the grace-period poll —
    /// `try_wait` reaps as it reports — so the sequence must not go on to
    /// block on a wait it does not need.
    #[test]
    fn a_child_that_exits_on_sigterm_is_not_waited_on_again() {
        let mut child = FakeChild::new(true);
        terminate_oneshot_child(&mut child, std::time::Duration::from_millis(50));
        assert!(
            !child.reaped,
            "a child already reaped by the poll must not be waited on again"
        );
    }

    /// The case the blocking wait must never be reached in: signals that
    /// cannot be delivered at all. Waiting on a child we could not signal is
    /// how the whole PAM stack would hang, so no signal means no wait.
    #[test]
    fn a_child_that_cannot_be_signalled_is_never_waited_on() {
        let mut child = FakeChild::new(false);
        child.signals_fail = true;
        let sent = terminate_oneshot_child(&mut child, std::time::Duration::from_millis(20));
        assert!(sent.is_empty(), "nothing was delivered: {sent:?}");
        assert!(
            !child.reaped,
            "blocked on a child that could not be signalled"
        );
    }

    /// The grace period is the documented one: half a second, matching the
    /// container assertion that the child exits within it.
    #[test]
    fn the_sigterm_grace_period_is_half_a_second() {
        assert_eq!(ONESHOT_TERM_GRACE, std::time::Duration::from_millis(500));
    }

    /// A cancelled attempt is an abstention, never a failure: the daemon
    /// stopped looking, it did not look and say no.
    #[test]
    fn daemon_error_cancelled_maps_to_ignore() {
        let (code, reason) = pam_code_for_daemon_error("cancelled");
        assert_eq!(code, PAM_IGNORE);
        assert_eq!(reason, "cancelled");
    }

    /// The cancellation row matches the frozen string exactly, so an error
    /// that merely mentions cancelling still lands on the generic branch.
    #[test]
    fn only_the_exact_cancelled_string_claims_the_cancelled_row() {
        for message in [
            "camera error: capture cancelled by driver",
            "storage error: transaction cancelled",
        ] {
            let (code, reason) = pam_code_for_daemon_error(message);
            assert_eq!(code, PAM_IGNORE);
            assert_eq!(reason, "error: internal", "message: {message}");
        }
    }

    #[test]
    fn daemon_error_never_maps_to_success() {
        for message in [
            "rate limited",
            "IR camera required",
            "camera error: device busy",
            "storage error: disk io",
            "cancelled",
            "",
        ] {
            let (code, _) = pam_code_for_daemon_error(message);
            assert_ne!(code, PAM_SUCCESS, "fail-open for message: {message}");
        }
    }

    // --- Plan 05: structured timeout classification ---

    #[test]
    fn timeout_classification_matches_io_timed_out() {
        let err = zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timeout waiting for reply",
        )));
        assert!(is_timeout_zbus_error(&err));
    }

    #[test]
    fn timeout_classification_matches_fdo_variants() {
        for fdo_err in [
            zbus::fdo::Error::NoReply("no reply".into()),
            zbus::fdo::Error::Timeout("timeout".into()),
            zbus::fdo::Error::TimedOut("timed out".into()),
        ] {
            let err = zbus::Error::FDO(Box::new(fdo_err));
            assert!(is_timeout_zbus_error(&err), "not classified: {err}");
        }
    }

    #[test]
    fn timeout_classification_rejects_other_errors() {
        assert!(!is_timeout_zbus_error(&zbus::Error::Failure(
            "something timed out-ish but is not a timeout variant".into()
        )));
        let io_err = zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        )));
        assert!(!is_timeout_zbus_error(&io_err));
    }

    // --- E2: shared-schema conformance (domain-object-map.md §1) ---

    /// The shipped config template is parsed by two independent, deliberately
    /// unmerged schemas: facelock-core's `Config` (see
    /// `crates/facelock-core/tests/config_template_test.rs`) and this
    /// crate's private `PamConfig`, kept minimal by the PAM dependency
    /// ceiling. This is PAM's half of that contract: if a future key drifts
    /// so one side gains a key the other rejects (a `deny_unknown_fields`
    /// creeping onto either schema, or a new required field with no
    /// `#[serde(default)]`), this fails loudly in CI instead of surfacing as
    /// a silent config parse error at `pam_sm_authenticate` time on a real
    /// system.
    #[test]
    fn shipped_config_template_parses_as_pam_config() {
        const TEMPLATE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/facelock.toml"
        ));
        let config: PamConfig = toml::from_str(TEMPLATE)
            .expect("PamConfig must parse the same shipped template facelock-core parses");

        // Every key this template documents for PAM is shipped commented out
        // (illustrating syntax, not overriding anything), so parsing it must
        // be indistinguishable from parsing an empty document.
        assert_eq!(config.daemon.mode, "daemon");
        assert!(!config.security.disabled);
        assert!(config.security.abort_if_ssh);
        assert!(config.security.abort_if_lid_closed);
        assert!(config.security.pam_policy.is_none());
        assert_eq!(config.recognition.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(config.notification.terminal());
        assert!(config.notification.notify_prompt);
        assert!(config.notification.notify_on_success);
    }
}
