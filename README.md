# Visage: Facial Authentication for Linux

A modern, performant face authentication system for Linux PAM. Provides Windows Hello-style facial auth with IR anti-spoofing, configurable as a persistent daemon or daemonless one-shot.

## Architecture

Visage supports three operating modes:

### Daemon Mode (default)
```
sudo / login
    → pam_visage.so (thin IPC client, ~600KB)
    → Unix socket
    → visage-daemon (holds ONNX models + camera in memory)
    → PAM_SUCCESS / PAM_AUTH_ERR
```
~50ms auth latency (warm). Best for frequent authentication. Managed by systemd socket activation or any process supervisor.

### Oneshot Mode (daemonless)
```
sudo / login
    → pam_visage.so
    → fork/exec visage-auth
    → loads models + opens camera + matches + exits
    → PAM_SUCCESS / PAM_AUTH_ERR
```
~700ms auth latency. No daemon process, no socket, no systemd. Set `daemon.mode = "oneshot"` in config.

### Socket Activation (systemd)
```
systemctl enable visage-daemon.socket
    → systemd listens on /run/visage/visage.sock
    → first PAM auth starts visage-daemon on demand
    → daemon shuts down after idle timeout
```
Zero memory when idle. Automatic lifecycle management.

## Workspace Crates

| Crate | Type | Purpose |
|-------|------|---------|
| `visage-core` | lib | Config, types, error handling, IPC protocol, traits |
| `visage-camera` | lib | V4L2 camera capture, auto-detection, preprocessing |
| `visage-face` | lib | ONNX inference pipeline (SCRFD + ArcFace) |
| `visage-store` | lib | SQLite face embedding storage |
| `visage-daemon` | bin | Persistent daemon owning camera + models |
| `visage-auth` | bin | One-shot auth binary (used by PAM in oneshot mode) |
| `pam-visage` | cdylib | PAM module — thin IPC client or oneshot launcher |
| `visage-cli` | bin | User-facing CLI tool |
| `visage-bench` | bin | Benchmark and calibration tooling |
| `visage-tpm` | lib | Optional TPM embedding encryption (feature-gated) |
| `visage-test-support` | lib | Mock camera/engine for testing (dev only) |

## Configuration

TOML config at `/etc/visage/config.toml`. All keys are optional — Visage auto-detects the camera and uses sensible defaults.

```toml
[device]
# path = "/dev/video2"     # Optional: auto-detected if omitted (prefers IR cameras)
# max_height = 480
# rotation = 0

[recognition]
# threshold = 0.45         # Cosine similarity threshold (0.0-1.0)
# timeout_secs = 5

[daemon]
# mode = "daemon"          # "daemon" (default) or "oneshot" (no daemon needed)
# socket_path = "/run/visage/visage.sock"
# model_dir = "/var/lib/visage/models"
# idle_timeout_secs = 0    # Socket-activated idle shutdown (0 = disabled)

[storage]
# db_path = "/var/lib/visage/visage.db"

[security]
# require_ir = true        # Refuse auth on RGB-only cameras (anti-spoof)
# require_frame_variance = true  # Reject static images (photo attack defense)
# min_auth_frames = 3
# abort_if_ssh = true
# abort_if_lid_closed = true

[notification]
# enabled = true

# [tpm]
# seal_database = false    # Encrypt embeddings at rest with TPM
# pcr_binding = false      # Verify system integrity at startup
```

Supports `VISAGE_CONFIG` env var override for rootless development.

## Quick Start

```bash
# Build
cargo build --workspace

# Download models (~170MB)
VISAGE_CONFIG=dev/config.toml cargo run --bin visage -- setup

# Option A: Daemon mode (start daemon, then use CLI)
export VISAGE_CONFIG=dev/config.toml
cargo run --bin visage-daemon &
cargo run --bin visage -- enroll
cargo run --bin visage -- test

# Option B: Oneshot mode (no daemon needed)
# Set mode = "oneshot" in dev/config.toml, then:
cargo run --bin visage -- enroll
cargo run --bin visage -- test
```

## Testing

```bash
# Unit tests (no hardware)
cargo test --workspace

# Hardware tests (camera + models)
cargo test --workspace -- --ignored

# Container PAM smoke tests
just test-pam

# Container end-to-end with camera (daemon mode)
just test-integration

# Container end-to-end with camera (oneshot, no daemon)
just test-oneshot

# Interactive container shell for manual pamtester
just test-shell

# All checks (test + clippy + fmt)
just check
```

## Face Recognition Pipeline

```
Camera Frame (RGB) → SCRFD Detection → Bounding boxes + 5-point landmarks
    → Affine Alignment → 112x112 face crop
    → ArcFace Embedding → 512-dim L2-normalized vector
    → Cosine Similarity vs stored embeddings → MATCH / NO MATCH
```

## Models

Downloaded during `visage setup`:
- **SCRFD** (`scrfd_2.5g_bnkps.onnx`): ~3MB face detection with keypoints
- **ArcFace** (`w600k_r50.onnx`): ~166MB face embedding network

Stored in `/var/lib/visage/models/` with SHA-256 integrity verification at load time.

## Installation

See `docs/quickstart.md` for full instructions. On Arch Linux:

```bash
makepkg -si        # from dist/PKGBUILD
visage setup       # download models
visage enroll      # capture your face
visage test        # verify recognition

# For daemon mode with socket activation:
sudo systemctl enable --now visage-daemon.socket

# Or for oneshot mode, just set in /etc/visage/config.toml:
#   [daemon]
#   mode = "oneshot"
```

Then add to `/etc/pam.d/sudo`:
```
auth  sufficient  pam_visage.so
```

**Read `docs/testing-safety.md` before editing PAM config.** Keep a root shell open.
