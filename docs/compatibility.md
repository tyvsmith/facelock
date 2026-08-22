# Compatibility

## System Requirements

| Component | Requirement |
|-----------|-----------|
| OS | Linux (kernel 4.14+ for V4L2) |
| Architecture | x86_64 (ONNX Runtime binaries) |
| Rust | 1.88+ (edition 2024) |
| Camera | V4L2-compatible (USB webcam, built-in IR) |
| PAM | Linux-PAM (pam 1.5+) |

## Tested Distributions

Debian-family support starts at Debian 13+ and Ubuntu 26.04+; older Debian and
Ubuntu releases are unsupported.

| Distribution | Init System | Mode | Status |
|-------------|-------------|------|--------|
| Arch Linux | systemd | daemon + D-Bus activation | Primary target |
| Arch Linux | systemd | oneshot | Tested |
| Debian 13 (Trixie) | systemd | daemon + D-Bus activation | Booted package gate |
| Ubuntu 26.04 LTS (Resolute) | systemd | daemon + D-Bus activation | Booted package gate |
| Container (Arch) | none | daemon (manual) | CI-tested |
| Container (Arch) | none | oneshot | CI-tested |

### Expected to Work (untested)

| Distribution | Init System | Mode |
|-------------|-------------|------|
| Fedora 38+ | systemd | daemon + D-Bus activation |
| Any Linux | any / none | oneshot |
| Void Linux | runit | oneshot or manual daemon |
| Alpine Linux | OpenRC | oneshot or manual daemon |
| Gentoo | OpenRC / systemd | oneshot or daemon |

## Camera Compatibility

### IR Cameras (recommended)

IR cameras provide anti-spoofing protection. Facelock auto-detects IR cameras from the pixel formats they report — a node that enumerates *only* IR-typical mono formats (GREY, Y8, Y10, Y12, Y16) with no color format (YUYV/MJPG) mixed in — or from a hardware quirk that matches the device. The device name is never used to classify a camera as IR.

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
| NV12 | Full | Semi-planar 4:2:0; common on Intel IPU processed cameras |
| GREY | Full | IR cameras, replicated to RGB |
| Y16 | Full | 16-bit IR grayscale, bit-depth-aware conversion to 8-bit |
| Y8, Y10, Y12 | Not supported | IR-typical classification evidence, but not decodable; excluded from the setup wizard and automatic selection with a path/format warning |
| Raw Bayer (SGRBG10, ...) | Not supported | Raw sensor nodes are skipped by auto-detection |
| Other | Not supported | Device is rejected at open with an error listing its formats |

Negotiation priority: `GREY > Y16 > YUYV > NV12 > MJPG` (a hardware quirk's
`format_preference` is tried first). Devices that advertise none of these are
excluded from auto-detection and setup selection, and opening one explicitly
fails with an error naming the advertised formats. This includes an IR node
whose only formats are Y8, Y10, or Y12: it still classifies as IR, but Facelock
does not persist it as an automatic setup choice. If `security.require_ir` is
enabled and every detected IR node is excluded this way, the setup wizard
stops with all excluded IR paths and formats instead of offering an RGB node.
When `require_ir` is disabled, a decodable RGB node remains a valid explicit
wizard choice.

### Intel IPU6/IPU7 MIPI cameras (v4l2-relayd)

Intel IPU6/IPU7 laptop cameras (many 2023+ Dell XPS, Lenovo ThinkPad, HP
models) expose their sensors as **raw Bayer capture nodes** (`/dev/video0`
through `/dev/video31`, formats like `SGRBG10`). Facelock cannot decode raw
Bayer — these nodes are skipped by auto-detection. The usable camera is the
**processed loopback device** provided by `v4l2-relayd` + `v4l2loopback`
(commonly `/dev/video50`), fed by the `icamerasrc` GStreamer element.

