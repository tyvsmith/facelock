# Compatibility

## System Requirements

| Component | Requirement |
|-----------|-----------|
| OS | Linux with V4L2 and Linux-PAM |
| Architecture | x86_64 for current published artifacts |
| Rust | 1.88+ (edition 2024) |
| Camera | V4L2-compatible (USB webcam, built-in IR) |
| PAM | Linux-PAM |

## Tested Distributions

Debian-family release support is exactly Debian 13 (Trixie) and Ubuntu 26.04
LTS (Resolute). Other Debian and Ubuntu releases are unsupported.

| Distribution | Init System | Mode | Status |
|-------------|-------------|------|--------|
| Arch Linux | systemd | daemon + D-Bus activation | Primary target |
| Arch Linux | systemd | oneshot | Tested |
| Debian 13 (Trixie) | systemd | daemon + D-Bus activation | Booted package gate |
| Ubuntu 26.04 LTS (Resolute) | systemd | daemon + D-Bus activation | Booted package gate |
| Fedora 43 and 44 | systemd | daemon + D-Bus activation | Required full-lifecycle COPR targets |
| Fedora 45 | systemd | daemon + D-Bus activation | Required build/install/runtime target |
| Container (Arch) | none | daemon (manual) | CI-tested |
| Container (Arch) | none | oneshot | CI-tested |

No RHEL release target is claimed. OpenRC, runit, and s6 service templates are
provided in the source tree, but their presence is not a claim that a named
distribution has passed package or hardware validation.

### Expected to Work (untested)

No additional distribution is claimed as expected to work. This section is
kept explicit so source templates and plausible compatibility are not mistaken
for tested support.

## Camera Compatibility

### IR Cameras (recommended)

IR cameras provide anti-spoofing protection. Facelock auto-detects IR cameras from the pixel formats they report — a node that enumerates *only* IR-typical mono formats (GREY, Y8, Y10, Y12, Y16) with no color format (YUYV/MJPG) mixed in — or from a hardware quirk that matches the device. The device name is never used to classify a camera as IR.

Hardware-validated in the current alpha test record:

- Logitech BRIO 046d:085e IR node, native GREY (#162: 33/33 integration and
  27/27 oneshot scenarios)

Intel RealSense and other Windows Hello IR devices are recognized by the
classification/quirks paths, but their Y16 authentication support is
conditional on a verified sensor depth as described below. There is no Y16
hardware validation record yet.

### RGB Cameras (development only)

RGB cameras require `security.require_ir = false` and lose the default IR
boundary. Frame variance and any enabled landmark-liveness check still apply,
but they do not make RGB authentication resistant to presentation attacks;
use RGB only for development and testing.

### Format Support

| Format | Support | Notes |
|--------|---------|-------|
| MJPG | Full | Most common USB camera format |
| YUYV | Full | Raw format, converted to RGB |
| NV12 | Full | Semi-planar 4:2:0; common on Intel IPU processed cameras |
| GREY | Full | IR cameras, replicated to RGB |
| Y16 | Conditional for authentication | 16-bit IR grayscale; auth requires a valid, hardware-verified `y16_bit_depth` quirk (8..=16) |
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

Y16 has a separate scale-provenance gate because the absolute IR texture
threshold is meaningful only at a known 8-bit conversion scale. Facelock
derives expected provenance from the selected normalized FourCC before open
and checks the actual negotiated FourCC again after open. Actual Y16 with a
missing or invalid `y16_bit_depth` is rejected recoverably before auth capture,
even when `security.require_ir = false`; this setting cannot bypass the scale
gate. GREY remains an ordinary verified 8-bit path. Scene-based calibration is
available only for non-auth Y16 conversion and never becomes authentication
evidence.

