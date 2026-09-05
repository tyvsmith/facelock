# Quickstart

## Published packages

v0.1.4 is the latest published stable release. Prerelease tags do not enter
the stable AUR, APT, or production COPR channels. The v0.2.0-alpha.1 tag exists
but currently has no corresponding GitHub Release; a checkout, staged package,
or local rebuild is not proof that a release artifact was published.

Arch users can install the stable AUR package:

```bash
yay -S facelock
```

Debian 13 (`trixie`) and Ubuntu 26.04 LTS (`resolute`) on amd64 are the exact
0.2.0 stable APT targets. They are not currently served: the clean-system
check of the documented `trixie` Release URL returns 404. Do not add either
source until a stable 0.2.0 release publishes it. Existing v0.1.4 sources used
the transitional `main`/`legacy` names; the current alpha tree is a source
build, not an APT publication.

The production COPR targets Fedora 43, 44, and 45 only. It currently serves
v0.1.3, behind stable v0.1.4. `tyvsmith/facelock-testing` is a staging/candidate
project, not a stable channel. RHEL is not in the supported matrix.

```bash
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

Use Rust 1.88 or newer and `just`. Build dependencies follow the package
manifests: PAM and V4L2 development headers, Clang, Wayland and libxkbcommon
headers, gettext, pkg-config, and TPM2-TSS headers. The system ONNX Runtime is a
runtime/package concern; Cargo's source build obtains the pinned runtime crate
dependency.

```bash
just build
target/debug/facelock --help
just check
```

`just build` does not install anything or put `facelock` on `PATH`; use the
explicit `target/debug/facelock` path. Unit and static checks need neither a
camera nor host authentication changes.

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

For a source-based system install:

```bash
just install
sudo facelock setup
sudo facelock test
```

`just install` builds as the invoking user and prompts for sudo only for the
file installation. It installs the binary, PAM module, service units, D-Bus
policy, configuration and supporting assets, but does not edit any PAM service.
Only the later wizard or `facelock pam add` does that.

## Explore safely

After models and a development or installed configuration exist:

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

```bash
just uninstall
```

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
