// PAM module for facelock face authentication.
//
// Dependency rule: libc, toml, serde, zbus ONLY. No facelock-core.
// This is security-critical code: never panic across FFI, always fail gracefully,
// all auth attempts logged to syslog.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString};
use std::panic;

use serde::Deserialize;

// ---------------------------------------------------------------------------
// PAM constants
// ---------------------------------------------------------------------------

const PAM_SUCCESS: libc::c_int = 0;
const PAM_AUTH_ERR: libc::c_int = 7;
const PAM_IGNORE: libc::c_int = 25;

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
    /// Path to the facelock binary for oneshot mode
    #[serde(default = "default_auth_bin")]
    auth_bin: String,
}

impl Default for PamDaemonConfig {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            auth_bin: default_auth_bin(),
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
    #[serde(default)]
    pam_policy: Option<PamPolicyConfig>,
}

impl Default for PamSecurityConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            abort_if_ssh: true,
            abort_if_lid_closed: true,
            pam_policy: None,
        }
    }
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // Variants used via deserialization
enum PamNotificationMode {
    Off,
    Terminal,
    Desktop,
    #[default]
    Both,
}

#[derive(Deserialize)]
struct PamNotificationConfig {
    #[serde(default)]
    mode: PamNotificationMode,
    #[serde(default = "default_true")]
    notify_prompt: bool,
    #[serde(default = "default_true")]
    notify_on_success: bool,
}

impl Default for PamNotificationConfig {
    fn default() -> Self {
        Self {
            mode: PamNotificationMode::Both,
            notify_prompt: true,
            notify_on_success: true,
        }
    }
}

impl PamNotificationConfig {
    fn terminal(&self) -> bool {
        matches!(self.mode, PamNotificationMode::Terminal | PamNotificationMode::Both)
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

fn default_auth_bin() -> String {
    "/usr/bin/facelock".to_string()
}

fn default_true() -> bool {
    true
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
        let _ = conv_fn(1, &msg_ptr as *const _ as *mut _, &mut resp_ptr, conv.appdata_ptr);
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
            data.split(|&b| b == 0).any(|var| {
                var.starts_with(b"SSH_CONNECTION=") || var.starts_with(b"SSH_TTY=")
            })
        })
        .unwrap_or(false)
}

/// Check if the laptop lid is closed
fn is_lid_closed() -> bool {
    // Try multiple lid paths (different ACPI implementations)
    for lid_path in &[
        "/proc/acpi/button/lid/LID0/state",
        "/proc/acpi/button/lid/LID/state",
        "/proc/acpi/button/lid/LID1/state",
    ] {
        if let Ok(contents) = std::fs::read_to_string(lid_path) {
            return contents.contains("closed");
        }
    }
    false
}

// ---------------------------------------------------------------------------
// D-Bus daemon authentication
// ---------------------------------------------------------------------------

/// Parsed auth response from daemon.
enum AuthResponse {
    /// Face matched with given similarity score
    Matched { similarity: f32 },
    /// Face not matched
    NoMatch { similarity: f32 },
    /// Daemon returned an error
    Error { message: String },
}

/// Authenticate via D-Bus system bus to the facelock daemon.
fn daemon_authenticate(config: &PamConfig, user: &str) -> Result<AuthResponse, String> {
    // Timeout = recognition timeout + buffer for camera open/warmup/model load
    let timeout_secs = config.recognition.timeout_secs as u64 + 5;

    let connection = zbus::blocking::connection::Builder::system()
        .map_err(|e| format!("dbus_connect_failed: {e}"))?
        .method_timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("dbus_connect_failed: {e}"))?;

    let proxy = zbus::blocking::Proxy::new(
        &connection,
        DBUS_BUS_NAME,
        DBUS_OBJECT_PATH,
        DBUS_INTERFACE_NAME,
    ).map_err(|e| format!("dbus_proxy_failed: {e}"))?;

    // D-Bus method returns (matched: bool, model_id: i32, label: String, similarity: f64)
    let reply: (bool, i32, String, f64) = proxy
        .call("Authenticate", &(user,))
        .map_err(|e: zbus::Error| {
            let msg = e.to_string();
            // Check if this is a timeout or connection error for fallback
            if msg.contains("timed out") || msg.contains("Timeout") {
                format!("dbus_timeout: {msg}")
            } else {
                format!("dbus_call_failed: {msg}")
            }
        })?;

    let (matched, model_id, label, similarity) = reply;

    // Check for D-Bus error responses encoded in the return value
    // model_id == -2 with matched == false signals a daemon error, label contains the error message
    if !matched && model_id == -2 {
        return Ok(AuthResponse::Error { message: label });
    }

    if matched {
        Ok(AuthResponse::Matched {
            similarity: similarity as f32,
        })
    } else if similarity == 0.0 && model_id == -1 {
        // No enrolled faces
        Ok(AuthResponse::NoMatch { similarity: 0.0 })
    } else {
        Ok(AuthResponse::NoMatch {
            similarity: similarity as f32,
        })
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

fn load_config() -> Result<PamConfig, String> {
    let config_path = std::env::var("FACELOCK_CONFIG")
        .unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());

    let content =
        std::fs::read_to_string(&config_path).map_err(|e| format!("read config: {e}"))?;

    toml::from_str(&content).map_err(|e| format!("parse config: {e}"))
}

