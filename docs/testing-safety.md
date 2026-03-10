# Testing & Safety Strategy

**READ THIS BEFORE implementing anything PAM-related.**

## The Golden Rule

**Never install `pam_visage.so` on the host or edit `/etc/pam.d/*` until validated in both container and VM.** A broken PAM module can lock you out of sudo, login, and su.

## Testing Tiers

### Tier 1: Unit Tests (Host, Always Safe)

Run: `cargo test --workspace`

Covers:
- Config parsing and validation
- YUV/MJPG format conversion
- CLAHE histogram equalization
- NMS and IoU computation
- Similarity transforms (Umeyama alignment)
- L2 normalization and cosine similarity
- IPC serialization round-trip
- SQLite CRUD operations
- Bincode/bytemuck embedding round-trip
- File locking and atomic writes

All pure functions. No hardware, no system state, no root required.

### Tier 2: Integration Tests with Hardware (Host, Marked #[ignore])

Run: `cargo test --workspace -- --ignored`

Requires: camera device, downloaded ONNX models.

Covers:
- Camera capture and format negotiation
- ONNX model loading and inference
- Full detect -> align -> embed pipeline
- End-to-end face matching
- Model download and SHA256 verification

Safe: worst case is camera doesn't open or model file is invalid.

### Tier 3: PAM Module Testing (Container Only)

Build container, install PAM module inside, test with `pamtester`.

```dockerfile
FROM archlinux:latest
RUN pacman -Syu --noconfirm pam pamtester sudo
RUN useradd -m testuser && echo "testuser:test" | chpasswd
COPY target/release/libpam_visage.so /lib/security/pam_visage.so
COPY test/pam.d/visage-test /etc/pam.d/visage-test
```

Test cases:
1. Module loads without crash
2. Returns PAM_IGNORE when daemon not running
3. Returns PAM_IGNORE with missing/invalid config
4. Respects `security.disabled = true`
5. Falls through to password when face fails
6. Works with sudo integration (daemon running in container)

Run: `podman build -f test/Containerfile -t visage-test . && podman run visage-test`

### Tier 4: VM Testing (Disposable VM with Snapshots)

For full end-to-end PAM integration with real camera:
- Use QEMU/virt-manager or systemd-nspawn
- Take snapshot before installing PAM module
- Test sudo, su, and login scenarios
- Verify rollback by restoring snapshot

USB camera passthrough: `qemu -device usb-host,vendorid=0x...,productid=0x...`
Or virtual V4L2 device for deterministic tests.

### Tier 5: Host Installation (After Tier 3 + 4 Pass)

Safety checklist:
1. Open a root shell in a separate terminal (keep it open throughout)
2. Back up: `sudo cp /etc/pam.d/sudo /etc/pam.d/sudo.bak`
3. Add ONLY to `/etc/pam.d/sudo`: `auth sufficient pam_visage.so`
4. Test in a NEW terminal: `sudo echo test`
5. If it hangs or fails, revert with the root shell: `cp /etc/pam.d/sudo.bak /etc/pam.d/sudo`
6. NEVER modify `/etc/pam.d/system-auth` or `/etc/pam.d/login` until sudo works perfectly

Emergency recovery (if locked out):
- Boot from USB, mount partition, remove PAM line, reboot

## Development Safety

### VISAGE_CONFIG Environment Variable

All crates must support `VISAGE_CONFIG` env var, checked before `/etc/visage/config.toml`:

```rust
fn config_path() -> PathBuf {
    std::env::var("VISAGE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/visage/config.toml"))
}
```

### Dev Config

`dev/config.toml` uses local paths (no root required). Camera is auto-detected:

```toml
[device]
# path auto-detected — omit for auto, or set explicitly
max_height = 480

[daemon]
# mode = "daemon"  # or "oneshot" for daemonless development
socket_path = "/tmp/visage-dev.sock"
model_dir = "./models"

[storage]
db_path = "/tmp/visage-dev.db"

[security]
require_ir = true
require_frame_variance = true
```

### Dev Workflow (No Root Required)

**Daemon mode:**
```bash
export VISAGE_CONFIG=dev/config.toml
cargo build --workspace
cargo run --bin visage-daemon &    # starts with dev config
cargo run --bin visage -- setup    # downloads models to ./models
cargo run --bin visage -- enroll   # captures face
cargo run --bin visage -- test     # tests recognition
```

**Oneshot mode (no daemon):**
```bash
export VISAGE_CONFIG=dev/config.toml
# Set daemon.mode = "oneshot" in dev/config.toml
cargo run --bin visage -- setup    # downloads models
cargo run --bin visage -- enroll   # opens camera directly
cargo run --bin visage -- test     # tests recognition directly
```

## CI Test Script

```bash
#!/bin/bash
set -euo pipefail

echo "=== Tier 1: Unit Tests ==="
cargo test --workspace

echo "=== Lint ==="
cargo clippy --workspace -- -D warnings

echo "=== Build Check ==="
cargo build --workspace --release

echo "=== PAM Symbol Verification ==="
nm -D target/release/libpam_visage.so | grep -q pam_sm_authenticate
nm -D target/release/libpam_visage.so | grep -q pam_sm_setcred

echo "=== PAM Size Check ==="
size=$(stat -c%s target/release/libpam_visage.so)
if [ "$size" -gt 1048576 ]; then
    echo "WARNING: PAM module is ${size} bytes (>1MB)"
fi

echo "=== All automated checks passed ==="
```

## Test Files

| File | Purpose |
|------|---------|
| `dev/config.toml` | Development config (auto-detect, temp paths) |
| `test/Containerfile` | PAM container test image |
| `test/run-tests.sh` | CI test script |
| `test/run-container-tests.sh` | Container PAM smoke tests |
| `test/run-integration-tests.sh` | End-to-end daemon mode tests (with camera) |
| `test/run-oneshot-tests.sh` | End-to-end oneshot mode tests (no daemon, with camera) |
| `test/pam.d/visage-test` | Test PAM config for container |

## Just Recipes

| Recipe | Description |
|--------|-------------|
| `just test` | Unit tests |
| `just check` | test + clippy + fmt |
| `just test-pam` | Container PAM smoke tests |
| `just test-integration` | End-to-end with camera (daemon mode) |
| `just test-oneshot` | End-to-end with camera (oneshot mode) |
| `just test-shell` | Interactive container shell |
