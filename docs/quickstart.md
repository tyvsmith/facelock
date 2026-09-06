# Quickstart

## Published packages

As checked on 2026-09-06,
[v0.2.0](https://github.com/tyvsmith/facelock/releases/tag/v0.2.0) is the
latest stable release. The AUR entries and both APT suites serve it; the
production COPR does not yet.

The following SHA256 values are pinned from the release's
[MANIFEST.json](https://github.com/tyvsmith/facelock/releases/download/v0.2.0/MANIFEST.json).
Each command chain verifies the downloaded bytes before invoking the package
manager; stop if verification fails. These checks establish integrity against
the reviewed release, not an independent signing or build attestation.

For Debian 13 amd64, install the published package directly:

```bash
curl -fLO https://github.com/tyvsmith/facelock/releases/download/v0.2.0/facelock_0.2.0-1.deb13u1_amd64.deb &&
printf '%s  %s\n' '2554a1dcd4eca7bb1e3c2f2fbafbffa31d2c6a84210d32e9e221ece9e6e8dace' 'facelock_0.2.0-1.deb13u1_amd64.deb' | sha256sum --check - &&
sudo apt install ./facelock_0.2.0-1.deb13u1_amd64.deb
```

For Ubuntu 26.04 amd64, use the corresponding suite build:

```bash
curl -fLO https://github.com/tyvsmith/facelock/releases/download/v0.2.0/facelock_0.2.0-1.ubuntu26.04.1_amd64.deb &&
printf '%s  %s\n' 'ee4bc06963752bf39e888c2d10ba5229f256978d0b14b134463e19525b323d79' 'facelock_0.2.0-1.ubuntu26.04.1_amd64.deb' | sha256sum --check - &&
sudo apt install ./facelock_0.2.0-1.ubuntu26.04.1_amd64.deb
```

The dots in those download filenames are GitHub-safe stored names.
`MANIFEST.json` records the native names `facelock_0.2.0-1~deb13u1_amd64.deb`
and `facelock_0.2.0-1~ubuntu26.04.1_amd64.deb`, and Debian reports the
installed versions `0.2.0-1~deb13u1` and `0.2.0-1~ubuntu26.04.1`. The bytes and
the checksum are the same either way. Fedora 44 x86_64 users can install the
direct release RPM:

```bash
curl -fLO https://github.com/tyvsmith/facelock/releases/download/v0.2.0/facelock-0.2.0-1.fc44.x86_64.rpm &&
printf '%s  %s\n' '65cf7d3167979daa8d5af6f0e5c96c25d21c6b27b07a47d8dd212007fab725c5' 'facelock-0.2.0-1.fc44.x86_64.rpm' | sha256sum --check - &&
sudo dnf install ./facelock-0.2.0-1.fc44.x86_64.rpm
```

The separately downloadable `facelock`, PAM module, and polkit-agent binaries
are release components, not a complete installation. They do not install the
configuration, models, service and D-Bus policy, PAM layout, or required shared
libraries; use a native package or the source installation path instead.

Arch users can install the stable source-build AUR package. `facelock-bin` is
the prebuilt alternative and `facelock-git` follows development; all three AUR
entries served version 0.2.0-1 on 2026-09-06, and each declares the
`onnxruntime` dependency the binary loads at runtime.

```bash
yay -S facelock
```

Debian 13 (`trixie`) and Ubuntu 26.04 LTS (`resolute`) on amd64 are the 0.2.0
stable APT targets. Both suites are published at
`https://tysmith.me/facelock/apt` and served 0.2.0 when checked on 2026-09-06:

| Supported target | Suite | Architecture | Required capability |
|------------------|-------|--------------|---------------------|
| Debian 13 | `trixie` | amd64 | TPM |
| Ubuntu 26.04 | `resolute` | amd64 | TPM |

Install the archive keyring, write a source entry naming your codename, then
install the package:

```bash
sudo curl -fsSL https://tysmith.me/facelock/apt/tysmith-archive-keyring.gpg \
  -o /usr/share/keyrings/tysmith-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/tysmith-archive-keyring.gpg] https://tysmith.me/facelock/apt trixie facelock" | sudo tee /etc/apt/sources.list.d/facelock.list
sudo apt update
sudo apt install facelock
```

On Ubuntu 26.04, use `resolute` instead of `trixie` in the source line. The
keyring holds one rsa4096 key, `Ty Smith (Package Signing)
<packages@m.tysmith.me>`, fingerprint
`E7F8A4C424C6D59BD38536B536A81FCD934C17CE`, checked on 2026-09-06. Confirm it
with `gpg --show-keys` before trusting the source; `signed-by` scopes that key
to this repository alone, and `apt update` rejects the suite if the archive
signature does not match.

Existing v0.1.4 entries naming `main` or `legacy` keep working until 0.3.0:
`main` maps to the Trixie package set and `legacy` serves signed empty indexes.
At 0.3.0, `apt update` fails until the entry is removed. Rewrite those entries
to your operating system's codename now.

The production COPR targets Fedora 43, 44, and 45 only. On 2026-09-06 it served
v0.1.3 on all three, behind stable v0.2.0, so the commands below install 0.1.3
rather than the current release. Use the direct RPM above until COPR catches
up. `tyvsmith/facelock-testing` is a staging/candidate project, not a stable
channel. RHEL is not in the supported matrix. Fedora's `dnf copr` command comes
from `dnf5-plugins`; installing it is idempotent, and minimal installations may
not include it.

```bash
sudo dnf install dnf5-plugins
sudo dnf copr enable tyvsmith/facelock
sudo dnf install facelock
```

After any package install, keep a separate root shell open and run the wizard:

```bash
sudo facelock setup
sudo facelock test
```

The wizard offers camera selection, model download, encryption, daemon setup,
enrollment, and PAM configuration. If enrollment is selected there, do not run
a redundant initial `facelock enroll`. `facelock test` may exit zero when no
scan ran or when a completed scan did not match; verify the printed result,
then test `sudo` from a new terminal before closing the recovery shell.

## Build from source

### Source-build prerequisites

Facelock's minimum supported Rust version is 1.88+. `rust-toolchain.toml` selects
Rust 1.95 when Cargo is a rustup proxy; distro-provided Cargo does not interpret
that file and uses its installed compiler instead. The target distro versions
below all satisfy the 1.88 floor. ONNX Runtime is a separate runtime shared
library: compiling the Rust crate does not install it.

On Arch Linux, the `rust` package provides Cargo. Use the exact CPU runtime
package, `onnxruntime-cpu`; `onnxruntime` is a virtual provide shared by the
CPU, CUDA, and ROCm variants.

```bash
sudo pacman -Syu --needed base-devel git rust just clang gettext pkgconf pam v4l-utils wayland libxkbcommon tpm2-tss onnxruntime-cpu
```

On Debian 13, add `deb http://deb.debian.org/debian trixie-backports main` to a
root-owned file under `/etc/apt/sources.list.d/`. Trixie's native Rust 1.85 is
below the project floor; install both Rust packages explicitly from backports.

```bash
sudo apt update
sudo apt install build-essential git just clang gettext pkg-config libpam0g-dev libv4l-dev libwayland-dev libxkbcommon-dev libtss2-dev
sudo apt install -t trixie-backports rustc cargo
```

Ubuntu 26.04's native Rust satisfies the floor:

```bash
sudo apt update
sudo apt install build-essential git rustc cargo just clang gettext pkg-config libpam0g-dev libv4l-dev libwayland-dev libxkbcommon-dev libtss2-dev
```

Fedora 43/44/45 provides both the compiler dependencies and a system ONNX
Runtime. These are the names used by the RPM build, plus `just` for this
repository's commands:

```bash
sudo dnf install git rust cargo just gcc gcc-c++ clang-devel gettext pkgconf-pkg-config pam-devel libv4l-devel wayland-devel libxkbcommon-devel tpm2-tss-devel onnxruntime
```

Debian 13 publishes `libonnxruntime1.21`, and Ubuntu 26.04 publishes
`libonnxruntime1.23`. Their multiarch library paths and versioned SONAMEs
(`libonnxruntime.so.1.21` and `libonnxruntime.so.1.23`) are incompatible with
Facelock's current trusted loader, which requires `libonnxruntime.so.1` as the
SONAME and searches fixed runtime directories rather than multiarch paths.
A source build can compile and run non-inference commands such as `--help`,
but enrollment, authentication, preview, and inference benchmarks require a
separately installed compatible ONNX Runtime 1.20+ in a trusted runtime
location. Merely adding a symlink does not repair a mismatched SONAME.
The published `.deb` packages bundle the compatible CPU runtime 1.20.1;
this is not supplied by Cargo.

```bash
just build
target/debug/facelock --help
```

`just build` does not install anything or put `facelock` on `PATH`; use the
explicit `target/debug/facelock` path. `target/debug/facelock --help` does not
load ONNX Runtime and is not an inference check.

For camera development, first provide the checksum-verified model files and
use the development configuration explicitly:

```bash
just link-models
sudo target/debug/facelock --config "$PWD/dev/config.toml" devices
sudo target/debug/facelock --config "$PWD/dev/config.toml" enroll --skip-setup-check
sudo target/debug/facelock --config "$PWD/dev/config.toml" test
```

These commands are privileged because the management CLI enforces its normal
root gate even with `dev/config.toml`. `FACELOCK_CONFIG` cannot replace the
explicit flag under `sudo`: all effective-UID-0 processes ignore that variable.
The development configuration uses direct/oneshot operation and temporary
state; it is not an installed PAM setup.

On Arch, after installing `onnxruntime-cpu`, a source-based system install is:

```bash
just install
sudo facelock setup
sudo facelock test
```

`just install` builds as the invoking user and prompts for sudo only for the
file installation. It installs the binary, PAM module, service units, D-Bus
policy, configuration and supporting assets, but does not edit any PAM service.
Only the later wizard or `facelock pam add` does that. This source installer
uses the Arch/Debian `/lib/security` PAM layout. Use the distro package rather
than this installer on Fedora, and do not treat a Debian/Ubuntu source install
as inference-capable until a trusted ONNX Runtime has been installed separately.

### NixOS source-tree module

Facelock is not in nixpkgs and Nix is not a published package channel or a row
in the supported release matrix. The repository does ship a flake and NixOS
module under `dist/nix`. Release CI gates flake evaluation, while the actual Nix
build remains advisory and the derivation disables its test phase. The flake
has no checked-in `flake.lock`, so evaluation resolves network inputs and is
not a locked, reproducible package publication.

The source-tree module exports `nixosModules.default`. Its public options are
`services.facelock.enable`, `services.facelock.package`, and
`services.facelock.config`; the last maps directly to Facelock's TOML
configuration. When enabled, it installs the selected package, writes the
configuration, enables the daemon, and adds Facelock to the `sudo` PAM service.

This interface is experimental and is not currently a usable authentication
installation. The derivation places ONNX Runtime under its Nix store output,
but privileged Facelock processes search only the trusted `/usr/lib` and
`/usr/lib64` roots and intentionally ignore `ORT_DYLIB_PATH`. The module also
does not provision the model files or the encryption key needed for enrollment.
Do not enable its PAM rule on a system that depends on it until those gaps are
fixed and the complete NixOS path has been validated.

## Explore safely

After system installation and setup:

```bash
sudo facelock devices
sudo facelock list
sudo facelock preview --json
sudo facelock status
sudo facelock bench camera-reopen
```

Camera commands touch real hardware. See [Testing Safety](testing-safety.md)
before changing PAM, and [Developer Commands](developer-commands.md) for the
full validation inventory.

## Package lifecycle and retained data

For a source installation only, use the source checkout's removal recipe:

```bash
just uninstall
```

For a native package installation, remove Facelock through the same package
manager that installed it; do not use the source uninstaller to delete
package-owned files behind the package manager.

Ordinary package removal and `just uninstall` preserve the face database,
encryption keys, models, enrollment markers, logs, snapshots, and setup state.
Debian purge removes only provably safe entries under the compiled Facelock
roots; safety refusals are reported without stranding package-manager state.
Unsafe or externally configured remnants are retained for manual review.
To inspect the supported bounded erasure path while Facelock is still installed:

```bash
sudo facelock data purge --dry-run
sudo facelock data purge --allow-destruction
```

Purge admits only safe entries inside the three compiled Facelock roots. It
does not follow links, cross mounts, remove unsafe/wrong-owner objects, or chase
configured paths outside those roots. Its report may therefore list remnants
that require manual review; it never promises whole-disk erasure. See
[Package Lifecycle Ownership](contracts.md#package-lifecycle-ownership).

## Configuration highlights

The installed file is `/etc/facelock/config.toml`; source development uses
`dev/config.toml`. The default encryption method is `keyfile`, IR is required,
and plaintext enrollment is refused unless explicitly enabled. See the
[Configuration Reference](configuration.md).
