[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

# Facelock: Face Authentication for Linux

> **Release status (checked 2026-09-05):** v0.1.4 is the latest published stable release. This tree
> is v0.2.0-alpha.1; its tag exists, but no corresponding GitHub Release is
> currently published. A tag, source build, staged package, or successful
> rebuild is not evidence that an artifact is available to users.

A modern face authentication system for Linux PAM. Provides Windows Hello-style
facial auth with IR-required capture and layered static-presentation checks,
configurable as a persistent daemon or daemonless one-shot. All inference runs
locally on your hardware -- no cloud services, no runtime network requests, no
telemetry. Your biometric data never leaves your machine.

## Install

### Arch Linux (AUR)

This installs the stable source-build package. `facelock-bin` is the prebuilt
alternative and `facelock-git` follows development; all three AUR entries
served version 0.1.4-1 when checked on 2026-09-05.

```bash
yay -S facelock           # or paru -S facelock
```

### Debian / Ubuntu (APT status)

Debian-family release support is exactly Debian 13 (Trixie) and Ubuntu 26.04
LTS (Resolute). Those exact suites are the 0.2.0 stable publication contract,
but they are not currently served: clean checks of both codenamed `Release`
URLs returned 404 on 2026-09-05. Do not add that source until a stable 0.2.0
release publishes it.

The future public base is `https://tysmith.me/facelock/apt`; this mapping is a
release contract, not a currently usable repository:

| Future stable target | Suite | Required package capability |
|----------------------|-------|-----------------------------|
| Debian 13 | `trixie` | TPM |
| Ubuntu 26.04 | `resolute` | TPM |

Source entries written for v0.1.4 name the `main` or `legacy` suite. Both
returned a signed `Release` file on 2026-09-05. The release policy says they
keep working until 0.3.0: `main` maps to the Trixie package set and `legacy`
serves signed empty indexes. At 0.3.0, `apt update` fails until the entry is
removed. Bookworm, Noble, and Ubuntu 25.x have no future suite. Use the source
build below for the current alpha tree.

### Fedora (COPR)

The supported COPR targets are Fedora 43, 44, and 45. On 2026-09-05, the
production COPR served v0.1.3 on all three, behind the v0.1.4 stable release;
use the source path below if you need the current tree. The staging COPR is a
candidate channel, not a stable installation source. RHEL is not in the
supported matrix.

```bash
sudo dnf install dnf5-plugins
sudo dnf copr enable tyvsmith/facelock
sudo dnf install facelock
```

### From Source

Install the distro-specific build prerequisites first; the Rust dependency and
the separately loaded ONNX Runtime shared library are not the same thing. See
the [source-build prerequisites](docs/quickstart.md#source-build-prerequisites).

```bash
just build                # build into target/debug; does not install
target/debug/facelock --help
```

`just build` does not install Facelock, and `--help` does not prove that an
ONNX Runtime can be loaded. On Arch, where the system runtime is packaged, the
optional `just install` command builds and installs the current tree, prompts
for sudo for file writes, and does not edit PAM. Debian/Ubuntu source builds
need a separately installed trusted ONNX Runtime to run inference; their future
Facelock `.deb` packages bundle it. Fedora users should use the RPM/COPR layout,
and the quickstart records why the current NixOS source-tree module is not yet
a usable authentication installation.

### Post-Install

After installing a package, or after a source installation with a working ONNX
Runtime:

```bash
sudo facelock setup       # interactive wizard: camera, models, encryption,
                          # daemon, enrollment, and optional PAM services
```

That's it. Open a new terminal and run `sudo echo "ok"` to confirm face auth fires. Keep a root shell open until you've verified it works.

The setup wizard already offers enrollment; do not add a redundant enrollment
command to the initial sequence. To re-run individual steps later:
`sudo facelock enroll`, `sudo facelock test`, `sudo facelock setup --systemd`,
or `sudo facelock setup --pam`.

Every wizard step can also be answered or declined from the command line — `--camera`, `--models`, `--execution-provider`, `--encryption` to supply a value, and `--no-pam` / `--no-systemd` / `--no-enroll` to decline an action outright. See the [CLI reference](book/src/cli-reference.md#facelock-setup) for the full flag surface.

### GPU Acceleration (Optional)

GPU support is runtime-only -- no Facelock rebuild is needed. On Arch, replace
the CPU runtime with the matching official-repository ONNX Runtime variant and
set `execution_provider` in `/etc/facelock/config.toml`:

| GPU Vendor | Package (Arch) | Config value |
|------------|---------------|--------------|
| NVIDIA | `onnxruntime-opt-cuda` | `"cuda"` |
| AMD | `onnxruntime-opt-rocm` | `"rocm"` |
| Intel | none packaged | `"openvino"` |

Facelock has configuration support for CUDA, ROCm, and OpenVINO; those GPU
paths are not part of the release package validation matrix. CPU is the
default. Arch packages no OpenVINO build of ONNX Runtime, in the repositories
or the AUR. Build ONNX Runtime with the OpenVINO execution provider yourself to
use `execution_provider = "openvino"`. See [GPU acceleration](book/src/gpu.md).

### Uninstall

```bash
just uninstall  # source installations only; remove native packages with their package manager
```

Ordinary uninstall preserves biometric and configuration state. Preview the
bounded purge with `sudo facelock data purge --dry-run`; destruction additionally
requires `--allow-destruction`. It operates only inside compiled Facelock roots
and reports unsafe or externally configured remnants for manual inspection.

## Operating Modes

| Mode | Config | How it works | Latency |
|------|--------|-------------|---------|
| **Daemon** | `mode = "daemon"` (default) | PAM → D-Bus → persistent daemon | fastest: no model load, no reopen when warm |
| **D-Bus activation** | systemd + D-Bus service | systemd starts daemon on demand | + daemon start on the first call |
| **Oneshot** | `mode = "oneshot"` | PAM → `facelock auth` subprocess | + model load on every call |

Daemon latency depends on camera state: a cold attempt pays a camera reopen, a retry within `device.camera_release_secs` of a failed attempt does not. That reopen cost is a property of your camera and driver, not a number to quote from someone else's laptop — measure it with `sudo facelock bench camera-reopen`, which prints the open / STREAMON / warmup split. The lighter default model (`scrfd_2.5g`) keeps inference fast.

The CLI works in all modes — it connects to the daemon if available, otherwise operates directly.

## CLI Reference

<!-- docs-example: schematic command overview, not executable shell -->
```text
facelock setup          Download models, validate systemd, configure PAM
facelock is-enrolled    Is this user enrolled? (exit 0/1/2)
facelock capabilities   Report machine-readable integration capabilities
facelock enroll         Capture and store a face
facelock test           Test recognition; inspect output, not exit 0 alone
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
facelock pam            Inspect or edit PAM services
facelock hyprlock       Manage the built-in hyprlock adapter
facelock data purge     Preview or destroy retained state
facelock audit          View structured audit log
```

### For integrators

Desktop projects own their setup/removal wrapper and lock-screen UI. Facelock
provides stable capability, enrollment, and arbitrary-service PAM commands for
those wrappers; it does not ship desktop-specific downstream scripts. See the
[integration guide](docs/integrating.md) for the complete contract and a worked
Omarchy example.

## Architecture

```text
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

Facelock includes a [hyprlock](https://github.com/hyprwm/hyprlock) adapter for
systems where a hyprlock PAM service is present. This does not imply package or
hardware validation for every Hyprland distribution. Two things are needed:

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
- Frame variance and IR texture checks are enabled by default against static
  presentation attacks; landmark liveness is experimental and off by default.
  These checks do not establish resistance to video replay.
- Constant-time embedding comparison via `subtle` crate
- AES-256-GCM encryption at rest with optional TPM-sealed keys
- Model SHA256 verification at every load
- D-Bus system bus policy: deny-all default; `Authenticate` open to every local user (daemon-checked UID), everything else root-only; no group
- D-Bus caller UID verification on all daemon methods
- PAM audit logging to syslog
- Rate limiting (5 face-detected authentication failures/user/60s by default)
- systemd service hardening (ProtectSystem=strict, NoNewPrivileges, etc.)

See [docs/security.md](docs/security.md) for the full threat model.

## Releasing

```bash
just version              # show current version
just release 0.2.0        # bump version across all packaging files
git push origin main --tags  # trigger CI release workflow
```

A `vX.Y.Z` tag starts the release workflow. It attempts binaries and the direct
Fedora `.rpm`. It also attempts two suite-specific `.deb` artifacts, one for
each supported suite. Those builds are not publication proof: the release and
every required asset must be present and verified. Stable releases publish
AUR/APT and enable production COPR handling;
prereleases must not enter those stable channels. As checked on 2026-09-05,
the existing v0.2.0-alpha.1 tag has no GitHub Release. See
[docs/releasing.md](docs/releasing.md) for the gates and versioning contract.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.

The ONNX face models used by Facelock are licensed separately under the InsightFace
non-commercial research license. See [models/NOTICE.md](models/NOTICE.md) for details.
