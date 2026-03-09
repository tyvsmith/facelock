# Spec 12: Integration Tests

**Phase**: 6 (Validation) | **Depends on**: all prior

## Goal

Comprehensive integration testing across all tiers. Validates the full system works end-to-end, with special emphasis on PAM safety.

## Test Categories

### IPC Round-Trip Tests (howdy-core)

Location: `crates/howdy-core/tests/ipc_integration.rs`

- Serialize/deserialize all DaemonRequest variants
- Serialize/deserialize all DaemonResponse variants
- Large payload: PreviewFrame with JPEG data
- Timeout: socket pair without send, verify recv_message times out
- Malformed: truncated message, corrupt bincode

### Store Lifecycle Tests (howdy-store)

Location: `crates/howdy-store/tests/store_integration.rs`

- Full lifecycle: add -> list -> retrieve -> remove
- Multi-user: alice and bob with separate models
- Embedding round-trip: store -> retrieve -> bit-exact comparison
- Concurrent access: two FaceStore instances on same file
- Schema migration: open old DB, verify migration runs

### Face Engine Tests (howdy-face, #[ignore])

Location: `crates/howdy-face/tests/engine_integration.rs`

Requires downloaded ONNX models + test images in `tests/fixtures/`.

- Load SCRFD model, detect faces in test image
- Load ArcFace model, extract embedding from aligned face
- Same-person similarity > 0.5
- Different-person similarity < 0.3
- Embedding L2 norm = 1.0
- NMS produces fewer detections than raw output

### Daemon Integration Tests (howdy-daemon, #[ignore])

Location: `crates/howdy-daemon/tests/daemon_integration.rs`

- Start test daemon (temp dir for socket, DB)
- Ping -> Ok
- Enroll -> Enrolled with model ID
- ListModels -> Models with enrolled model
- Authenticate -> AuthResult
- RemoveModel -> Removed
- Shutdown -> Ok, daemon exits

### CLI Smoke Tests (howdy-cli)

Location: `crates/howdy-cli/tests/cli_smoke.rs`

- `howdy --help` exits 0
- `howdy --version` exits 0
- `howdy status` runs without daemon (reports daemon as down)
- `howdy devices` runs without camera (may show empty list)
- `howdy config` shows config path

### PAM Module Verification

Automated checks (in CI):
```bash
# Symbol exports
nm -D target/release/libpam_howdy.so | grep -q pam_sm_authenticate
nm -D target/release/libpam_howdy.so | grep -q pam_sm_setcred

# Size check
size=$(stat -c%s target/release/libpam_howdy.so)
[ "$size" -lt 1048576 ]  # < 1MB

# Dependency check (should be minimal)
ldd target/release/libpam_howdy.so
```

### PAM Container Tests

Container: `test/Containerfile`

```dockerfile
FROM archlinux:latest
RUN pacman -Syu --noconfirm pam pamtester sudo
RUN useradd -m testuser && echo "testuser:test" | chpasswd

# Copy built artifacts
COPY target/release/libpam_howdy.so /lib/security/pam_howdy.so
COPY target/release/howdy-daemon /usr/bin/howdy-daemon
COPY target/release/howdy /usr/bin/howdy
COPY test/pam.d/howdy-test /etc/pam.d/howdy-test
COPY dev/config.toml /etc/howdy/config.toml

# Test script
COPY test/run-container-tests.sh /run-tests.sh
RUN chmod +x /run-tests.sh
CMD ["/run-tests.sh"]
```

Container test cases:
1. Module loads: `pamtester howdy-test testuser authenticate` returns (doesn't crash)
2. PAM_IGNORE when daemon not running: exit code indicates fallthrough
3. Missing config: returns PAM_IGNORE
4. Disabled config: returns PAM_IGNORE
5. With daemon running: Ping works, auth attempted

Test PAM config (`test/pam.d/howdy-test`):
```
auth  sufficient  pam_howdy.so
auth  required    pam_permit.so
```

## Files to Create

- `test/Containerfile`
- `test/run-tests.sh` (CI script, Tier 1 + build + PAM symbol check)
- `test/run-container-tests.sh` (container PAM tests)
- `test/pam.d/howdy-test`
- `tests/fixtures/` (test images for face engine tests)

## Acceptance Criteria

1. `cargo test --workspace` passes all non-ignored tests
2. `cargo clippy --workspace -- -D warnings` clean
3. IPC round-trip tests pass
4. Store lifecycle tests pass
5. CLI smoke tests pass
6. PAM module exports correct symbols, size < 1MB
7. PAM container tests pass (module loads, returns PAM_IGNORE when daemon down)
8. All ignored tests documented with hardware requirements

## Verification

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
bash test/run-tests.sh
podman build -f test/Containerfile -t howdy-test . && podman run howdy-test
```
