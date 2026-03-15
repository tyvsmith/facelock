# Testing & Safety Strategy

**READ THIS BEFORE implementing anything PAM-related.**

## The Golden Rule

**Never install `pam_facelock.so` on the host or edit `/etc/pam.d/*` until validated in container.** A broken PAM module can lock you out of sudo, login, and su.

## Testing Tiers

### Tier 1: Unit Tests (always safe)

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Covers: config parsing, format conversion, NMS, cosine similarity, IPC serialization, SQLite CRUD, frame variance logic. No hardware, no root.

### Tier 2: Hardware Integration (camera + models)

```bash
cargo test --workspace -- --ignored
```

Requires camera and downloaded ONNX models. Tests capture, model loading, full pipeline.

### Tier 3: Container Tests (requires podman)

```bash
just test-pam             # PAM smoke tests (no camera needed)
just test-integration     # full E2E with camera (daemon mode)
just test-oneshot         # full E2E with camera (no daemon)
just test-shell           # interactive shell for manual testing
```

Container tests validate:
- PAM module loads without crashing
- Returns PAM_IGNORE when daemon unavailable
- Handles missing/invalid config
- Exports correct PAM symbols
- End-to-end: enroll → list → test → PAM auth → clear
- Both daemon and oneshot modes

### Tier 4: VM Testing (optional, recommended)

Disposable VM with snapshots. USB camera passthrough for real hardware testing. Verify sudo, su, login scenarios with rollback safety.

### Tier 5: Host PAM Installation

Safety checklist:
1. Open root shell in separate terminal — **keep it open**
2. `sudo cp /etc/pam.d/sudo /etc/pam.d/sudo.bak`
3. `sudo facelock setup --pam --service sudo`
4. Test in NEW terminal: `sudo echo test`
5. If broken, revert from root shell: `sudo cp /etc/pam.d/sudo.bak /etc/pam.d/sudo`
6. **Never** modify `system-auth` or `login` until sudo works perfectly

Emergency recovery: boot from USB, mount partition, remove PAM line, reboot.

## Development Workflow

### Setup
```bash
export FACELOCK_CONFIG=dev/config.toml
cargo build --workspace
cargo run --bin facelock -- setup       # download models
```

### No-Daemon Development
All CLI commands work without a daemon — the CLI falls back to direct mode silently:
```bash
facelock enroll
facelock test
facelock list
facelock devices
```

### Daemon Development
```bash
facelock daemon &
facelock enroll       # uses daemon (faster for repeated commands)
facelock test
kill %1             # stop daemon
```

### Logging
Control via `RUST_LOG` environment variable:
```bash
RUST_LOG=facelock_daemon=debug facelock daemon    # verbose daemon
RUST_LOG=facelock_cli=debug facelock test         # verbose CLI
```

## Dev Config

`dev/config.toml` — temp paths, no root, camera auto-detected:

```toml
[device]
max_height = 480

[daemon]
model_dir = "./models"

[storage]
db_path = "/tmp/facelock-dev.db"

[security]
require_ir = true
require_frame_variance = true
```

## CI

GitHub Actions at `.github/workflows/ci.yml`:
- Build + test + clippy + fmt check
- Container PAM smoke tests

Local full CI: `bash test/run-tests.sh`

## Test Files

| File | Purpose |
|------|---------|
| `test/Containerfile` | Container image (Arch + pamtester) |
| `test/run-tests.sh` | CI script (unit + lint + PAM symbols) |
| `test/run-container-tests.sh` | PAM smoke tests |
| `test/run-integration-tests.sh` | E2E with camera (daemon) |
| `test/run-oneshot-tests.sh` | E2E with camera (oneshot) |
| `test/pam.d/facelock-test` | Test PAM config |

## Just Recipes

| Recipe | Description |
|--------|-------------|
| `just test` | Unit tests |
| `just lint` | Clippy |
| `just check` | test + lint + fmt |
| `just test-pam` | Container PAM smoke |
| `just test-integration` | E2E daemon mode |
| `just test-oneshot` | E2E oneshot mode |
| `just test-shell` | Interactive container |
| `just install` | System install (root) |
