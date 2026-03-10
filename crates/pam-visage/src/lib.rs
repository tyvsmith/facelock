// PAM module for visage face authentication.
//
// STRICT dependency rule: libc, toml, serde ONLY. No visage-core.
// This is security-critical code: never panic across FFI, always fail gracefully,
// all socket ops have timeouts, all auth attempts logged to syslog.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::panic;
use std::time::Duration;

use serde::Deserialize;

// ---------------------------------------------------------------------------
// PAM constants
// ---------------------------------------------------------------------------

const PAM_SUCCESS: libc::c_int = 0;
const PAM_AUTH_ERR: libc::c_int = 7;
const PAM_IGNORE: libc::c_int = 25;

// Syslog constants
const LOG_AUTH: libc::c_int = 4 << 3; // LOG_AUTH facility
const LOG_INFO: libc::c_int = 6;
const LOG_WARNING: libc::c_int = 4;

// IPC constants
const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

// Default config path
const DEFAULT_CONFIG_PATH: &str = "/etc/visage/config.toml";
const DEFAULT_SOCKET_PATH: &str = "/run/visage/visage.sock";
const DEFAULT_TIMEOUT_SECS: u32 = 5;

// Socket connect timeout
const CONNECT_TIMEOUT_SECS: u64 = 2;

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
}

#[derive(Deserialize)]
struct PamDaemonConfig {
    #[serde(default = "default_socket")]
    socket_path: String,
}

impl Default for PamDaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket(),
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
    #[serde(default = "default_true")]
    #[allow(dead_code)]
    detection_notice: bool,
    #[serde(default)]
    pam_policy: Option<PamPolicyConfig>,
}

impl Default for PamSecurityConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            abort_if_ssh: true,
            abort_if_lid_closed: true,
            detection_notice: true,
            pam_policy: None,
        }
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

fn default_socket() -> String {
    DEFAULT_SOCKET_PATH.to_string()
}

