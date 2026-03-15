# Introduction

Facelock is a modern face authentication system for Linux PAM. It provides Windows Hello-style facial authentication with IR anti-spoofing, configurable as a persistent daemon or daemonless one-shot.

## Quick Start

```bash
cargo build --workspace
FACELOCK_CONFIG=dev/config.toml cargo run --bin facelock -- setup    # download models
FACELOCK_CONFIG=dev/config.toml cargo run --bin facelock -- enroll   # capture face
FACELOCK_CONFIG=dev/config.toml cargo run --bin facelock -- test     # verify recognition
```

No daemon needed -- the CLI auto-falls back to direct mode when no daemon is running.

## Operating Modes

| Mode | Config | How it works | Latency |
|------|--------|-------------|---------|
| **Daemon** | `mode = "daemon"` (default) | PAM connects via D-Bus, persistent daemon | ~200ms warm |
| **Socket activation** | systemd `.socket` unit | systemd starts daemon on demand | ~700ms cold |
| **Oneshot** | `mode = "oneshot"` | PAM spawns `facelock auth` subprocess | ~700ms |

The CLI works in all modes -- it connects to the daemon if available, otherwise operates directly.

## Architecture

```
facelock (unified binary)
├── facelock setup          Download models, install systemd/PAM
├── facelock enroll         Capture and store a face
├── facelock test           Test recognition
├── facelock list           List enrolled models
├── facelock preview        Live camera preview
├── facelock daemon         Run persistent daemon
├── facelock auth           One-shot auth (PAM helper)
├── facelock devices        List cameras
├── facelock tpm status     TPM status
└── facelock bench          Benchmarks

pam_facelock.so (PAM module)
├── daemon mode → D-Bus IPC to daemon
├── polkit agent → facelock-polkit
└── oneshot mode → fork/exec facelock auth
```

### Crates

| Crate | Type | Purpose |
|-------|------|---------|
| `facelock-core` | lib | Config, types, errors, D-Bus interface, traits |
| `facelock-camera` | lib | V4L2 capture, auto-detection, preprocessing |
| `facelock-face` | lib | ONNX inference (SCRFD detection + ArcFace embedding) |
| `facelock-store` | lib | SQLite face embedding storage |
| `facelock-daemon` | lib | Auth/enroll logic, liveness, audit, rate limiting, request handler |
| `facelock-cli` | bin | All CLI commands, daemon runner, direct mode |
| `pam-facelock` | cdylib | PAM module (libc + toml + serde + zbus only) |
| `facelock-tpm` | lib | Optional TPM-bound encryption for embeddings at rest |
| `facelock-polkit` | bin | Polkit authentication agent for face auth |
| `facelock-test-support` | lib | Mock camera/engine for testing |

### Face Recognition Pipeline

```
Camera Frame → SCRFD Detection → 5-point landmarks
  → Affine Alignment → 112x112 face crop
  → ArcFace Embedding → 512-dim L2-normalized vector
  → Cosine Similarity vs stored embeddings → MATCH / NO MATCH
```

## Configuration

All keys are optional. Camera is auto-detected if `device.path` is omitted. See the [Configuration](configuration.md) chapter for full reference.

```toml
[device]
# path = "/dev/video2"     # auto-detected if omitted (prefers IR)

[recognition]
# threshold = 0.80         # cosine similarity threshold

[daemon]
# mode = "daemon"          # "daemon" or "oneshot"

[security]
# require_ir = true        # refuse auth on RGB cameras
# require_frame_variance = true  # reject photo attacks
```

## Installation

See [Quick Start](quickstart.md) for full instructions.

## Security

- IR camera enforcement on by default (anti-spoofing)
- Frame variance checks reject static photo attacks
- Model SHA256 verification at every load
- D-Bus system bus policy
- PAM audit logging to syslog
- Rate limiting (5 attempts/user/60s)
- systemd service hardening

See [Security](security.md) for the full threat model.

## License

Dual-licensed under MIT or Apache 2.0, at your option.

The ONNX face models used by Facelock are licensed separately under the InsightFace non-commercial research license.
