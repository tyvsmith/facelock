# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

### Security

## [0.1.0] - 2026-05-17

Initial open-source release.

### Added

- **Core pipeline**: SCRFD face detection + ArcFace 512-dim embedding with ONNX Runtime
- **PAM module**: Thin cdylib with D-Bus daemon and oneshot subprocess modes
- **Daemon**: Persistent process with model caching, ~200ms warm auth latency
- **CLI**: Unified `facelock` binary — setup, enroll, test, preview, bench, audit, and more
- **Anti-spoofing**: IR camera enforcement, frame variance checks, landmark liveness detection
- **D-Bus**: System bus interface (`org.facelock.Daemon`) with deny-all policy and caller UID verification
- **GPU**: Runtime-selectable execution providers (CPU, CUDA, ROCm, OpenVINO) via `execution_provider` config — no compile-time flags
- **Setup wizard**: Interactive model-quality and inference-device selection, streaming download progress bar, only downloads the models actually selected in config
- **Status command**: Reports inference provider and ORT library location, enrolled face count for the current user, security posture (IR enforcement, liveness, `min_auth_frames`), and notification state (`73a5c00`)
- **Models**: Self-hosted ONNX assets distributed via GitHub release downloads (no third-party model fetches in the auth path)
- **Packaging**: deb, rpm, PKGBUILD (`facelock` and `facelock-git`), Nix flake, signed APT repository with two channels — `main` (TPM-enabled, Debian trixie+ / Ubuntu 25.04+) and `legacy` (non-TPM, Debian bookworm / Ubuntu 24.04) — systemd/D-Bus activation, OpenRC/runit/s6 (`c70999b`)
- **CI/CD**: Build/test/lint pipeline, TPM tests via swtpm, container PAM smoke tests, end-to-end `.deb` and `.rpm` package install validation
- **Documentation**: mdBook, man pages, ADRs, security posture assessment, threat model

### Security

- **Constant-time matching**: Embedding comparison via `subtle` crate to prevent timing side-channels
- **Encryption at rest**: AES-256-GCM software encryption for stored face embeddings
- **TPM key sealing**: Optional TPM-backed key protection for the encryption key
- **Model integrity**: SHA256 verification of ONNX model files at load time
- **Rate limiting**: 5 auth attempts per user per 60 seconds (default), enforced in daemon
- **D-Bus authorization**: Daemon verifies caller UID via `GetConnectionUnixUser` before executing methods
- **Enrollment restriction**: Root-required enrollment enforced in auth paths (`c01a655`)
- **PAM env hardening**: Hardened PAM environment handling to prevent injection (`c01a655`)
- **systemd hardening**: `ProtectSystem=strict`, `NoNewPrivileges`, `InaccessiblePaths`, and related service restrictions

### Fixed

- **PAM install output**: Conditional install messages — suppressed when PAM entry already present (`c12a970`)
- **PAM uninstall**: Uninstall now removes entries from all relevant PAM services, not just the primary one (`c12a970`)

[0.1.0]: https://github.com/tyvsmith/facelock/releases/tag/v0.1.0
