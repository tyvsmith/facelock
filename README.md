[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

# Facelock: Face Authentication for Linux

> **v0.1.4** — Stable release. See [CHANGELOG.md](CHANGELOG.md) for details.

A modern face authentication system for Linux PAM. Provides Windows Hello-style facial auth with IR anti-spoofing, configurable as a persistent daemon or daemonless one-shot. All inference runs locally on your hardware -- no cloud services, no network requests, no telemetry. Your biometric data never leaves your machine.

## Install

### Arch Linux (AUR)

```bash
yay -S facelock           # or paru -S facelock
```

### Debian / Ubuntu (APT)

```bash
# Add signing key
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://tysmith.me/facelock/apt/tysmith-archive-keyring.gpg \
  | sudo tee /etc/apt/keyrings/tysmith-archive-keyring.gpg >/dev/null

# Set APT_SUITE to the exact suite for your host:
# Debian 13 — trixie — TPM
# Debian 12 — bookworm — legacy
# Ubuntu 26.04 — resolute — TPM
# Ubuntu 24.04 — noble — legacy
APT_SUITE=trixie  # Debian 13 example
echo "deb [signed-by=/etc/apt/keyrings/tysmith-archive-keyring.gpg] https://tysmith.me/facelock/apt ${APT_SUITE} facelock" \
  | sudo tee /etc/apt/sources.list.d/facelock.list

sudo apt update && sudo apt install facelock
```

### Fedora / RHEL (COPR)

```bash
sudo dnf copr enable tyvsmith/facelock
sudo dnf install facelock
```

### From Source

```bash
just install              # build + install binaries, systemd, D-Bus, PAM
```

### Post-Install

```bash
sudo facelock setup       # interactive wizard: camera, models, encryption,
                          # daemon, enrollment, and PAM for sudo + screen lock
```

That's it. Open a new terminal and run `sudo echo "ok"` to confirm face auth fires. Keep a root shell open until you've verified it works.

To re-run individual steps later: `sudo facelock enroll`, `sudo facelock test`, `sudo facelock setup --systemd`, `sudo facelock setup --pam`.

Every wizard step can also be answered or declined from the command line — `--camera`, `--models`, `--execution-provider`, `--encryption` to supply a value, and `--no-pam` / `--no-systemd` / `--no-enroll` to decline an action outright. See the [CLI reference](book/src/cli-reference.md#facelock-setup) for the full flag surface.

### GPU Acceleration (Optional)

GPU support is runtime-only -- no special build flags needed. Install a GPU-enabled ONNX Runtime package for your hardware and set `execution_provider` in `/etc/facelock/config.toml`:

| GPU Vendor | Package (Arch) | Config value |
|------------|---------------|--------------|
| NVIDIA | `onnxruntime-opt-cuda` | `"cuda"` |
| AMD | `onnxruntime-opt-rocm` | `"rocm"` |
| Intel | `onnxruntime-opt-openvino` | `"openvino"` |

Supports CUDA, ROCm, and OpenVINO execution providers. CPU is the default.

### Uninstall

```bash
just uninstall
```

## Operating Modes

| Mode | Config | How it works | Latency |
|------|--------|-------------|---------|
| **Daemon** | `mode = "daemon"` (default) | PAM → D-Bus → persistent daemon | fastest: no model load, no reopen when warm |
| **D-Bus activation** | systemd + D-Bus service | systemd starts daemon on demand | + daemon start on the first call |
| **Oneshot** | `mode = "oneshot"` | PAM → `facelock auth` subprocess | + model load on every call |

Daemon latency depends on camera state: a cold attempt pays a camera reopen, a retry within `device.camera_release_secs` of a failed attempt does not. That reopen cost is a property of your camera and driver, not a number to quote from someone else's laptop — measure it with `sudo facelock bench camera-reopen`, which prints the open / STREAMON / warmup split. The lighter default model (`scrfd_2.5g`) keeps inference fast.

The CLI works in all modes — it connects to the daemon if available, otherwise operates directly.

## CLI Reference

```
facelock setup          Download models, install systemd/PAM
facelock is-enrolled    Is this user enrolled? (exit 0/1/2)
facelock enroll         Capture and store a face
facelock test           Test recognition
facelock list           List enrolled models
facelock remove <id>    Remove a specific model
facelock clear          Remove all models for a user
facelock preview        Live camera preview
facelock config         Show configuration (config edit to open $EDITOR)
facelock status         Check system status
facelock daemon         Run persistent daemon (daemon restart to restart it)
facelock auth           One-shot auth (PAM helper)
facelock devices        List cameras
facelock tpm status     TPM status/management
facelock tpm encrypt    Encrypt stored embeddings (tpm decrypt to reverse)
facelock tpm reseal     Re-seal the TPM key under current PCRs
facelock bench          Benchmarks and calibration
facelock audit          View structured audit log
```

### For integrators

Desktop projects own their setup/removal wrapper and lock-screen UI. Facelock
provides stable capability, enrollment, and arbitrary-service PAM commands for
those wrappers; it does not ship desktop-specific downstream scripts. See the
[integration guide](docs/integrating.md) for the complete contract and a worked
Omarchy example.

## Architecture

```
facelock-core       Config, types, errors, D-Bus interface, traits
facelock-camera     V4L2 capture, auto-detection, preprocessing
facelock-face       ONNX inference (SCRFD detection + ArcFace embedding)
facelock-store      SQLite face embedding storage
facelock-daemon     Auth/enroll logic, rate limiting, liveness, audit
facelock-cli        Unified CLI binary (facelock)
facelock-bench      Standalone benchmark and calibration utility
facelock-tpm        TPM-sealed key encryption, software AES-256-GCM
facelock-polkit     Polkit authentication agent
pam-facelock        PAM module (libc + toml + serde + zbus only)
facelock-test-support  Mocks and fixtures for testing
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
# execution_provider = "cpu"  # "cpu", "cuda", "rocm", or "openvino"
# threads = 4              # ORT inference threads

[daemon]
# mode = "daemon"          # "daemon" or "oneshot"

[security]
# require_ir = true        # refuse auth on RGB cameras
# require_frame_variance = true  # reject photo attacks
```

Full reference: `config/facelock.toml`.

## Hyprlock Integration

Facelock works with [hyprlock](https://github.com/hyprwm/hyprlock) on Hyprland (Arch, Omarchy, NixOS, etc.). Two things are needed:

1. **PAM line** in `/etc/pam.d/hyprlock` — `sudo facelock setup` does this automatically when you select hyprlock in the PAM step.
2. **Lock-screen tweak** in `~/.config/hypr/hyprlock.conf` — set `ignore_empty_input = false` and add a face icon to `placeholder_text`. Run as your normal user:
   ```bash
   facelock hyprlock enable      # add face icon + enable empty-Enter submission
   facelock hyprlock enable --no-icon  # functional change only (no icon)
   facelock hyprlock disable     # revert (preserves fingerprint setup if present)
   facelock hyprlock status      # show current integration state
   ```

`facelock hyprlock enable` preserves any existing fingerprint integration (icon 󰈷, `fingerprint:enabled = true`, `pam_fprintd.so`) — face and fingerprint can coexist. If your hyprlock font isn't a Nerd Font, run with `--no-icon`; the functional integration still works.

This command family is frozen compatibility surface, not a template for new
desktop adapters. New desktops use `facelock pam add --service <name>` and own
their UI/configuration changes downstream. Omarchy likewise owns its
end-to-end integration and package choice; Facelock only supplies the backend
contracts described in the [integration guide](docs/integrating.md).

## Testing

```bash
just check              # unit tests + clippy + fmt
just test-arch-pam          # Arch container PAM smoke tests
just test-arch-integration  # end-to-end with camera (daemon mode)
just test-arch-oneshot      # end-to-end with camera (no daemon)
just test-arch-dev-shell    # interactive container for manual testing
```

See [docs/testing-safety.md](docs/testing-safety.md) before editing PAM config on your system.

## Privacy & Security

**Privacy**: Facelock is 100% local. Face detection and recognition run entirely on your hardware via ONNX Runtime. No images, embeddings, or metadata are ever sent to any external server. There is no telemetry, no analytics, no phone-home behavior. Models are downloaded once during setup and verified by SHA256 checksum -- after that, Facelock never touches the network.

**Security**:

- IR camera enforcement on by default (anti-spoofing)
- Frame variance + landmark liveness checks reject photo/video attacks
- Constant-time embedding comparison via `subtle` crate
- AES-256-GCM encryption at rest with optional TPM-sealed keys
- Model SHA256 verification at every load
- D-Bus system bus policy: deny-all default; `Authenticate` open to every local user (daemon-checked UID), everything else root-only; no group
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

Tagging `vX.Y.Z` builds release binaries, four suite-specific `.deb` artifacts
(trixie, bookworm, resolute, and noble), and the direct Fedora `.rpm` artifact.
Stable tags publish the stable AUR and APT channels; Packit handles production
COPR builds. Prerelease tags create a GitHub prerelease without entering those
stable channels. See [docs/releasing.md](docs/releasing.md) for the full process
and versioning contract.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.

The ONNX face models used by Facelock are licensed separately under the InsightFace
non-commercial research license. See [models/NOTICE.md](models/NOTICE.md) for details.
