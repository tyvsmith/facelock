# AGENTS.md — Visage

## Project Overview

Visage is a Linux face authentication PAM module written in Rust. Single unified binary (`visage`) handles CLI, daemon, one-shot auth, and benchmarks. The PAM module (`pam_visage.so`) is a thin client that either connects to the daemon or spawns `visage auth`.

## Repository Structure

Cargo workspace with 9 crates:

| Crate | Type | Purpose |
|-------|------|---------|
| `visage-core` | lib | Config, types, errors, IPC protocol, traits |
| `visage-camera` | lib | V4L2 capture, auto-detection, preprocessing |
| `visage-face` | lib | ONNX inference (SCRFD + ArcFace) |
| `visage-store` | lib | SQLite face embedding storage |
| `visage-daemon` | lib | Auth/enroll logic, rate limiting, handler |
| `visage-cli` | bin | Unified CLI (`visage` binary) |
| `pam-visage` | cdylib | PAM module (libc + toml + serde only) |
| `visage-tpm` | lib | Optional TPM encryption |
| `visage-test-support` | lib | Mocks and fixtures for testing |

## Build & Verify

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo run --bin visage -- --help
```

## Core Rules

- Do not change binary names, paths, config keys, database schema, or auth semantics without updating `docs/contracts.md`.
- Keep the PAM module free of heavy dependencies (no ort, no v4l, no visage-core).
- Keep all inference local. No cloud services, no runtime model downloads in the auth path.
- Prefer minimal dependencies and clear crate boundaries.

## Security Rules

- **Read `docs/security.md`** before implementing any auth-related code.
- `security.require_ir` defaults to **true**. Never weaken this default.
- Frame variance checks must be in the auth path.
- Model files SHA256-verified at load time.
- IPC messages have size limits (10MB max). Never allocate unbounded buffers.
- Socket access verified via `SO_PEERCRED`.
- PAM module logs all auth attempts to syslog.
- Database and model files have restrictive permissions (640/644, root:visage).
- Rate limiting enforced in daemon (5 attempts/user/60s default).

## Code Style

- `thiserror` for library error types, `anyhow` in binaries.
- Return `Result<T>` over panicking. Never `unwrap()` in library code.
- `tracing` for structured logging. Control verbosity via `RUST_LOG` env filter.
- `#[cfg(test)]` modules in each source file for tests.

## Dependency Rules

| Crate | Dependencies |
|-------|-------------|
| visage-core | serde, toml, bincode, thiserror, tracing |
| visage-camera | visage-core, v4l, image |
| visage-face | visage-core, ort, ndarray, image |
| visage-store | visage-core, rusqlite (bundled), bytemuck |
| visage-daemon | visage-core, visage-camera, visage-face, visage-store, signal-hook |
| visage-cli | visage-core + all above, clap, reqwest, notify-rust |
| pam-visage | **libc, toml, serde ONLY** |
| visage-tpm | visage-core, tracing |

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

**Never** install `pam_visage.so` or edit `/etc/pam.d/*` on the host until container tests pass.

## Workspace Conventions

- Version declared once in root `Cargo.toml`, inherited via `version.workspace = true`.
- Inter-crate deps use relative paths (`path = "../visage-core"`).
- Release profile: LTO + single codegen unit + strip.