fn default_timeout() -> u32 {
    DEFAULT_TIMEOUT_SECS
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Syslog logging
// ---------------------------------------------------------------------------

/// Log an authentication event to syslog (LOG_AUTH facility).
///
/// Format: pam_visage(<service>): <result> for user <username>
fn log_auth(service: &str, result: &str, user: &str, level: libc::c_int) {
    // Use a static ident string to avoid dangling pointer issues.
    // openlog requires the ident pointer to remain valid until closelog.
    let ident = b"pam_visage\0";
    let msg = match CString::new(format!(
        "pam_visage({}): {} for user {}",
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
// IPC protocol (inline reimplementation, ~50 lines)
// ---------------------------------------------------------------------------

/// Encode a varint (bincode 2 standard config format).
/// Returns the number of bytes written.
fn encode_varint(value: u64, buf: &mut [u8]) -> usize {
    // bincode 2 varint encoding:
    // 0-250: single byte as-is
    // 251: followed by u16 LE (251..=65535)
    // 252: followed by u32 LE
    // 253: followed by u64 LE
    if value <= 250 {
        buf[0] = value as u8;
        1
    } else if value <= 0xFFFF {
        buf[0] = 251;
        buf[1..3].copy_from_slice(&(value as u16).to_le_bytes());
        3
    } else if value <= 0xFFFF_FFFF {
        buf[0] = 252;
        buf[1..5].copy_from_slice(&(value as u32).to_le_bytes());
        5
    } else {
        buf[0] = 253;
        buf[1..9].copy_from_slice(&value.to_le_bytes());
        9
    }
}

/// Decode a varint from a byte slice. Returns (value, bytes_consumed).
fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        v @ 0..=250 => Some((v as u64, 1)),
        251 => {
            if data.len() < 3 {
                return None;
            }
            let val = u16::from_le_bytes([data[1], data[2]]);
            Some((val as u64, 3))
        }
        252 => {
            if data.len() < 5 {
                return None;
            }
            let val = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
            Some((val as u64, 5))
        }
        253 => {
            if data.len() < 9 {
                return None;
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&data[1..9]);
            Some((u64::from_le_bytes(bytes), 9))
        }
        _ => None,
    }
}

/// Build the bincode-encoded Authenticate { user } request.
///
/// Wire format (bincode 2, standard config):
///   varint(0)           -- DaemonRequest::Authenticate variant index
///   varint(user.len())  -- string length
///   user bytes          -- UTF-8 string data
fn build_auth_request(user: &str) -> Vec<u8> {
    let user_bytes = user.as_bytes();
    // Max size: 1 (variant) + 9 (varint len) + user_bytes.len()
    let mut buf = vec![0u8; 1 + 9 + user_bytes.len()];
    let mut pos = 0;

    // Variant index 0 = Authenticate
    pos += encode_varint(0, &mut buf[pos..]);
    // String length
    pos += encode_varint(user_bytes.len() as u64, &mut buf[pos..]);
    // String data
    buf[pos..pos + user_bytes.len()].copy_from_slice(user_bytes);
    pos += user_bytes.len();

    buf.truncate(pos);
    buf
}

/// Send a length-prefixed message over the socket.
fn send_message(stream: &mut UnixStream, data: &[u8]) -> Result<(), String> {
    let len = data.len() as u32;
    stream
        .write_all(&len.to_le_bytes())
        .map_err(|e| format!("write length: {e}"))?;
    stream
        .write_all(data)
        .map_err(|e| format!("write payload: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Receive a length-prefixed message from the socket.
fn recv_message(stream: &mut UnixStream) -> Result<Vec<u8>, String> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .map_err(|e| format!("read length: {e}"))?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len > MAX_MESSAGE_SIZE {
        return Err(format!("message too large: {len} bytes"));
    }

    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("read payload: {e}"))?;
    Ok(buf)
}

/// Parsed auth response from daemon.
enum AuthResponse {
    /// Face matched with given similarity score
    Matched { similarity: f32 },
    /// Face not matched
    NoMatch { similarity: f32 },
    /// Daemon returned an error
    Error { message: String },
}

/// Parse a DaemonResponse from bincode bytes.
///
/// We only care about two response variants:
///   variant 0 = AuthResult(MatchResult { matched: bool, model_id: Option<u32>, label: Option<String>, similarity: f32 })
///   variant 6 = Error { message: String }
fn parse_auth_response(data: &[u8]) -> Result<AuthResponse, String> {
    if data.is_empty() {
        return Err("empty response".to_string());
    }

    let (variant, mut pos) = decode_varint(data).ok_or("invalid variant")?;

    match variant {
        0 => {
            // AuthResult(MatchResult)
            // matched: bool (1 byte)
            if pos >= data.len() {
                return Err("truncated: matched".to_string());
            }
            let matched = data[pos] != 0;
            pos += 1;

            // model_id: Option<u32>
            // Option encoding: 0 = None, 1 = Some(value)
            if pos >= data.len() {
                return Err("truncated: model_id option".to_string());
            }
            if data[pos] == 1 {
                // Some - skip the varint value
                pos += 1;
                let (_, consumed) =
                    decode_varint(&data[pos..]).ok_or("truncated: model_id value")?;
                pos += consumed;
            } else {
                pos += 1; // None
            }

            // label: Option<String>
            if pos >= data.len() {
                return Err("truncated: label option".to_string());
            }
            if data[pos] == 1 {
                // Some - skip the string
                pos += 1;
                let (str_len, consumed) =
                    decode_varint(&data[pos..]).ok_or("truncated: label length")?;
                pos += consumed;
                pos += str_len as usize; // skip string bytes
            } else {
                pos += 1; // None
            }

            // similarity: f32 (4 bytes LE)
            if pos + 4 > data.len() {
                return Err("truncated: similarity".to_string());
            }
            let similarity =
                f32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);

            if matched {
                Ok(AuthResponse::Matched { similarity })
            } else {
                Ok(AuthResponse::NoMatch { similarity })
            }
        }
        6 => {
            // Error { message: String }
            let (str_len, consumed) =
                decode_varint(&data[pos..]).ok_or("truncated: error message length")?;
            pos += consumed;
            let str_len = str_len as usize;
            if pos + str_len > data.len() {
                return Err("truncated: error message".to_string());
            }
            let message =
                String::from_utf8_lossy(&data[pos..pos + str_len]).to_string();
            Ok(AuthResponse::Error { message })
        }
        _ => Err(format!("unexpected response variant: {variant}")),
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

fn load_config() -> Result<PamConfig, String> {
    let config_path = std::env::var("VISAGE_CONFIG")
        .unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());

    let content =
        std::fs::read_to_string(&config_path).map_err(|e| format!("read config: {e}"))?;

    toml::from_str(&content).map_err(|e| format!("parse config: {e}"))
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

    // 4. Connect to daemon socket
    let socket_path = &config.daemon.socket_path;
    let stream = UnixStream::connect(socket_path);
    let mut stream = match stream {
        Ok(s) => s,
        Err(e) => {
            log_auth(
                &service,
                &format!("error: daemon_unavailable: {e}"),
                &user,
                LOG_WARNING,
            );
            return PAM_IGNORE;
        }
    };

    // 5. Set socket timeouts
    // Total timeout = recognition timeout + 2s buffer
    let timeout_secs = config.recognition.timeout_secs as u64 + CONNECT_TIMEOUT_SECS;
    let timeout = Duration::from_secs(timeout_secs);
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        log_auth(&service, "error: set_timeout", &user, LOG_WARNING);
        return PAM_IGNORE;
    }

    // 6. Send Authenticate request
    let request_data = build_auth_request(&user);
    if let Err(e) = send_message(&mut stream, &request_data) {
        log_auth(
            &service,
            &format!("error: send: {e}"),
            &user,
            LOG_WARNING,
        );
        return PAM_IGNORE;
    }

    // 7. Read and parse response
    let response_data = match recv_message(&mut stream) {
        Ok(d) => d,
        Err(e) => {
            // Check if this was a timeout
            if e.contains("timed out") || e.contains("WouldBlock") {
                log_auth(&service, "timeout", &user, LOG_WARNING);
                return PAM_AUTH_ERR;
            }
            log_auth(
                &service,
                &format!("error: recv: {e}"),
                &user,
                LOG_WARNING,
            );
            return PAM_IGNORE;
        }
    };

    match parse_auth_response(&response_data) {
        Ok(AuthResponse::Matched { similarity }) => {
            log_auth(
                &service,
                &format!("success (similarity={similarity:.3})"),
                &user,
                LOG_INFO,
            );
            PAM_SUCCESS
        }
        Ok(AuthResponse::NoMatch { similarity }) => {
            log_auth(
                &service,
                &format!("no_match (similarity={similarity:.3})"),
                &user,
                LOG_INFO,
            );
            PAM_AUTH_ERR
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
            log_auth(
                &service,
                &format!("error: parse: {e}"),
                &user,
                LOG_WARNING,
            );
            PAM_IGNORE
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
    fn test_encode_varint_small() {
        let mut buf = [0u8; 9];
        assert_eq!(encode_varint(0, &mut buf), 1);
        assert_eq!(buf[0], 0);

        assert_eq!(encode_varint(42, &mut buf), 1);
        assert_eq!(buf[0], 42);

        assert_eq!(encode_varint(250, &mut buf), 1);
        assert_eq!(buf[0], 250);
    }

    #[test]
    fn test_encode_varint_medium() {
        let mut buf = [0u8; 9];
        assert_eq!(encode_varint(251, &mut buf), 3);
        assert_eq!(buf[0], 251);
        assert_eq!(u16::from_le_bytes([buf[1], buf[2]]), 251);

        assert_eq!(encode_varint(1000, &mut buf), 3);
        assert_eq!(buf[0], 251);
        assert_eq!(u16::from_le_bytes([buf[1], buf[2]]), 1000);
    }

    #[test]
    fn test_decode_varint_roundtrip() {
        for &val in &[0u64, 1, 42, 250, 251, 1000, 65535, 65536, 1_000_000] {
            let mut buf = [0u8; 9];
            let written = encode_varint(val, &mut buf);
            let (decoded, consumed) = decode_varint(&buf).unwrap();
            assert_eq!(decoded, val, "roundtrip failed for {val}");
            assert_eq!(consumed, written, "consumed mismatch for {val}");
        }
    }

    #[test]
    fn test_build_auth_request() {
        let data = build_auth_request("alice");
        // Expected: 00 05 61 6c 69 63 65
        assert_eq!(data, vec![0x00, 0x05, 0x61, 0x6c, 0x69, 0x63, 0x65]);
    }

    #[test]
    fn test_parse_auth_response_matched() {
        // AuthResult(matched=true, model_id=Some(42), label=Some("office"), similarity=0.87)
        // Manually constructed from known bincode output
        let data = vec![
            0x00, // variant 0: AuthResult
            0x01, // matched = true
            0x01, 0x2a, // model_id = Some(42)
            0x01, 0x06, 0x6f, 0x66, 0x66, 0x69, 0x63, 0x65, // label = Some("office")
            0x52, 0xb8, 0x5e, 0x3f, // similarity = 0.87 (f32 LE)
        ];
        match parse_auth_response(&data).unwrap() {
            AuthResponse::Matched { similarity } => {
                assert!((similarity - 0.87).abs() < 1e-5);
            }
            _ => panic!("expected Matched"),
        }
    }

    #[test]
    fn test_parse_auth_response_no_match() {
        // AuthResult(matched=false, model_id=None, label=None, similarity=0.2)
        let data = vec![
            0x00, // variant 0: AuthResult
            0x00, // matched = false
            0x00, // model_id = None
            0x00, // label = None
            0xcd, 0xcc, 0x4c, 0x3e, // similarity = 0.2 (f32 LE)
        ];
        match parse_auth_response(&data).unwrap() {
            AuthResponse::NoMatch { similarity } => {
                assert!((similarity - 0.2).abs() < 1e-5);
            }
            _ => panic!("expected NoMatch"),
        }
    }

    #[test]
    fn test_parse_auth_response_error() {
        // Error { message: "rate_limited" }
        let data = vec![
            0x06, // variant 6: Error
            0x0c, // string length 12
            0x72, 0x61, 0x74, 0x65, 0x5f, 0x6c, 0x69, 0x6d, 0x69, 0x74, 0x65,
            0x64, // "rate_limited"
        ];
        match parse_auth_response(&data).unwrap() {
            AuthResponse::Error { message } => {
                assert_eq!(message, "rate_limited");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn test_parse_auth_response_empty() {
        assert!(parse_auth_response(&[]).is_err());
    }

    #[test]
    fn test_parse_auth_response_truncated() {
        // Just the variant byte, missing the rest
        assert!(parse_auth_response(&[0x00]).is_err());
    }

    #[test]
    fn test_parse_auth_response_unknown_variant() {
        assert!(parse_auth_response(&[0x09]).is_err());
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
socket_path = "/tmp/test.sock"
"#;
        let config: PamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.socket_path, "/tmp/test.sock");
        assert!(!config.security.disabled);
        assert!(config.security.abort_if_ssh);
        assert!(config.security.abort_if_lid_closed);
        assert_eq!(config.recognition.timeout_secs, DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn test_config_parsing_full() {
        let toml_str = r#"
[daemon]
socket_path = "/run/visage/visage.sock"

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
        assert_eq!(config.daemon.socket_path, "/run/visage/visage.sock");
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
        assert_eq!(config.daemon.socket_path, DEFAULT_SOCKET_PATH);
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
socket_path = "/tmp/test.sock"

[storage]
db_path = "/tmp/test.db"
"#;
        let config: PamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.socket_path, "/tmp/test.sock");
    }
}
