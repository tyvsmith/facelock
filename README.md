# Visage: Face Authentication for Linux

A modern face authentication system for Linux PAM. Provides Windows Hello-style facial auth with IR anti-spoofing, configurable as a persistent daemon or daemonless one-shot.

## Quick Start

```bash
cargo build --workspace
VISAGE_CONFIG=dev/config.toml cargo run --bin visage -- setup    # download models
VISAGE_CONFIG=dev/config.toml cargo run --bin visage -- enroll   # capture face
VISAGE_CONFIG=dev/config.toml cargo run --bin visage -- test     # verify recognition
```

No daemon needed — the CLI auto-falls back to direct mode when no daemon is running.

## Operating Modes

| Mode | Config | How it works | Latency |
|------|--------|-------------|---------|
| **Daemon** | `mode = "daemon"` (default) | PAM → socket → persistent daemon | ~50ms warm |
| **Socket activation** | systemd `.socket` unit | systemd starts daemon on demand | ~700ms cold |
| **Oneshot** | `mode = "oneshot"` | PAM → `visage auth` subprocess | ~700ms |

The CLI works in all modes — it connects to the daemon if available, otherwise operates directly.

## Architecture

```
visage (unified binary)
├── visage setup          Download models, install systemd/PAM
├── visage enroll         Capture and store a face
├── visage test           Test recognition
├── visage list           List enrolled models
├── visage preview        Live camera preview
├── visage daemon         Run persistent daemon
├── visage auth           One-shot auth (PAM helper)
├── visage devices        List cameras
├── visage tpm status     TPM status
└── visage bench          Benchmarks

pam_visage.so (PAM module, ~600KB)
├── daemon mode → socket IPC to daemon
└── oneshot mode → fork/exec visage auth
```

### Crates

| Crate | Type | Purpose |
|-------|------|---------|
| `visage-core` | lib | Config, types, errors, IPC protocol, traits |
| `visage-camera` | lib | V4L2 capture, auto-detection, preprocessing |
| `visage-face` | lib | ONNX inference (SCRFD detection + ArcFace embedding) |
| `visage-store` | lib | SQLite face embedding storage |
| `visage-daemon` | lib | Auth/enroll logic, rate limiting, request handler |
| `visage-cli` | bin | All CLI commands, daemon runner, direct mode |
| `pam-visage` | cdylib | PAM module (libc + toml + serde only) |
| `visage-tpm` | lib | Optional TPM encryption (feature-gated) |
| `visage-test-support` | lib | Mock camera/engine for testing |

### Face Recognition Pipeline

```
Camera Frame → SCRFD Detection → 5-point landmarks
  → Affine Alignment → 112x112 face crop
  → ArcFace Embedding → 512-dim L2-normalized vector
  → Cosine Similarity vs stored embeddings → MATCH / NO MATCH
```

## Configuration

All keys are optional. Camera is auto-detected if `device.path` is omitted.

```toml
[device]
# path = "/dev/video2"     # auto-detected if omitted (prefers IR)

[recognition]
# threshold = 0.45         # cosine similarity threshold
# detector_model = "scrfd_2.5g_bnkps.onnx"
# embedder_model = "w600k_r50.onnx"
# threads = 4              # ORT inference threads

[daemon]
# mode = "daemon"          # "daemon" or "oneshot"
# idle_timeout_secs = 0    # socket-activated idle shutdown

[security]
# require_ir = true        # refuse auth on RGB cameras
# require_frame_variance = true  # reject photo attacks
```

Full reference: `config/visage.toml`. Override path: `VISAGE_CONFIG` env var.

## Installation

See [docs/quickstart.md](docs/quickstart.md) for full instructions.

```bash
# Arch Linux
cd dist && makepkg -si
sudo visage setup
sudo visage setup --systemd   # enable socket activation
sudo visage enroll
sudo visage setup --pam       # install to /etc/pam.d/sudo
```

## Testing

```bash
just check              # unit tests + clippy + fmt
just test-pam           # container PAM smoke tests
just test-integration   # end-to-end with camera (daemon mode)
just test-oneshot       # end-to-end with camera (no daemon)
just test-shell         # interactive container for manual testing
```

See [docs/testing-safety.md](docs/testing-safety.md) before editing PAM config on your system.

## Security

- IR camera enforcement on by default (anti-spoofing)
- Frame variance checks reject static photo attacks
- Model SHA256 verification at every load
- Socket peer credential checks (SO_PEERCRED)
- PAM audit logging to syslog
- Rate limiting (5 attempts/user/60s)
- systemd service hardening (ProtectSystem=strict, NoNewPrivileges, etc.)

See [docs/security.md](docs/security.md) for the full threat model.
