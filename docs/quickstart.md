# Quickstart

## Prerequisites

- Rust 1.85+ (`rustup update`)
- A webcam (IR camera recommended for production; RGB works for development)
- Linux with V4L2 support

## 1. Build

```bash
cargo build --workspace
```

## 2. Download Models

Models are ~170MB total and gitignored. Two options:

**Option A: Use the CLI** (downloads automatically)
```bash
HOWDY_CONFIG=dev/config.toml cargo run --bin howdy -- setup
```

**Option B: Manual download**
```bash
mkdir -p models
curl -L -o models/scrfd_2.5g_bnkps.onnx \
  "https://github.com/visomaster/visomaster-assets/releases/download/v0.1.0/scrfd_2.5g_bnkps.onnx"
curl -L -o models/w600k_r50.onnx \
  "https://github.com/visomaster/visomaster-assets/releases/download/v0.1.0/w600k_r50.onnx"
```

Verify checksums:
```bash
sha256sum models/*.onnx
# scrfd_2.5g_bnkps.onnx: bc24bb349491481c3ca793cf89306723162c280cb284c5a5e49df3760bf5c2ce
# w600k_r50.onnx:        4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43
```

## 3. Configure for Development

The repo includes `dev/config.toml` which uses temp paths and disables security
checks that require an IR camera. Set `HOWDY_CONFIG` in your shell — every
command (daemon, CLI, bench) reads it:

```bash
export HOWDY_CONFIG=dev/config.toml
```

Find your camera and update `dev/config.toml` if needed:
```bash
cargo run --bin howdy -- devices
# Update device.path in dev/config.toml to match (e.g. /dev/video2)
```

## 4. Run the Daemon

The daemon must be running for the CLI to work. Start it in the background:

```bash
cargo run --bin howdy-daemon &
```

Check it's running:
```bash
cargo run --bin howdy -- status
```

## 5. Enroll and Test

```bash
# Enroll your face (look at the camera)
cargo run --bin howdy -- enroll

# Test recognition
cargo run --bin howdy -- test

# Live camera preview with detection overlay (Wayland)
cargo run --bin howdy -- preview

# List enrolled face models
cargo run --bin howdy -- list
```

Stop the daemon when done:
```bash
kill %1
```

## Testing

### Tier 1: Unit Tests (always safe, no hardware needed)

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

### Tier 2: Hardware Integration Tests (requires camera + models)

```bash
cargo test --workspace -- --ignored
```

### Tier 3: PAM Container Tests (requires podman/docker)

Tests the PAM module inside a container — safe, never touches your host PAM config.

```bash
# Build release first (container copies the binaries)
cargo build --workspace --release

# Build and run the test container
podman build -f test/Containerfile -t howdy-test .
podman run --rm howdy-test
```

This verifies:
- PAM module loads without crashing
- Returns `PAM_IGNORE` when daemon isn't running (graceful fallback)
- Handles missing/invalid config gracefully
- Exports correct PAM symbols (`pam_sm_authenticate`, `pam_sm_setcred`)

### Full CI Script

Runs Tier 1 tests, clippy, release build, and PAM symbol/size checks:

```bash
bash test/run-tests.sh
```

### Benchmarks

```bash
cargo run --bin howdy-bench -- model-load    # ONNX load time
cargo run --bin howdy-bench -- report        # full benchmark report
cargo run --bin howdy-bench -- --help        # all subcommands
```

## Installing on Your System (PAM)

**Read `docs/testing-safety.md` first.** A broken PAM module can lock you out.

The safe progression:

1. **Container tests pass** (Tier 3 above)
2. **VM tests pass** (optional but recommended — test in a disposable VM with snapshots)
3. **Host install** (only after 1 and 2):

```bash
# Keep a root shell open in another terminal the entire time
sudo -i

# Build release
cargo build --workspace --release

# Install binaries
sudo install -m 755 target/release/howdy /usr/bin/howdy
sudo install -m 755 target/release/howdy-daemon /usr/bin/howdy-daemon
sudo install -m 755 target/release/libpam_howdy.so /lib/security/pam_howdy.so

# Create directories and config
sudo mkdir -p /etc/howdy /var/lib/howdy/models /run/howdy /var/log/howdy/snapshots
sudo cp dev/config.toml /etc/howdy/config.toml
# Edit /etc/howdy/config.toml — set device.path, enable security.require_ir, etc.

# Copy models
sudo cp models/*.onnx /var/lib/howdy/models/

# Back up your sudo PAM config
sudo cp /etc/pam.d/sudo /etc/pam.d/sudo.bak

# Add howdy to sudo (ONLY sudo first, never system-auth)
# Add this line at the TOP of /etc/pam.d/sudo:
#   auth  sufficient  pam_howdy.so

# Test in a NEW terminal (keep root shell open!)
sudo echo "it works"

# If anything goes wrong, revert from your root shell:
#   cp /etc/pam.d/sudo.bak /etc/pam.d/sudo
```

**Never** edit `/etc/pam.d/system-auth` or `/etc/pam.d/login` until sudo works
perfectly. Never close your root shell until you've verified everything works.

## Dev Config Reference

`dev/config.toml` uses these paths (no root needed):

| What | Path |
|------|------|
| Config | `dev/config.toml` (via `HOWDY_CONFIG`) |
| Socket | `/tmp/howdy-dev.sock` |
| Database | `/tmp/howdy-dev.db` |
| Models | `./models/` |
| Snapshots | `/tmp/howdy-dev-snapshots/` |

Security checks disabled for development:
- `require_ir = false` — allows RGB webcams
- `require_frame_variance = false` — skips anti-spoofing frame check
