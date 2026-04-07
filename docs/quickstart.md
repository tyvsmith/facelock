# Quickstart

## Prerequisites

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

No daemon needed — the CLI auto-falls back to direct mode when no daemon is running.

### 3. Explore

```bash
sudo facelock devices            # list cameras
sudo facelock list               # see enrolled models
sudo facelock preview --text-only  # live detection output
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

**A broken PAM module can lock you out.** Keep a root shell open until you've verified face auth works. See [testing-safety.md](testing-safety.md) for details.

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

Config and data are preserved in `/etc/facelock` and `/var/lib/facelock`. To remove everything:

```bash
sudo rm -rf /etc/facelock /var/lib/facelock /var/log/facelock
```

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
