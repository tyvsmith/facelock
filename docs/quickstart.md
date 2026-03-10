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
VISAGE_CONFIG=dev/config.toml cargo run --bin visage -- setup
```

**Option B: Manual download**
```bash
mkdir -p models
curl -L -o models/scrfd_2.5g_bnkps.onnx \
  "https://github.com/visomaster/visomaster-assets/releases/download/v0.1.0/scrfd_2.5g_bnkps.onnx"
curl -L -o models/w600k_r50.onnx \
  "https://github.com/visomaster/visomaster-assets/releases/download/v0.1.0/w600k_r50.onnx"
```

## 3. Configure for Development

The repo includes `dev/config.toml` which uses temp paths and auto-detects the camera. Set `VISAGE_CONFIG` in your shell:

```bash
export VISAGE_CONFIG=dev/config.toml
```

Camera auto-detection will find your device. To check which camera was detected:
```bash
cargo run --bin visage -- devices
```

## 4. Choose a Mode

### Option A: Daemon Mode (default)

Start the daemon, then use the CLI:

```bash
cargo run --bin visage-daemon &
cargo run --bin visage -- status
cargo run --bin visage -- enroll
cargo run --bin visage -- test
kill %1  # stop daemon
```

### Option B: Oneshot Mode (no daemon)

Set `daemon.mode = "oneshot"` in `dev/config.toml`, then use the CLI directly — no daemon needed:

```bash
cargo run --bin visage -- devices
cargo run --bin visage -- enroll
cargo run --bin visage -- test
cargo run --bin visage -- list
```

Every command opens the camera and models directly. Slightly slower (~700ms startup per command) but zero background processes.

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

### Tier 3: Container Tests (requires podman)

```bash
# PAM smoke tests (no camera needed)
just test-pam

# End-to-end with camera (daemon mode)
just test-integration

# End-to-end with camera (oneshot mode, no daemon)
just test-oneshot

# Interactive shell for manual pamtester
just test-shell
```

### Full CI Script

```bash
just check           # test + clippy + fmt
bash test/run-tests.sh  # full CI with PAM checks
```

### Benchmarks

```bash
cargo run --bin visage-bench -- model-load    # ONNX load time
cargo run --bin visage-bench -- report        # full benchmark report
```

## Installing on Your System (PAM)

**Read `docs/testing-safety.md` first.** A broken PAM module can lock you out.

### Arch Linux (PKGBUILD)

```bash
cd dist && makepkg -si
visage setup
visage enroll
visage test

# Daemon mode with socket activation:
sudo systemctl enable --now visage-daemon.socket

# Or oneshot mode (edit /etc/visage/config.toml):
#   [daemon]
#   mode = "oneshot"
```

### Manual Install

```bash
sudo -i  # keep this root shell open the entire time

cargo build --workspace --release

# Install binaries
sudo install -m 755 target/release/visage /usr/bin/visage
sudo install -m 755 target/release/visage-daemon /usr/bin/visage-daemon
sudo install -m 755 target/release/visage-auth /usr/bin/visage-auth
sudo install -m 755 target/release/libpam_visage.so /lib/security/pam_visage.so

# Create directories and config
sudo mkdir -p /etc/visage /var/lib/visage/models /run/visage /var/log/visage/snapshots
sudo cp config/visage.toml /etc/visage/config.toml

# Copy models
sudo cp models/*.onnx /var/lib/visage/models/

# Install systemd units (if using daemon mode)
sudo cp systemd/visage-daemon.service /usr/lib/systemd/system/
sudo cp systemd/visage-daemon.socket /usr/lib/systemd/system/
sudo systemctl enable --now visage-daemon.socket

# Back up and edit PAM config
sudo cp /etc/pam.d/sudo /etc/pam.d/sudo.bak
# Add to TOP of /etc/pam.d/sudo:
#   auth  sufficient  pam_visage.so

# Test in a NEW terminal (keep root shell open!)
sudo echo "it works"
```

**Never** edit `/etc/pam.d/system-auth` or `/etc/pam.d/login` until sudo works perfectly.

## Dev Config Reference

`dev/config.toml` uses these paths (no root needed):

| What | Path |
|------|------|
| Config | `dev/config.toml` (via `VISAGE_CONFIG`) |
| Camera | Auto-detected |
| Socket | `/tmp/visage-dev.sock` |
| Database | `/tmp/visage-dev.db` |
| Models | `./models/` |
| Snapshots | `/tmp/visage-dev-snapshots/` |
