# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-03-15

Initial open-source release.

### Added

- **Core pipeline**: SCRFD face detection + ArcFace 512-dim embedding with ONNX Runtime
- **PAM module**: Thin cdylib with D-Bus daemon and oneshot subprocess modes
- **Daemon**: Persistent process with model caching, ~200ms warm auth latency
- **CLI**: Unified `facelock` binary — setup, enroll, test, preview, bench, audit, and more
- **Anti-spoofing**: IR camera enforcement, frame variance checks, landmark liveness detection
- **Security**: Constant-time matching (subtle), AES-256-GCM encryption at rest, optional TPM key sealing
- **D-Bus**: System bus interface (`org.facelock.Daemon`) with deny-all policy and caller UID verification
- **systemd**: Service hardening (ProtectSystem, NoNewPrivileges, InaccessiblePaths)
- **GPU**: Feature-gated CUDA/TensorRT via system ONNX Runtime (`just install-cuda`)
- **Packaging**: deb, rpm, PKGBUILD, Nix flake, systemd/D-Bus activation, OpenRC/runit/s6
- **CI/CD**: Build/test/lint pipeline, TPM tests via swtpm, container PAM smoke tests
- **Documentation**: mdBook, man pages, ADRs, security posture assessment, threat model

[0.1.0]: https://github.com/tyvsmith/facelock/releases/tag/v0.1.0
