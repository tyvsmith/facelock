[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

# Facelock: Face Authentication for Linux

A modern face authentication system for Linux PAM. Provides Windows Hello-style facial auth with IR anti-spoofing, configurable as a persistent daemon or daemonless one-shot.

## Quick Start

```bash
cargo build --workspace
FACELOCK_CONFIG=dev/config.toml cargo run --bin facelock -- setup    # download models
FACELOCK_CONFIG=dev/config.toml cargo run --bin facelock -- enroll   # capture face
FACELOCK_CONFIG=dev/config.toml cargo run --bin facelock -- test     # verify recognition
```

No daemon needed — the CLI auto-falls back to direct mode when no daemon is running.

## Operating Modes

| Mode | Config | How it works | Latency |
|------|--------|-------------|---------|
| **Daemon** | `mode = "daemon"` (default) | PAM → D-Bus → persistent daemon | ~200ms warm |
| **Socket activation** | systemd `.socket` unit | systemd starts daemon on demand | ~700ms cold |
| **Oneshot** | `mode = "oneshot"` | PAM → `facelock auth` subprocess | ~700ms |

The CLI works in all modes — it connects to the daemon if available, otherwise operates directly.

## Architecture

```
facelock (unified binary)
├── facelock setup          Download models, install systemd/PAM
├── facelock enroll         Capture and store a face
├── facelock test           Test recognition
├── facelock list           List enrolled models
├── facelock remove <id>    Remove a specific model
├── facelock clear          Remove all models for a user
├── facelock preview        Live camera preview
├── facelock config         Show/edit configuration
├── facelock status         Check system status
├── facelock daemon         Run persistent daemon
├── facelock auth           One-shot auth (PAM helper)
├── facelock devices        List cameras
├── facelock tpm status     TPM status/management
├── facelock bench          Benchmarks and calibration
├── facelock encrypt        Encrypt stored embeddings
├── facelock decrypt        Decrypt stored embeddings
└── facelock audit          View structured audit log

facelock-polkit-agent       Polkit face authentication agent

pam_facelock.so (PAM module)
├── daemon mode → D-Bus IPC to daemon
└── oneshot mode → fork/exec facelock auth
```

### Crates

| Crate | Type | Purpose |
|-------|------|---------|
| `facelock-core` | lib | Config, types, errors, D-Bus interface, traits |
| `facelock-camera` | lib | V4L2 capture, auto-detection, preprocessing |
| `facelock-face` | lib | ONNX inference (SCRFD detection + ArcFace embedding) |
| `facelock-store` | lib | SQLite face embedding storage |
| `facelock-daemon` | lib | Auth/enroll logic, rate limiting, liveness, audit |
| `facelock-cli` | bin | All CLI commands, daemon runner, direct mode |
| `pam-facelock` | cdylib | PAM module (libc + toml + serde + zbus only) |
| `facelock-tpm` | lib | Optional TPM sealing, software AES-256-GCM encryption |
| `facelock-polkit` | bin | Polkit face authentication agent |
| `facelock-test-support` | lib | Mock camera/engine for testing |

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
# threshold = 0.80         # cosine similarity threshold (default)
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

Full reference: `config/facelock.toml`. Override path: `FACELOCK_CONFIG` env var.

## Installation

See [docs/quickstart.md](docs/quickstart.md) for full instructions.

```bash
# Arch Linux
cd dist && makepkg -si
sudo facelock setup
sudo facelock setup --systemd   # enable socket activation
sudo facelock enroll
sudo facelock setup --pam       # install to /etc/pam.d/sudo
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
- D-Bus system bus policy restricts daemon access
- PAM audit logging to syslog
- Rate limiting (5 attempts/user/60s)
- systemd service hardening (ProtectSystem=strict, NoNewPrivileges, etc.)

See [docs/security.md](docs/security.md) for the full threat model.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.

The ONNX face models used by Facelock are licensed separately under the InsightFace
non-commercial research license. See [models/NOTICE.md](models/NOTICE.md) for details.
