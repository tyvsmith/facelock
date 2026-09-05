# Quick Start

## Release availability summary

v0.1.4 is the latest published stable release. The v0.2.0-alpha.1 tag exists,
but no corresponding GitHub Release is currently published. Prereleases do not
enter the stable AUR, APT, or production COPR channels.

Install an actually published package, or build the current tree:

```bash
just build
target/debug/facelock --help
```

The planned 0.2.0 APT suites (`trixie` and `resolute`) are not currently served;
a clean `trixie` check returns 404. The production Fedora COPR currently serves
v0.1.3, while stable is v0.1.4; the testing COPR is a candidate channel. RHEL
is not in the supported matrix.
See the canonical [Quickstart on GitHub](https://github.com/tyvsmith/facelock/blob/main/docs/quickstart.md)
for repository setup commands and exact channel limitations.

## System installation

For a source system install:

```bash
just install
sudo facelock setup
sudo facelock test
```

`just install` installs files but does not edit PAM. The setup wizard offers
enrollment and PAM configuration, so do not repeat enrollment in the initial
sequence. Keep a root shell open while testing. A zero exit from
`facelock test` means the command completed, not that recognition succeeded;
inspect its output and then verify a new `sudo` session.

## Development

For non-installing development, use the built path and explicit development
configuration. Management commands remain root-gated, and root ignores
`FACELOCK_CONFIG`:

```bash
just link-models
sudo target/debug/facelock --config "$PWD/dev/config.toml" devices
sudo target/debug/facelock --config "$PWD/dev/config.toml" enroll --skip-setup-check
sudo target/debug/facelock --config "$PWD/dev/config.toml" test
```

## Package lifecycle and retained data

Ordinary uninstall preserves retained biometric state. Preview the fixed-root
purge while the CLI is installed:

```bash
sudo facelock data purge --dry-run
sudo facelock data purge --allow-destruction
```

Debian purge removes only provably safe entries under the compiled Facelock
roots and reports safety refusals without stranding package-manager state.
Unsafe or externally configured remnants are retained for manual review;
cross-mount and wrong-owner objects are likewise reported rather than
traversed or removed. See
[Package Lifecycle Ownership](contracts.md#package-lifecycle-ownership)
and [Testing](testing.md).
