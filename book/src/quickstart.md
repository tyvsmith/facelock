# Quick Start

## Package Install

**Note:** Packages are published via tag-driven CI on release. If your distro doesn't see the latest version yet, fall back to building from source ([Development Setup](#development-setup)).

### Arch Linux (AUR)

```bash
yay -S facelock           # or paru -S facelock
```

### Debian / Ubuntu (APT)

Use the exact suite and package variant for your platform:

| Platform | Suite | Variant |
|----------|-------|---------|
| Debian 13 | trixie | TPM |
| Debian 12 | bookworm | legacy |
| Ubuntu 26.04 | resolute | TPM |
| Ubuntu 24.04 | noble | legacy |

```bash
# Add signing key
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://tysmith.me/facelock/apt/tysmith-archive-keyring.gpg \
  | sudo tee /etc/apt/keyrings/tysmith-archive-keyring.gpg >/dev/null

# Set this to the suite in the table above; trixie is the Debian 13 example.
APT_SUITE=trixie
echo "deb [signed-by=/etc/apt/keyrings/tysmith-archive-keyring.gpg] https://tysmith.me/facelock/apt ${APT_SUITE} facelock" \
  | sudo tee /etc/apt/sources.list.d/facelock.list

# Install
sudo apt update
sudo apt install facelock
```

### Fedora / RHEL (COPR)

```bash
sudo dnf copr enable tyvsmith/facelock
sudo dnf install facelock
```

### Post-Install

```bash
sudo facelock setup       # download models, configure PAM
sudo facelock enroll      # register your face
sudo facelock test        # verify recognition
```

---

## Prerequisites (Building from Source)

- Rust 1.85+ (`rustup update`)
- [just](https://github.com/casey/just) task runner
- Linux with V4L2 support
- System dependencies: `libv4l-dev libpam0g-dev clang` (Debian/Ubuntu) or `v4l-utils pam clang` (Arch)
- A webcam (IR recommended for production; RGB works for development)

## Development Setup

### 1. Build

```bash
just build
```

### 2. Download Models and Enroll

```bash
sudo facelock setup     # interactive wizard (camera, models, encryption)
sudo facelock enroll    # capture your face (look at camera)
sudo facelock test      # verify recognition works
```

No daemon needed -- the CLI auto-falls back to direct mode when no daemon is running.

### 3. Explore

```bash
sudo facelock devices            # list cameras
sudo facelock list               # see enrolled models
sudo facelock preview --json     # live detection output, one JSON object per frame
sudo facelock status             # check system status
sudo facelock bench warm-auth    # measure auth latency
```

### 4. Run Tests

```bash
just check                # unit tests + clippy + fmt
just test-arch-pam          # Arch container PAM smoke tests (no camera)
just test-arch-integration  # end-to-end with camera (daemon mode)
just test-arch-oneshot      # end-to-end with camera (no daemon)
just test-arch-dev-shell    # interactive container shell
```

## System Installation

**A broken PAM module can lock you out.** Keep a root shell open until you've verified face auth works. See the [Testing](testing.md) chapter for details.

### Install

```bash
just install              # build release + install everything
sudo facelock setup       # download models
sudo facelock enroll      # register your face
```

This installs the binary, PAM module, systemd service, D-Bus policy, and adds face auth to `/etc/pam.d/sudo`.

### Verify

Open a **new terminal** and run:

```bash
sudo echo "face auth works"
```

You should see "Identifying face..." and authenticate by looking at the camera.

### GPU Acceleration (Optional)

GPU support is runtime-only -- no special build flags needed. The setup wizard (`facelock setup`) offers CPU or CUDA selection and warns if dependencies are missing.

For manual configuration, install a GPU-enabled ONNX Runtime package:

```bash
sudo pacman -S onnxruntime-opt-cuda      # NVIDIA
sudo pacman -S onnxruntime-opt-rocm      # AMD
sudo pacman -S onnxruntime-opt-openvino  # Intel
```

Set `execution_provider` in `/etc/facelock/config.toml` to `"cuda"`, `"rocm"`, or `"openvino"`. CPU is the default.

### Uninstall

```bash
just uninstall
```

### Package lifecycle and retained data

Ordinary package removal and `just uninstall` preserve the face database,
encryption keys, downloaded models, enrollment markers, audit logs, snapshots,
and setup state. Debian also retains its conffile until `purge`; RPM follows
`%config(noreplace)` and may retain an administrator-modified config as
`config.toml.rpmsave`.

Facelock does not currently expose a safe "remove everything" command. Do not
replace package lifecycle handling with a broad recursive deletion: configured
state paths can live outside the default directories, and links, mounts, or
wrong-owner remnants require inspection rather than traversal. The authoritative
fixed-root, retained-data, and erasure-limit contract is in
`docs/contracts.md`, "Package Lifecycle Ownership".

## Configuration

Config file: `/etc/facelock/config.toml` (installed) or `config/facelock.toml` (source).

Key settings:

| Setting | Default | Description |
|---------|---------|-------------|
| `device.path` | auto-detect | Camera path (prefers IR cameras) |
| `recognition.threshold` | `0.80` | Cosine similarity threshold |
| `recognition.execution_provider` | `"cpu"` | `"cpu"`, `"cuda"`, `"rocm"`, or `"openvino"` |
| `daemon.mode` | `"daemon"` | `"daemon"` or `"oneshot"` |
| `security.require_ir` | `true` | Reject RGB-only cameras |

Full reference: `config/facelock.toml` (all keys documented with comments).