On newer platforms (Panther Lake and later) the RGB path additionally needs the
out-of-tree `intel_cvs` module from
[intel/vision-drivers](https://github.com/intel/vision-drivers). Without it no
`/dev/video*` node appears for the camera at all, so there is nothing for the
relay — or facelock — to open.

Working configuration (verified on a Dell XPS 14 with IPU7, issue #89):

1. Install the vendor stack (`intel-ipu6-camera`/`intel-ipu7-camera`,
   `v4l2-relayd`, `v4l2loopback-dkms`) and confirm the loopback node works:
   `gst-launch-1.0 v4l2src device=/dev/video50 ! fakesink`

2. Point facelock at the loopback node in `/etc/facelock/config.toml`:

   ```toml
   [device]
   path = "/dev/video50"
   ```

3. The relay's default `FORMAT=NV12` is supported natively. If you need a
   different format, set it in the v4l2-relayd config — but note that a bare
   caps string cannot be appended to `VIDEOSRC` (v4l2-relayd attaches its own
   `appsink` dynamically); end the pipeline with a `videoconvert` element and
   set `FORMAT=` instead:

   ```ini
   VIDEOSRC="icamerasrc device-name=<sensor> ! videoconvert"
   FORMAT=YUY2
   ```

4. With `v4l2loopback` option `exclusive_caps=1`, the loopback node flips to
   *Video Output* while no producer is streaming, and facelock intermittently
   sees "not a video capture device". Keep the relay's output side always
   active with a splash source:

   ```ini
   SPLASHSRC="videotestsrc is-live=true pattern=black"
   ```

#### IR sensors on IPU6/IPU7: not supported out of the box

These laptops ship an IR sensor for Windows Hello (Himax HM1092 on recent Dell
XPS models), but no **in-tree or Intel-shipped** Linux driver exists for it —
neither `intel/ipu6-drivers` nor `intel/ipu7-drivers` includes one, and the
`v4l2-relayd` path relays the RGB sensor only. On a stock distro,
`security.require_ir = true` (the default) cannot be satisfied on this
hardware.

Facelock classifying the relay node as non-IR is therefore correct, not a
detection bug: that pipeline really is RGB. Disabling `require_ir` does not
recover the IR sensor — it trades away the spoof resistance that IR provides,
leaving a camera that a printed photo or a phone screen can drive. See
`docs/security.md` §1 for what each IR-dependent check stops covering.

**Experimental community IR support exists.** The out-of-tree
[svp7500-camera-fix-pack](https://github.com/jibsta210/svp7500-camera-fix-pack)
(DKMS) has the HM1092 IR sensor streaming — including face unlock with the IR
flood illuminator — on SVP7500-bridged IPU7 laptops (verified on a Dell XPS 16
DA16260; see [intel/ipu7-drivers#26](https://github.com/intel/ipu7-drivers/issues/26)
for the full history). The illuminator side is already in mainline
(`intel_skl_int3472_discrete` registers `ir_flood_led` on kernels ≥ 7.1.4), and
kernel 7.2-rc1 gained an in-tree `intel_cvs` bridge driver. Facelock does not
yet consume that IR node (capture-node format and IR classification for
`hm1092` are tracked in issue #101) — treat this path as experimental until the
sensor driver lands upstream.

## Init System Support

### systemd (recommended)

Full support via D-Bus activation:
```bash
sudo facelock setup --systemd
```

Features:
- D-Bus activation (daemon starts on first D-Bus call)
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
| system-auth | Affects ALL auth — test sudo first |
| login | Console login — hard to recover if broken |
| sshd | SSH has no camera — always fails |

## Build Dependencies

### Runtime
- `pam` (Linux-PAM library)
- `gcc-libs` (C runtime)

### Build
- `rust` + `cargo` (1.88+)
- `clang` (for ONNX Runtime bindings)
- System headers: `libv4l-dev`, `libxkbcommon-dev`, `libpam0g-dev` (names vary by distro)

### Optional
- `tpm2-tss` — TPM2 support for embedding encryption
- `podman` or `docker` — container testing

## ONNX Runtime

Facelock uses the `ort` crate (Rust bindings for ONNX Runtime) and loads the
compatible shared library dynamically. Native packages either include the
reviewed CPU runtime or depend on the distribution runtime; source builds use
a compatible ONNX Runtime installed on the build host.

### Execution Providers

GPU support is runtime-only -- no special build flags needed. Install a GPU-enabled ONNX Runtime package and set `execution_provider` in config.

| Provider | Config | Runtime Requirement | Status |
|----------|--------|---------------------|--------|
| CPU | `execution_provider = "cpu"` | none (default) | Working |
| CUDA (NVIDIA) | `execution_provider = "cuda"` | CUDA toolkit + GPU-enabled ORT | Config ready, untested |
| ROCm (AMD) | `execution_provider = "rocm"` | ROCm runtime + GPU-enabled ORT | Config ready, untested |
| OpenVINO (Intel) | `execution_provider = "openvino"` | OpenVINO runtime + GPU-enabled ORT | Config ready, untested |

CPU is the default and only tested provider.
