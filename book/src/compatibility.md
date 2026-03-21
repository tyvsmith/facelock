# Compatibility

## System Requirements

| Component | Requirement |
|-----------|-----------|
| OS | Linux (kernel 4.14+ for V4L2) |
| Architecture | x86_64 (ONNX Runtime binaries) |
| Rust | 1.85+ (edition 2024) |
| Camera | V4L2-compatible (USB webcam, built-in IR) |
| PAM | Linux-PAM (pam 1.5+) |

## Tested Distributions

| Distribution | Init System | Mode | Status |
|-------------|-------------|------|--------|
| Arch Linux | systemd | daemon + D-Bus activation | Primary target |
| Arch Linux | systemd | oneshot | Tested |
| Container (Arch) | none | daemon (manual) | CI-tested |
| Container (Arch) | none | oneshot | CI-tested |

### Expected to Work (untested)

| Distribution | Init System | Mode |
|-------------|-------------|------|
| Fedora 38+ | systemd | daemon + D-Bus activation |
| Ubuntu 22.04+ | systemd | daemon + D-Bus activation |
| Debian 12+ | systemd | daemon + D-Bus activation |
| Any Linux | any / none | oneshot |
| Void Linux | runit | oneshot or manual daemon |
| Alpine Linux | OpenRC | oneshot or manual daemon |
| Gentoo | OpenRC / systemd | oneshot or daemon |

## Camera Compatibility

### IR Cameras (recommended)

IR cameras provide anti-spoofing protection. Facelock auto-detects IR cameras by:
- Device name containing "ir" or "infrared"
- Supporting GREY or Y16 pixel formats

Known working:
- Logitech BRIO (IR mode)
- Intel RealSense (IR stream)
- Most laptops with Windows Hello IR cameras

### RGB Cameras (development only)

RGB cameras work with `security.require_ir = false` but provide no anti-spoofing. Any photo of the enrolled user will authenticate.

### Format Support

| Format | Support | Notes |
|--------|---------|-------|
| MJPG | Full | Most common USB camera format |
| YUYV | Full | Raw format, converted to RGB |
| GREY | Full | IR cameras, replicated to RGB |
| Other | Not supported | Camera negotiates to supported format |

## Init System Support

### systemd (recommended)

Full support via D-Bus activation:
```bash
sudo facelock setup --systemd
```

Features:
- D-Bus activation (daemon starts on first connection)
- Idle timeout (daemon stops when idle)
- Service hardening (ProtectSystem, NoNewPrivileges, etc.)
- Automatic restart on failure

### Non-systemd

Use oneshot mode (no daemon needed):
```toml
[daemon]
mode = "oneshot"
```

Or manage the daemon manually:
```bash
facelock daemon &                    # start
kill $(pidof facelock)               # stop
```

For process supervisors (runit, s6, dinit, OpenRC), create a service that runs `facelock daemon`. The daemon handles SIGTERM for graceful shutdown.

## PAM Stack Compatibility

Facelock works with standard Linux-PAM. The module is installed as:
```
auth  sufficient  pam_facelock.so
```

### Tested PAM Services

| Service | File | Notes |
|---------|------|-------|
| sudo | `/etc/pam.d/sudo` | Primary target, safest to test first |
| polkit | `/etc/pam.d/polkit-1` | GUI privilege escalation |

### Not Recommended

| Service | Reason |
|---------|--------|
| system-auth | Affects ALL auth -- test sudo first |
| login | Console login -- hard to recover if broken |
| sshd | SSH has no camera -- always fails |

## Build Dependencies

### Runtime
- `pam` (Linux-PAM library)
- `gcc-libs` (C runtime)

### Build
- `rust` + `cargo` (1.85+)
- `clang` (for ONNX Runtime bindings)
- System headers: `libv4l-dev`, `libxkbcommon-dev`, `libpam0g-dev` (names vary by distro)

### Optional
- `tpm2-tss` -- TPM2 support for embedding encryption
- `podman` or `docker` -- container testing

## ONNX Runtime

Facelock uses the `ort` crate (Rust bindings for ONNX Runtime). The runtime binary is downloaded at build time via the `download-binaries` feature.

### Execution Providers

GPU support is runtime-only -- no special build flags needed. Install a GPU-enabled ONNX Runtime package and set `execution_provider` in config.

| Provider | Config | Runtime Requirement | Status |
|----------|--------|---------------------|--------|
| CPU | `execution_provider = "cpu"` | none (default) | Working |
| CUDA (NVIDIA) | `execution_provider = "cuda"` | CUDA toolkit + GPU-enabled ORT | Config ready, untested |
| ROCm (AMD) | `execution_provider = "rocm"` | ROCm runtime + GPU-enabled ORT | Config ready, untested |
| OpenVINO (Intel) | `execution_provider = "openvino"` | OpenVINO runtime + GPU-enabled ORT | Config ready, untested |

CPU is the default and only tested provider.
