# Competitive Analysis: Ours vs Sovren Visage

**Date:** 2026-03-11
**Repo:** https://github.com/sovren-software/visage (52 commits, 3 contributors, Feb 21-25 2026)

---

## Project Vitals

| | **Sovren Visage** | **Ours** |
|---|---|---|
| Created | Feb 21, 2026 | Earlier (pre-dates sovren) |
| Version | 0.3.0 | 0.1.0 |
| Commits | 52 | 150+ |
| Rust LOC | 5,109 | 14,590 |
| Crates | 6 | 9 |
| License | MIT | MIT OR Apache-2.0 |
| Rust edition | 2021 / MSRV 1.75 | 2024 / MSRV 1.85 |
| Agent-built | Yes (stated in strategy) | Yes |

---

## Architecture Decisions

| Decision | **Sovren** | **Ours** | **Edge** |
|---|---|---|---|
| IPC | D-Bus (zbus) | Custom Unix socket + bincode | Sovren — D-Bus is the Linux standard (fprintd model) |
| Async runtime | Tokio | Synchronous / blocking | Sovren — cleaner separation (overkill for use case) |
| PAM module deps | zbus + libc (~5-10 MB) | libc + toml + serde (~600 KB) | **Ours** — much thinner, less attack surface |
| PAM failure mode | PAM_IGNORE always | PAM_IGNORE | Tie |
| Daemon architecture | Engine on OS thread + async D-Bus | Single-threaded blocking loop | Sovren slightly cleaner |
| Config format | Environment variables only (~12) | TOML file + env override (30+ keys, 8 sections) | **Ours** — far more maintainable |
| Camera auto-detect | No (hardcoded `/dev/video2`) | Yes (prefers IR cameras) | **Ours** |
| Pixel formats | GREY, YUYV, Y16 | Same via v4l crate | Tie |

---

## Security

| Feature | **Sovren** | **Ours** | **Edge** |
|---|---|---|---|
| IR enforcement default | Not enforced | Enforced (`require_ir = true`) | **Ours** — critical anti-spoofing |
| Anti-spoofing method | Passive liveness (eye micro-saccade, 0.8px threshold) | Multi-frame variance requirement | Sovren more targeted |
| Embedding encryption | AES-256-GCM at rest (.key file, 0600) | TPM sealing (hardware-bound, feature-gated) | **Ours** stronger design; Sovren ships today |
| Constant-time comparison | Yes (always iterates all models) | No | **Sovren** — prevents timing side-channels |
| Rate limiting | 5/60s + 5-min lockout | 5/60s (configurable) | Tie |
| systemd hardening | ProtectSystem=strict, DeviceAllow, NoNewPrivileges | Minimal (ONNX breaks many directives) | Sovren ships more |
| Model verification | SHA-256 at download + startup (fail-closed) | SHA-256 at download + load time | Tie |
| Access control | D-Bus system bus policy file | SO_PEERCRED on Unix socket | Both valid |
| FFI safety | catch_unwind around PAM FFI | Similar | Tie |

---

## Features

| Feature | **Sovren** | **Ours** | **Edge** |
|---|---|---|---|
| Daemon mode | Yes | Yes | Tie |
| Oneshot/direct mode | No (daemon required) | Yes (PAM fallback) | **Ours** |
| Socket activation | No (always running) | Yes (systemd on-demand) | **Ours** |
| Camera auto-detect | No | Yes (prefers IR) | **Ours** |
| IR emitter control | Native UVC + quirk DB | No | **Sovren** |
| Model choices | 2 (SCRFD 10G + ArcFace R50) | 4 (SCRFD 2.5G/10G + R50/R100) | **Ours** |
| Multi-embedding enrollment | Single per enroll | Multiple per model | **Ours** |
| Live preview | No | Wayland + text-only | **Ours** |
| Setup wizard | Basic quickstart script | Full TUI (dialoguer) | **Ours** |
| TPM support | No | Yes (tss-esapi) | **Ours** |
| Desktop notifications | No | Yes (D-Bus + terminal) | **Ours** |
| Snapshot capture | No | Yes (success/failure/all) | **Ours** |
| Benchmarking CLI | No | Yes (`bench` command) | **Ours** |
| SSH/lid detection | No | Yes (abort_if_ssh, abort_if_lid_closed) | **Ours** |
| Suspend/resume | Dedicated systemd unit | Not explicit | **Sovren** |
| CLAHE preprocessing | Custom implementation (~150 LOC) | Via image crate | Sovren — no extra dep |
| Dark frame rejection | Histogram-based | Not explicit | Sovren |
| Warmup frame discard | Yes (default 4 frames) | Not explicit | Sovren |

---

## Packaging & Distribution

| Platform | **Sovren** | **Ours** |
|---|---|---|
| Debian/Ubuntu | .deb via cargo-deb (tested lifecycle) | .deb via debhelper (untested) |
| Arch | AUR PKGBUILD | PKGBUILD (more tested) |
| Fedora | None | RPM spec file |
| NixOS | Flake module | Flake + module.nix |
| CI/CD | Basic (fmt, clippy, test, deb) | Release workflow, RPM/deb, TPM tests, pages |

---

## Documentation & Polish

| Aspect | **Sovren** | **Ours** | **Edge** |
|---|---|---|---|
| Architecture docs | Good | Good | Tie |
| Threat model | Detailed, tiered | Detailed | Tie |
| ADRs | 11 decision records | None | **Sovren** |
| Operations guide | Yes | Troubleshooting guide | Tie |
| Hardware compat | Tiered matrix | Compatibility doc | Tie |
| Strategy doc | Yes (positioning, roadmap) | No | Sovren |
| Website | No | Landing page + mdBook | **Ours** |
| SECURITY.md | Yes (vuln reporting) | No | Sovren |
| Config reference | Scattered in env vars | Consolidated doc | **Ours** |

---

## What to Steal from Sovren

1. **Constant-time embedding comparison** — trivial to add, real security value
2. **Passive liveness (landmark stability)** — better signal than frame variance alone; we already have SCRFD landmarks
3. **Suspend/resume systemd unit** — solves real camera stale-handle bug
4. **SECURITY.md** — vulnerability reporting policy for open source
5. **Warmup frame discard** — skip N frames for camera AGC/AE stabilization
6. **Dark frame rejection** — skip frames too dark to process
7. **ADRs** — architecture decision records show engineering rigor

---

## Assessment

Sovren is tighter (5K LOC) and ships a few things we don't (D-Bus, passive liveness, software encryption at rest, constant-time comparison). But ours is a superset in features: oneshot mode, socket activation, camera auto-detect, TOML config, preview, TPM, wizard, notifications, snapshots, SSH/lid detection, more model choices, multi-embedding enrollment.

**Recommendation:** Continue building, rename to avoid conflict. Sovren plans a "Summer 2026" public launch and has the name established on GitHub.
