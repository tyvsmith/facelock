# WS2: Distribution Packaging — Spec

**Status:** Complete

## Changes Made

### Arch Linux
- Updated `dist/PKGBUILD` — dual license, both LICENSE files installed

### Debian/Ubuntu
- `dist/debian/control` — source + binary package metadata
- `dist/debian/rules` — debhelper with cargo build
- `dist/debian/changelog` — initial 0.1.0-1
- `dist/debian/copyright` — DEP-5 format, MIT OR Apache-2.0
- `dist/debian/postinst` — sysusers, tmpfiles, socket enable
- `dist/debian/prerm` — disable units, pam-auth-update --remove
- `dist/debian/compat` — debhelper compat 13
- `dist/debian/source/format` — 3.0 (native)

### Fedora/RHEL
- `dist/facelock.spec` — RPM spec with systemd macros

### NixOS
- `dist/nix/flake.nix` — flake with package + module
- `dist/nix/default.nix` — rustPlatform.buildRustPackage derivation
- `dist/nix/module.nix` — NixOS module with services.facelock.enable

## Verification

Build each package in a clean container/chroot, install, run `facelock status`.
