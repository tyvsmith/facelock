# AGENTS.md — Facelock

## Project Overview

Facelock is a Linux face authentication PAM module written in Rust. Single unified binary (`facelock`) handles CLI, daemon, one-shot auth, and benchmarks. The PAM module (`pam_facelock.so`) is a thin client that either connects to the daemon or spawns `facelock auth`.

## Repository Structure

Cargo workspace with 11 crates:

| Crate | Type | Purpose |
|-------|------|---------|
| `facelock-core` | lib | Config, types, errors, D-Bus interface, traits |
| `facelock-camera` | lib | V4L2 capture, auto-detection, preprocessing |
| `facelock-face` | lib | ONNX inference (SCRFD + ArcFace) |
| `facelock-store` | lib | SQLite face embedding storage |
| `facelock-daemon` | lib | Auth/enroll logic, rate limiting, liveness, audit |
| `facelock-cli` | bin | Unified CLI (`facelock` binary) |
| `pam-facelock` | cdylib | PAM module (libc + toml + serde + zbus only) |
| `facelock-tpm` | lib | Optional TPM sealing, software AES-256-GCM encryption |
| `facelock-polkit` | bin | Polkit authentication agent (`facelock-polkit-agent`) |
| `facelock-test-support` | lib | Mocks and fixtures for testing |
| `facelock-bench` | bin | Standalone benchmarks (not in workspace members) |

## Build & Verify

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo run --bin facelock -- --help
```

## Core Rules

- Do not change binary names, paths, config keys, database schema, or auth semantics without updating `docs/contracts.md`.
- Keep the PAM module free of heavy dependencies (no ort, no v4l, no facelock-core).
- Keep all inference local. No cloud services, no runtime model downloads in the auth path.
- Prefer minimal dependencies and clear crate boundaries.

## Security Rules

- **Read `docs/security.md`** before implementing any auth-related code.
- `security.require_ir` defaults to **true**. Never weaken this default.
- Frame variance checks must be in the auth path.
- Model files SHA256-verified at load time.
- D-Bus system bus policy restricts access to the daemon interface.
- D-Bus message size limits enforced by the bus daemon.
- PAM module logs all auth attempts to syslog.
- Database and model files have restrictive permissions (640/644, root:facelock).
- Rate limiting enforced in daemon (5 attempts/user/60s default).

## Code Style

- `thiserror` for library error types, `anyhow` in binaries.
- Return `Result<T>` over panicking. Never `unwrap()` in library code.
- `tracing` for structured logging. Control verbosity via `RUST_LOG` env filter.
- `#[cfg(test)]` modules in each source file for tests.

## Dependency Rules

Key dependencies per crate (see each `Cargo.toml` for full list):

| Crate | Key Dependencies |
|-------|-------------|
| facelock-core | serde, toml, thiserror, tracing, subtle, zeroize, zvariant |
| facelock-camera | facelock-core, v4l, image, tracing |
| facelock-face | facelock-core, ort, ndarray, image, sha2 |
| facelock-store | facelock-core, rusqlite (bundled), bytemuck |
| facelock-daemon | facelock-core, facelock-camera, facelock-face, facelock-store, facelock-tpm, signal-hook, nix |
| facelock-cli | facelock-core + all above, clap, reqwest, zbus, tokio, dialoguer |
| pam-facelock | **libc, toml, serde, zbus ONLY** |
| facelock-tpm | facelock-core, tracing, aes-gcm, rand, zeroize, tss-esapi (optional) |
| facelock-polkit | facelock-core, zbus, tokio, nix, anyhow |

## Testing Strategy

| Tier | What | How |
|------|------|-----|
| 1 | Unit tests | `cargo test --workspace` |
| 2 | Hardware tests | `cargo test --workspace -- --ignored` |
| 3 | Container PAM smoke | `just test-pam` |
| 3b | Container E2E (daemon) | `just test-integration` |
| 3c | Container E2E (oneshot) | `just test-oneshot` |
| 4 | VM testing | Disposable VM with snapshots |
| 5 | Host PAM | After tiers 3-4, with root shell backup |

**Never** install `pam_facelock.so` or edit `/etc/pam.d/*` on the host until container tests pass.

## Workspace Conventions

- Version declared once in root `Cargo.toml`, inherited via `version.workspace = true`.
- Inter-crate deps use relative paths (`path = "../facelock-core"`).
- Release profile: LTO + single codegen unit + strip.
