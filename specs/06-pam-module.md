# Spec 06: PAM Module

**Phase**: 4 (Interfaces) | **Crate**: pam-visage | **Depends on**: 01 (IPC protocol) | **Parallel with**: 07

## Goal

Minimal PAM shared library that connects to the daemon via Unix socket IPC and returns appropriate PAM codes. This is the security-critical component -- it must never crash, never block indefinitely, and always fail gracefully.

## Dependencies

**STRICT**: `libc`, `toml`, `serde` ONLY. No visage-core, no ort, no v4l, no rusqlite.

The PAM module must stay tiny (~200KB). It reads a minimal config subset and speaks the IPC protocol directly (reimplemented in ~50 lines, not imported from visage-core).

## Source File

Single file: `src/lib.rs`

## Design Constraints

1. **Never panic**: Use `catch_unwind` at FFI boundary
2. **Never block indefinitely**: All socket operations have timeouts
3. **Always fail gracefully**: Any error -> PAM_IGNORE (fall through to password)
4. **Minimal surface**: No heavy dependencies that could crash in PAM context
5. **Thread-safe**: PAM may call from multiple threads

## Implementation

### Exported Functions

```rust
/// Main authentication entry point
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut libc::c_void,
    flags: libc::c_int,
    argc: libc::c_int,
    argv: *const *const libc::c_char,
) -> libc::c_int;

/// Credential management (no-op)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut libc::c_void,
    _flags: libc::c_int,
    _argc: libc::c_int,
    _argv: *const *const libc::c_char,
) -> libc::c_int {
    0  // PAM_SUCCESS
}
```

### Authentication Flow

```rust
fn identify(pamh: *mut libc::c_void) -> libc::c_int {
    // 0. Get PAM service name (e.g. "sudo", "login") for logging
    //    Also check against allowed/denied service lists if configured

    // 1. Read minimal config (socket_path, disabled, abort_if_ssh, abort_if_lid_closed)
    //    Error -> log + return 25 (PAM_IGNORE)

    // 2. Pre-flight checks:
    //    disabled = true -> log "disabled" + return 25
    //    is_ssh_session() && abort_if_ssh -> log "ssh_abort" + return 25
    //    is_lid_closed() && abort_if_lid_closed -> log "lid_closed" + return 25

    // 3. Get PAM username via pam_get_user()
    //    Error -> log + return 25

    // 4. Connect to daemon socket (timeout: 2s)
    //    Error -> log "daemon_unavailable" + return 25

    // 5. Set socket read/write timeouts (recognition.timeout_secs + 2s)

    // 6. Send Authenticate { user } request

    // 7. Read response:
    //    AuthResult { matched: true } -> log "success" + return 0 (PAM_SUCCESS)
    //    AuthResult { matched: false } -> log "no_match" + return 7 (PAM_AUTH_ERR)
    //    Error -> log "error: <msg>" + return 25 (PAM_IGNORE)
    //    Timeout -> log "timeout" + return 7 (PAM_AUTH_ERR)

    // ALL outcomes logged to syslog: pam_visage(<service>): <result> for user <username>
}
```

### PAM Constants

```rust
const PAM_SUCCESS: libc::c_int = 0;
const PAM_AUTH_ERR: libc::c_int = 7;
const PAM_IGNORE: libc::c_int = 25;
```

### Config Subset (Minimal, Inline)

```rust
#[derive(Deserialize)]
struct PamConfig {
    #[serde(default)]
    daemon: PamDaemonConfig,
    #[serde(default)]
    security: PamSecurityConfig,
    #[serde(default)]
    recognition: PamRecognitionConfig,
}

#[derive(Deserialize, Default)]
struct PamDaemonConfig {
    #[serde(default = "default_socket")]
    socket_path: String,
}

#[derive(Deserialize, Default)]
struct PamSecurityConfig {
    #[serde(default)]
    disabled: bool,
    #[serde(default = "default_true")]
    abort_if_ssh: bool,
    #[serde(default = "default_true")]
    abort_if_lid_closed: bool,
    #[serde(default = "default_true")]
    detection_notice: bool,
}

#[derive(Deserialize, Default)]
struct PamRecognitionConfig {
    #[serde(default = "default_timeout")]
    timeout_secs: u32,
}
```

### IPC (Inline, ~50 lines)

Reimplement the length-prefixed bincode protocol directly:

```rust
fn send_auth_request(stream: &mut UnixStream, user: &str) -> Result<(), ()> {
    // Manually construct the Authenticate request as bincode
    // Send 4-byte LE length prefix + payload
}

fn recv_auth_response(stream: &mut UnixStream) -> Result<(bool, f32), ()> {
    // Read 4-byte LE length
    // Read payload
    // Parse AuthResult: extract matched bool and similarity f32
}
```

### Helper Functions

```rust
fn is_ssh_session() -> bool {
    // Check /proc/self/environ for SSH_CONNECTION or SSH_TTY
    std::fs::read("/proc/self/environ")
        .map(|data| {
            data.split(|&b| b == 0)
                .any(|var| var.starts_with(b"SSH_CONNECTION=") || var.starts_with(b"SSH_TTY="))
        })
        .unwrap_or(false)
}

fn is_lid_closed() -> bool {
    std::fs::read_to_string("/proc/acpi/button/lid/LID0/state")
        .map(|s| s.contains("closed"))
        .unwrap_or(false)
}

fn pam_get_user(pamh: *mut libc::c_void) -> Option<String> {
    // Call pam_get_user via FFI
    extern "C" { fn pam_get_user(...) -> libc::c_int; }
    // ...
}
```

## Build Configuration

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
libc = "0.2"
toml = "0.8"
serde = { version = "1", features = ["derive"] }
```

## Tests

- Build verification: `cargo build -p pam-visage --release`
- Symbol check: `nm -D target/release/libpam_visage.so | grep pam_sm_authenticate`
- Size check: `stat -c%s target/release/libpam_visage.so` (should be < 500KB)
- Dependency check: `ldd target/release/libpam_visage.so` (minimal deps)
- Container tests: see spec 12 and `docs/testing-safety.md`

## Audit Logging

The PAM module must log every authentication attempt to syslog (LOG_AUTH facility):

```rust
use libc::{openlog, syslog, closelog, LOG_AUTH, LOG_INFO, LOG_WARNING};

fn log_auth(service: &str, result: &str, user: &str) {
    // Writes to /var/log/auth.log or journald
    // Format: pam_visage(sudo): success for user ty
    unsafe {
        let ident = std::ffi::CString::new("pam_visage").unwrap();
        openlog(ident.as_ptr(), 0, LOG_AUTH);
        let msg = std::ffi::CString::new(
            format!("pam_visage({}): {} for user {}", service, result, user)
        ).unwrap();
        syslog(LOG_INFO, msg.as_ptr());
        closelog();
    }
}
```

## Acceptance Criteria

1. Builds as cdylib, exports `pam_sm_authenticate` and `pam_sm_setcred`
2. Binary size < 500KB
3. Returns PAM_IGNORE when daemon not running
4. Returns PAM_SUCCESS on face match (container test)
5. Returns PAM_AUTH_ERR on face mismatch (container test)
6. All socket operations have timeouts
7. No panics (catch_unwind wrapper)
8. No heavy dependencies (no ort, no v4l, no rusqlite)
9. All auth attempts logged to syslog with service name, result, and username
10. Syslog entries visible in `journalctl -t pam_visage`

## Verification

```bash
cargo build -p pam-visage --release
nm -D target/release/libpam_visage.so | grep pam_sm
stat -c%s target/release/libpam_visage.so
```
