# Introduction

Facelock is a modern face authentication system for Linux PAM. It provides
Windows Hello-style facial authentication with IR-required capture and layered
static-presentation checks, configurable as a persistent daemon or daemonless
one-shot. Inference runs locally; model download occurs during setup, while the
authentication path makes no network request and sends no telemetry.

## Quick Start

The latest stable release is
[v0.2.0](https://github.com/tyvsmith/facelock/releases/tag/v0.2.0), which
provides direct Debian 13, Ubuntu 26.04, and Fedora 44 packages and is served
by the AUR entries and both APT suites. Production COPR still serves v0.1.3.
See [Quick Start](quickstart.md) for the exact package filenames, the APT
source entry, and the current channel status.

```bash
just build
target/debug/facelock --help
```

Install the distro-specific native build dependencies first; Rust and the
dynamically loaded ONNX Runtime are separate prerequisites. `just build` does
not install the binary, and `--help` does not test inference or load ONNX
Runtime. Camera-facing development uses the
explicit built path and `--config "$PWD/dev/config.toml"`; management commands
remain root-gated and root ignores `FACELOCK_CONFIG`. See [Quick Start](quickstart.md)
before enrolling or changing host authentication.

## Operating Modes

| Mode | Config | How it works | Latency |
|------|--------|-------------|---------|
| **Daemon** | `mode = "daemon"` (default) | PAM connects via D-Bus, persistent daemon | fastest: no model load, no reopen when warm |
| **D-Bus activation** | systemd + D-Bus service | systemd starts daemon on demand | + daemon start on the first call |
| **Oneshot** | `mode = "oneshot"` | PAM spawns `facelock auth` subprocess | + model load on every call |

Daemon latency depends on camera state: a cold attempt pays a camera reopen, a retry within `device.camera_release_secs` of a failed attempt does not. That reopen cost is a property of your camera and driver, not a number to quote from someone else's laptop -- measure it with `sudo facelock bench camera-reopen`, which prints the open / STREAMON / warmup split.

The CLI works in all modes -- it connects to the daemon if available, otherwise operates directly.

## Architecture

<!-- docs-example: schematic command overview, not executable shell -->
```text
facelock (unified binary)
├── facelock setup          Download models, validate systemd, configure PAM
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
| `facelock-cli` | bin | All CLI commands, daemon runner, direct mode, benchmarks |
| `facelock-bench` | bin | Standalone benchmark and calibration utility |
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

## Privacy & Security

**Privacy**: Facelock is 100% local. Face detection and recognition run entirely on your hardware via ONNX Runtime. No images, embeddings, or metadata are ever sent to any external server. There is no telemetry, no analytics, no phone-home behavior. Models are downloaded once during setup -- after that, Facelock never touches the network.

**Security**:

- IR camera enforcement on by default (anti-spoofing)
- Frame variance checks reject static photo attacks
- Constant-time embedding comparison via `subtle` crate
- AES-256-GCM encryption at rest with optional TPM-sealed keys
- Model SHA256 verification at every load
- D-Bus system bus policy
- PAM audit logging to syslog
- Rate limiting (5 face-detected authentication failures/user/60s by default)
- systemd service hardening

See [Security](security.md) for the full threat model.

## License

Dual-licensed under MIT or Apache 2.0, at your option.

The ONNX face models used by Facelock are licensed separately under the InsightFace non-commercial research license.
