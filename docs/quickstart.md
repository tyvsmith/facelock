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
FACELOCK_CONFIG=dev/config.toml cargo run --bin facelock -- setup
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
export FACELOCK_CONFIG=dev/config.toml
```

The dev config auto-detects the camera and uses temp paths. No root needed.

## 4. Enroll and Test

No daemon required — the CLI operates directly when no daemon is running:

```bash
facelock devices            # list cameras
facelock enroll             # capture face (look at camera)
facelock test               # verify recognition
facelock list               # see enrolled models
facelock preview --text-only  # live detection output
```

To use daemon mode instead:
```bash
facelock daemon &           # start daemon in background
facelock enroll             # now uses daemon (faster)
facelock test
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
sudo facelock setup                    # download models
sudo facelock enroll                   # capture face
sudo facelock test                     # verify
sudo facelock setup --systemd          # enable socket activation
sudo facelock setup --pam              # install to /etc/pam.d/sudo
```

### Debian / Ubuntu

Coming soon. Packages are not yet built for Debian-based distributions. In the meantime, use the manual install method below.

### Fedora

Coming soon. Packages are not yet built for Fedora. In the meantime, use the manual install method below.

### NixOS

Coming soon. A Nix flake is not yet available. In the meantime, use the manual install method below.

### Manual Install
```bash
sudo -i                              # keep this root shell open!

cargo build --workspace --release
sudo install -m 755 target/release/facelock /usr/bin/facelock
sudo install -m 755 target/release/libpam_facelock.so /lib/security/pam_facelock.so

sudo mkdir -p /etc/facelock /var/lib/facelock/models /run/facelock
sudo cp config/facelock.toml /etc/facelock/config.toml
sudo cp models/*.onnx /var/lib/facelock/models/

# Socket activation (systemd):
sudo facelock setup --systemd

# Or oneshot mode (no daemon):
# Edit /etc/facelock/config.toml: daemon.mode = "oneshot"

# PAM (start with sudo only):
sudo facelock setup --pam --service sudo

# Test in a NEW terminal:
sudo echo "it works"

# If anything breaks, revert from root shell:
# sudo cp /etc/pam.d/sudo.facelock-backup /etc/pam.d/sudo
```

## Dev Config Reference

`dev/config.toml` uses temp paths — no root needed:

| What | Path |
|------|------|
| Camera | Auto-detected |
| Socket | `/tmp/facelock-dev.sock` |
| Database | `/tmp/facelock-dev.db` |
| Models | `./models/` |
| Snapshots | `/tmp/facelock-dev-snapshots/` |
