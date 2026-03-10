# Quickstart

## Prerequisites

- Rust 1.85+ (`rustup update`)
- Linux with V4L2 support
- A webcam (IR recommended for production; RGB works for development)

## 1. Build

```bash
cargo build --workspace
```

## 2. Download Models

Models are ~170MB total and gitignored.

```bash
VISAGE_CONFIG=dev/config.toml cargo run --bin visage -- setup
```

Or manually:
```bash
mkdir -p models
curl -L -o models/scrfd_2.5g_bnkps.onnx \
  "https://github.com/visomaster/visomaster-assets/releases/download/v0.1.0/scrfd_2.5g_bnkps.onnx"
curl -L -o models/w600k_r50.onnx \
  "https://github.com/visomaster/visomaster-assets/releases/download/v0.1.0/w600k_r50.onnx"
```

## 3. Configure for Development

```bash
export VISAGE_CONFIG=dev/config.toml
```

The dev config auto-detects the camera and uses temp paths. No root needed.

## 4. Enroll and Test

No daemon required — the CLI operates directly when no daemon is running:

```bash
visage devices            # list cameras
visage enroll             # capture face (look at camera)
visage test               # verify recognition
visage list               # see enrolled models
visage preview --text-only  # live detection output
```

To use daemon mode instead:
```bash
visage daemon &           # start daemon in background
visage enroll             # now uses daemon (faster)
visage test
kill %1                   # stop daemon
```

## 5. Testing

### Unit tests (no hardware)
```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

### Hardware tests (camera + models)
```bash
cargo test --workspace -- --ignored
```

### Container tests (requires podman)
```bash
just test-pam             # PAM smoke tests (no camera)
just test-integration     # end-to-end with camera (daemon mode)
just test-oneshot         # end-to-end with camera (no daemon)
just test-shell           # interactive container shell
```

### All checks
```bash
just check                # test + clippy + fmt
```

## 6. System Installation

**Read `docs/testing-safety.md` first.** A broken PAM module can lock you out.

### Arch Linux
```bash
cd dist && makepkg -si
sudo visage setup                    # download models
sudo visage enroll                   # capture face
sudo visage test                     # verify
sudo visage setup --systemd          # enable socket activation
sudo visage setup --pam              # install to /etc/pam.d/sudo
```

### Manual Install
```bash
sudo -i                              # keep this root shell open!

cargo build --workspace --release
sudo install -m 755 target/release/visage /usr/bin/visage
sudo install -m 755 target/release/libpam_visage.so /lib/security/pam_visage.so

sudo mkdir -p /etc/visage /var/lib/visage/models /run/visage
sudo cp config/visage.toml /etc/visage/config.toml
sudo cp models/*.onnx /var/lib/visage/models/

# Socket activation (systemd):
sudo visage setup --systemd

# Or oneshot mode (no daemon):
# Edit /etc/visage/config.toml: daemon.mode = "oneshot"

# PAM (start with sudo only):
sudo visage setup --pam --service sudo

# Test in a NEW terminal:
sudo echo "it works"

# If anything breaks, revert from root shell:
# sudo cp /etc/pam.d/sudo.visage-backup /etc/pam.d/sudo
```

## Dev Config Reference

`dev/config.toml` uses temp paths — no root needed:

| What | Path |
|------|------|
| Camera | Auto-detected |
| Socket | `/tmp/visage-dev.sock` |
| Database | `/tmp/visage-dev.db` |
| Models | `./models/` |
| Snapshots | `/tmp/visage-dev-snapshots/` |