The shipped Intel RealSense Y16 quirks intentionally omit `y16_bit_depth`:
their format selection and IR classification remain useful, but authentication
rejects them until hardware evidence supports an 8..=16 value. Do not infer
sensor depth from the 16-bit V4L2 container alone.

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

   [security]
   require_ir = false # processed relay is RGB; development/testing only
   ```

   The default `require_ir = true` correctly refuses this RGB relay. Disabling
   it is required to authenticate through the relay and weakens presentation-
   attack resistance; it does not expose or recover the laptop's IR sensor.

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

The affected Dell configurations include a Himax HM1092 IR sensor for Windows
Hello. The RGB `v4l2-relayd` recipe above does not expose that sensor. It cannot
satisfy `security.require_ir = true` (the default) merely by selecting the
processed RGB node; a working separate IR capture path is required.

Facelock classifying the relay node as non-IR is therefore correct, not a
detection bug: that pipeline really is RGB. Disabling `require_ir` does not
recover the IR sensor — it removes the IR boundary while the remaining passive
checks provide no guarantee against a printed photo or phone-screen replay. See
`docs/security.md` §1 for what each IR-dependent check stops covering.

**Experimental community IR support exists.** The out-of-tree
[svp7500-camera-fix-pack](https://github.com/jibsta210/svp7500-camera-fix-pack)
reports HM1092 IR streaming and Howdy face unlock on its author's Dell XPS 16
DA16260 (upstream checked 2026-09-05). That is not a Facelock validation result
or a guarantee for other IPU7 laptops. Driver, illuminator and bridge support
depends on the precise sensor, kernel and module combination; consult that
project's current hardware checks and support matrix rather than inferring
support from a kernel version alone. Facelock capture-node format and IR
classification work for `hm1092` is tracked in issue #101; treat this path as
experimental until the complete Facelock capture path is established.

## Init System Support

### systemd (recommended)

Full support via D-Bus activation:
```bash
sudo facelock setup --systemd
```

Features:
- D-Bus activation (daemon starts on first D-Bus call)
- Optional idle timeout (`daemon.idle_timeout_secs`; zero disables it by default)
- Service hardening (ProtectSystem, NoNewPrivileges, etc.)
- Automatic restart on failure

### Non-systemd

Use oneshot mode (no daemon needed):
```toml
[daemon]
mode = "oneshot"
```

After a source install, a supervisor may run `/usr/bin/facelock daemon` as root;
the daemon itself refuses non-root execution. Reviewed templates are supplied
for OpenRC (`dist/openrc/facelock-daemon`), runit (`dist/runit/`), and s6
(`dist/s6/facelock-daemon/`). They are not installed by the systemd-oriented
packages, and there is no dinit template. Install and enable the one matching
the local supervisor, preserving its root ownership and state-directory setup.

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
| sshd | Remote authentication is refused by the default `abort_if_ssh = true` policy; do not weaken that boundary |

## Build Dependencies

Use the distro-specific [source prerequisites](quickstart.md#build-from-source).
Build-time headers and tools are separate from the shared libraries needed on
the installed machine. In particular, source installation needs a trusted,
compatible ONNX Runtime; compiling the workspace does not install that runtime.
The `just install` release build enables TPM support and therefore needs
TPM2-TSS development files, even when the eventual configuration uses keyfile
encryption. The supplied container recipes invoke Podman, not Docker.

## ONNX Runtime

Facelock uses the `ort` crate (Rust bindings for ONNX Runtime) and loads the
compatible shared library dynamically. Native packages either include the
reviewed CPU runtime or depend on the distribution runtime; source builds use
a compatible ONNX Runtime installed on the build host.

### Execution Providers

GPU support is runtime-only -- no special build flags needed. Install a GPU-enabled ONNX Runtime package and set `execution_provider` in config.

| Provider | Config | Runtime Requirement | Status |
|----------|--------|---------------------|--------|
| CPU | `execution_provider = "cpu"` | compatible CPU ONNX Runtime (default provider) | Working |
| CUDA (NVIDIA) | `execution_provider = "cuda"` | CUDA toolkit + GPU-enabled ORT | Config ready, untested |
| ROCm (AMD) | `execution_provider = "rocm"` | ROCm runtime + GPU-enabled ORT | Config ready, untested |
| OpenVINO (Intel) | `execution_provider = "openvino"` | OpenVINO runtime + GPU-enabled ORT | Config ready, untested |

CPU is the default and only tested provider.
