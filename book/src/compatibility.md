# Compatibility

The current delivery targets are x86_64 Linux systems with V4L2 and
Linux-PAM. Package validation covers Arch, Debian 13 (`trixie`), Ubuntu 26.04
LTS (`resolute`), and Fedora 43/44/45. RHEL is not in the supported matrix.
Source templates exist for OpenRC, runit, and s6, but that does not establish
package or hardware support for every distribution using those supervisors.

## Cameras and formats

Facelock classifies an IR node from its advertised format set or an exact
hardware quirk. It does not use a device name containing “IR” as evidence.
The current hardware validation record covers the Logitech BRIO 046d:085e IR
node using native GREY. Do not infer support for Intel RealSense or “Windows
Hello” cameras from their product category alone.

| Format | Authentication support |
|--------|------------------------|
| `GREY` | supported 8-bit grayscale path |
| `Y16` | conditional on a hardware-verified `y16_bit_depth` quirk from 8 through 16 |
| `YUYV`, `NV12`, `MJPG` | supported decode paths; normally RGB unless an exact quirk says otherwise |
| `Y8`, `Y10`, `Y12` | IR-classification evidence only; not decoded |
| raw Bayer and other formats | not supported |

The shipped RealSense Y16 quirks deliberately have no bit-depth evidence, so
authentication rejects those Y16 paths. A 16-bit V4L2 container does not prove
the sensor's meaningful bit depth. Auto-detection excludes devices with no
decodable format and reports the advertised formats.

RGB operation requires `security.require_ir = false` and is for development;
it does not provide the default IR boundary. See the canonical
[Compatibility](../../docs/compatibility.md) page for the IPU6/IPU7 relay
notes, exact negotiation order, and current validation evidence.

## Init and PAM

systemd with D-Bus activation is the packaged daemon path. On non-systemd
systems, use `daemon.mode = "oneshot"` or install one of the source-tree
OpenRC/runit/s6 templates after a source install. The daemon command must run
as root.

Test PAM on `sudo` first while retaining a root recovery shell. Shared stacks,
console login, and SSH are sensitive targets and require the CLI's explicit
`--allow-sensitive` gate. PAM service availability is distribution-specific;
use `facelock pam status` rather than assuming a path exists.

## Inference providers

CPU is the default and tested provider. CUDA, ROCm, and OpenVINO require a
matching ONNX Runtime build; configuration support is not evidence that a GPU
or a particular runtime package has been validated. The setup `auto` choice
inspects providers compiled into ONNX Runtime, not the hardware.
