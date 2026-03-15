[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

# Facelock: Face Authentication for Linux

> **v0.1.0** — Pre-release. Functional and daily-driveable but APIs may change before 1.0. See [CHANGELOG.md](CHANGELOG.md).

A modern face authentication system for Linux PAM. Provides Windows Hello-style facial auth with IR anti-spoofing, configurable as a persistent daemon or daemonless one-shot.

## Quick Start

```bash
just install              # build + install binaries, systemd, D-Bus, PAM
sudo facelock setup       # download face models (~170MB)
sudo facelock enroll      # register your face
sudo facelock test        # verify recognition
```

That's it. Face auth is now active for `sudo`. Keep a root shell open until you've verified it works.

### GPU Acceleration (Optional)

```bash
sudo pacman -S onnxruntime-opt-cuda   # system ORT with CUDA support
just install-cuda                      # build + install with GPU support
```

Supports CUDA and TensorRT execution providers. Set `execution_provider = "cuda"` or `"tensorrt"` in `/etc/facelock/config.toml`.

### Uninstall

```bash
just uninstall
```

## Operating Modes

| Mode | Config | How it works | Latency |
|------|--------|-------------|---------|
| **Daemon** | `mode = "daemon"` (default) | PAM → D-Bus → persistent daemon | ~200ms warm |
| **D-Bus activation** | systemd + D-Bus service | systemd starts daemon on demand | ~700ms cold |
| **Oneshot** | `mode = "oneshot"` | PAM → `facelock auth` subprocess | ~700ms |

The CLI works in all modes — it connects to the daemon if available, otherwise operates directly.

## CLI Reference

```
facelock setup          Download models, install systemd/PAM
facelock enroll         Capture and store a face
facelock test           Test recognition
facelock list           List enrolled models
facelock remove <id>    Remove a specific model
facelock clear          Remove all models for a user
facelock preview        Live camera preview
facelock config         Show/edit configuration
facelock status         Check system status
facelock daemon         Run persistent daemon
facelock auth           One-shot auth (PAM helper)
facelock devices        List cameras
facelock tpm status     TPM status/management
facelock bench          Benchmarks and calibration
facelock encrypt        Encrypt stored embeddings
facelock decrypt        Decrypt stored embeddings
facelock audit          View structured audit log
```

## Architecture

```
facelock-core       Config, types, errors, D-Bus interface
facelock-camera     V4L2 capture, auto-detection, preprocessing
facelock-face       ONNX inference (SCRFD detection + ArcFace embedding)
facelock-store      SQLite face embedding storage
facelock-daemon     Auth/enroll logic, rate limiting, liveness, audit
facelock-cli        Unified CLI binary (facelock)
facelock-tpm        TPM-sealed key encryption, software AES-256-GCM
facelock-polkit     Polkit authentication agent
pam-facelock        PAM module (libc + toml + serde + zbus only)
```

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
# threshold = 0.80         # cosine similarity threshold
# execution_provider = "cpu"  # "cpu", "cuda", or "tensorrt"
# threads = 4              # ORT inference threads

[daemon]
# mode = "daemon"          # "daemon" or "oneshot"

[security]
# require_ir = true        # refuse auth on RGB cameras
# require_frame_variance = true  # reject photo attacks
```

Full reference: `config/facelock.toml`.

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
- Frame variance + landmark liveness checks reject photo/video attacks
- Constant-time embedding comparison via `subtle` crate
- AES-256-GCM encryption at rest with optional TPM-sealed keys
- Model SHA256 verification at every load
- D-Bus system bus policy: deny-all default, facelock group ACL
- D-Bus caller UID verification on all daemon methods
- PAM audit logging to syslog
- Rate limiting (5 attempts/user/60s)
- systemd service hardening (ProtectSystem=strict, NoNewPrivileges, etc.)

See [docs/security.md](docs/security.md) for the full threat model.

## Releasing

```bash
just version              # show current version
just release 0.2.0        # bump version across all packaging files
git push origin main --tags  # trigger CI release workflow
```

Tagging `vX.Y.Z` builds release binaries, `.deb`, and `.rpm` via GitHub Actions. See [docs/releasing.md](docs/releasing.md) for the full process and versioning contract.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.

The ONNX face models used by Facelock are licensed separately under the InsightFace
non-commercial research license. See [models/NOTICE.md](models/NOTICE.md) for details.