// ---------------------------------------------------------------------------
// One-shot authentication (daemonless)
// ---------------------------------------------------------------------------

/// Run facelock auth as a subprocess for daemonless authentication.
/// Exit codes: 0 = matched, 1 = no match, 2+ = error.
fn run_oneshot_auth(service: &str, user: &str, config: &PamConfig) -> libc::c_int {
    use std::process::Command;

    let timeout_secs = config.recognition.timeout_secs as u64 + 3; // buffer for model load

    // Resolve config path (same logic as load_config)
    let config_path = std::env::var("FACELOCK_CONFIG")
        .unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());

    let result = Command::new(&config.daemon.auth_bin)
        .arg("auth")
        .arg("--user")
        .arg(user)
        .arg("--config")
        .arg(&config_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();

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
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(timeout_secs);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code().unwrap_or(2);
                return match code {
                    0 => {
                        log_auth(service, "success (oneshot)", user, LOG_INFO);
                        PAM_SUCCESS
                    }
                    1 => {
                        log_auth(service, "no_match (oneshot)", user, LOG_INFO);
                        PAM_AUTH_ERR
                    }
                    _ => {
                        log_auth(
                            service,
                            &format!("error: oneshot_exit={code}"),
                            user,
                            LOG_WARNING,
                        );
                        PAM_IGNORE
                    }
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
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

    if config.security.abort_if_lid_closed && is_lid_closed() {
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
        unsafe { pam_info(pamh, "Identifying face...") };
    }

    // 5. Oneshot mode: run facelock-auth directly, skip D-Bus
    if config.daemon.mode == "oneshot" {
        let result = run_oneshot_auth(&service, &user, &config);
        if result == PAM_SUCCESS
            && config.notification.notify_on_success
            && config.notification.terminal()
        {
            unsafe { pam_info(pamh, "Face recognized.") };
        }
        return result;
    }

    // 6. Daemon mode: connect via D-Bus, fall back to oneshot if unavailable
    // Desktop notifications are handled by the daemon's auth_attempted D-Bus signal;
    // PAM only provides terminal feedback via pam_info().
    match daemon_authenticate(&config, &user) {
        Ok(AuthResponse::Matched { similarity }) => {
            log_auth(
                &service,
                &format!("success (similarity={similarity:.3})"),
                &user,
                LOG_INFO,
            );
            if config.notification.notify_on_success && config.notification.terminal() {
                unsafe { pam_info(pamh, "Face recognized.") };
            }
            PAM_SUCCESS
        }
        Ok(AuthResponse::NoMatch { similarity }) => {
            if similarity == 0.0 {
                // similarity 0.0 means no enrolled faces -- skip face auth entirely
                log_auth(&service, "no_enrolled_faces", &user, LOG_INFO);
                PAM_IGNORE
            } else {
                log_auth(
                    &service,
                    &format!("no_match (similarity={similarity:.3})"),
                    &user,
                    LOG_INFO,
                );
                PAM_AUTH_ERR
            }
        }
        Ok(AuthResponse::Error { message }) => {
            // Map specific daemon errors to appropriate PAM codes
            if message.contains("rate_limit") {
                log_auth(&service, "rate_limited", &user, LOG_WARNING);
                PAM_AUTH_ERR
            } else if message.contains("IR camera required") || message.contains("ir_required") {
                log_auth(&service, "ir_required", &user, LOG_WARNING);
                PAM_IGNORE
            } else {
                log_auth(
                    &service,
                    &format!("error: {message}"),
                    &user,
                    LOG_WARNING,
                );
                PAM_IGNORE
            }
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
                unsafe { pam_info(pamh, "Face recognized.") };
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
}
