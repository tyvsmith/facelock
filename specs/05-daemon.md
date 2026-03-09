# Spec 05: Daemon

**Phase**: 3 (Integration) | **Crate**: howdy-daemon | **Depends on**: 02, 03, 04

## Goal

Persistent daemon that owns the camera, ONNX models, and database. Serves authentication and enrollment requests over Unix socket IPC. This is the integration point for all Phase 2 components.

## Dependencies

- `howdy-core` (config, types, IPC protocol)
- `howdy-camera` (V4L2 capture)
- `howdy-face` (ONNX inference)
- `howdy-store` (SQLite storage)
- `signal-hook` (SIGTERM/SIGINT handling)
- `tracing`, `tracing-subscriber` (structured logging)

## Architecture

Single-threaded, synchronous request/response server. Rationale:
- PAM requests are sequential (one auth at a time)
- Camera is a shared resource (can't open twice)
- ONNX sessions are not thread-safe by default
- Simplicity: no async runtime, no channel coordination

## Modules

### `main.rs` -- Entry Point

```rust
fn main() {
    // 1. Parse args (--config <path>)
    // 2. Load config (HOWDY_CONFIG env var or /etc/howdy/config.toml)
    // 3. Init tracing (stderr + syslog)
    // 4. Open camera
    // 5. Load ONNX models (FaceEngine)
    // 6. Open database (FaceStore)
    // 7. Create Unix socket at daemon.socket_path
    // 8. Set socket permissions (0o660)
    // 9. Register signal handlers (SIGTERM, SIGINT)
    // 10. Accept loop: handle one connection at a time
    // 11. On signal: clean up socket file, exit
}
```

### `handler.rs` -- Request Dispatch

```rust
pub struct Handler {
    camera: Camera,
    engine: FaceEngine,
    store: FaceStore,
    config: Config,
}

impl Handler {
    pub fn handle(&mut self, request: DaemonRequest) -> DaemonResponse;
}
```

Request routing:
- `Ping` -> `Ok`
- `Shutdown` -> `Ok` (then exit)
- `Authenticate { user }` -> `auth::authenticate()`
- `Enroll { user, label }` -> `enroll::enroll()`
- `ListModels { user }` -> query store, return `Models`
- `RemoveModel { user, model_id }` -> delete from store, return `Removed`
- `ClearModels { user }` -> clear store, return `Removed`
- `PreviewFrame` -> capture frame, encode JPEG, return `Frame`

### `auth.rs` -- Authentication Flow

```rust
pub fn authenticate(
    camera: &mut Camera,
    engine: &FaceEngine,
    store: &FaceStore,
    config: &Config,
    user: &str,
) -> DaemonResponse;
```

**Authentication algorithm**:
1. Check pre-conditions:
   - `security.disabled` -> Error("howdy is disabled")
   - `is_ssh_session()` && `abort_if_ssh` -> Error("SSH session detected")
   - `is_lid_closed()` && `abort_if_lid_closed` -> Error("lid closed")
   - `!store.has_models(user)` -> AuthResult { matched: false, similarity: 0.0 }
   - Rate limiter check: if user exceeded `max_attempts` in `window_secs` -> Error("rate limited")
2. **Anti-spoofing checks** (see `docs/security.md`):
   - If `security.require_ir` && camera is not IR -> Error("IR camera required")
3. Load user embeddings from store
4. Deadline = now + `recognition.timeout_secs`
5. Collect matched embeddings across frames (for variance check)
6. Loop until deadline:
   a. Capture frame from camera
   b. Skip dark frames (increment dark_count)
   c. Run face engine: detect -> align -> embed
   d. If no face detected: continue
   e. **IR texture check** (if enabled): verify face region has sufficient texture variance in grayscale channel. Reject flat/uniform surfaces (std_dev < 10.0 in face bbox region).
   f. For each detected face (sorted by confidence):
      - Compare embedding against all stored embeddings (cosine similarity)
      - If best similarity >= threshold: add to matched_embeddings list
   g. Track best_similarity across all frames
   h. **Frame variance check** (if `security.require_frame_variance`):
      - Once `matched_embeddings.len() >= security.min_auth_frames`:
      - Verify consecutive matched embeddings have similarity < 0.995 (real faces have micro-movement)
      - If variance passes: return AuthResult { matched: true, ... }
      - If all frames too similar (static image): reject, continue waiting
7. If all frames were dark: return Error("all frames dark")
8. Timeout: return AuthResult { matched: false, similarity: best_similarity }
9. **Log result** via tracing: user, outcome, similarity, frame count, duration

**Helper functions**:
```rust
fn is_ssh_session() -> bool {
    std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok()
}

fn is_lid_closed() -> bool {
    std::fs::read_to_string("/proc/acpi/button/lid/LID0/state")
        .map(|s| s.contains("closed"))
        .unwrap_or(false)
}
```

### `enroll.rs` -- Enrollment Flow

```rust
pub fn enroll(
    camera: &mut Camera,
    engine: &FaceEngine,
    store: &FaceStore,
    user: &str,
    label: &str,
) -> DaemonResponse;
```

**Enrollment algorithm**:
1. Capture frames over 3-second window
2. For each frame:
   - Skip dark frames
   - Run face detection
   - Reject frames with 0 or 2+ faces (exactly 1 required)
   - Select largest face (max bbox area)
   - Run alignment + embedding
   - Store embedding
3. Accept 3-10 face captures (with 200ms delay between for varied angles)
4. Store via `store.add_model(user, label, embedding)`
5. Return Enrolled { model_id, embedding_count }

## Socket Management

- Create parent directory if missing (`/run/howdy/`)
- Remove stale socket file before binding
- Set permissions: `std::fs::set_permissions(path, Permissions::from_mode(0o660))`
- Set ownership: `nix::unistd::chown(path, Some(root), Some(howdy_gid))`
- Non-blocking accept with timeout for clean shutdown handling
- **Peer credential verification**: on every connection, call `getsockopt(SO_PEERCRED)` and verify UID is root (0) or in howdy group. Reject unauthorized connections immediately.
- **Message size limit**: reject messages > 10MB in `recv_message()`

## Rate Limiting

Track auth attempts per user:
- Default: 5 attempts per user per 60 seconds
- Configurable via `security.rate_limit.max_attempts` and `security.rate_limit.window_secs`
- Rate-limited attempts return Error("rate limited") immediately
- Log rate-limited attempts

## Signal Handling

```rust
let term = Arc::new(AtomicBool::new(false));
signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term));
signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term));

// In accept loop:
while !term.load(Ordering::Relaxed) {
    // non-blocking accept with 1s timeout
}
// Cleanup: remove socket file
```

## Tests

- Handler dispatches correct response for each request type
- Auth returns no-match for empty store
- Auth returns match for stored embedding (mock camera/engine)
- Enroll stores and returns model ID
- SSH detection with mocked env vars
- Signal handling: set flag, verify clean exit
- Socket creation and permission setting

Note: Full daemon integration tests are in spec 12. This spec focuses on unit-testable logic.

## Acceptance Criteria

1. Daemon starts, creates socket, handles Ping
2. Auth flow: capture -> detect -> compare -> respond
3. Enroll flow: capture -> store -> respond
4. ListModels, RemoveModel, ClearModels work
5. PreviewFrame returns JPEG data
6. Graceful shutdown on SIGTERM/SIGINT
7. Socket file cleaned up on exit
8. Returns appropriate errors for edge cases

## Verification

```bash
cargo build -p howdy-daemon
cargo test -p howdy-daemon
cargo clippy -p howdy-daemon
```
