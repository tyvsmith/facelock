# Contributing

## Prerequisites

- Rust 1.85+ (`rustup update`)
- Linux with V4L2 support
- A webcam (IR recommended; RGB works for development)
- Podman (for container tests)

## Building

```bash
cargo build --workspace
```

## Workspace structure

Facelock is a Cargo workspace with 10 crates:

| Crate | Type | Purpose |
|-------|------|---------|
| `facelock-core` | lib | Config, types, errors, IPC protocol, traits |
| `facelock-camera` | lib | V4L2 capture, auto-detection, preprocessing |
| `facelock-face` | lib | ONNX inference (SCRFD + ArcFace) |
| `facelock-store` | lib | SQLite face embedding storage |
| `facelock-daemon` | lib | Auth/enroll logic, rate limiting, liveness, audit |
| `facelock-cli` | bin | Unified CLI (`facelock` binary, includes `bench` subcommand) |
| `pam-facelock` | cdylib | PAM module (libc + toml + serde, zbus only) |
| `facelock-tpm` | lib | Optional TPM encryption |
| `facelock-polkit` | bin | Polkit face authentication agent |
| `facelock-test-support` | lib | Mocks and fixtures for testing |

Version is declared once in the root `Cargo.toml` and inherited via `version.workspace = true`. Inter-crate dependencies use relative paths.

## Code style

- **Error handling**: `thiserror` for library error types, `anyhow` in binaries. Return `Result<T>` over panicking. Never `unwrap()` in library code.
- **Logging**: `tracing` for structured logging. Control verbosity via `RUST_LOG` env filter.
- **Tests**: `#[cfg(test)]` modules in each source file.
- **Formatting**: `cargo fmt` (default rustfmt settings).
- **Linting**: `cargo clippy --workspace -- -D warnings` must pass with zero warnings.

## Dependency rules

The PAM module (`pam-facelock`) must stay lightweight: **libc, toml, serde, zbus only**. No ort, no v4l, no facelock-core. This keeps the shared library small and avoids dragging heavy dependencies into every PAM-using process.

Each crate has a defined dependency boundary. See `AGENTS.md` for the full table.

## Testing

### Tier 1: Unit tests (no hardware)

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Run these before every commit. They require no camera or models.

### Tier 2: Hardware tests (camera + models)

```bash
cargo test --workspace -- --ignored
```

Requires a connected camera and downloaded models. These tests are marked `#[ignore]` and skipped by default.

### Tier 3: Container tests (requires podman)

```bash
just test-pam          # PAM smoke tests (no camera)
just test-integration  # end-to-end with camera (daemon mode)
just test-oneshot      # end-to-end with camera (no daemon)
just test-shell        # interactive container shell for debugging
```

Container tests validate PAM integration without risking host lockout.

### Tier 4: VM testing

Use a disposable VM with snapshots for testing PAM changes against real login flows.

### Tier 5: Host PAM testing

Only after tiers 3--4 pass. Always keep a root shell open. Start with `sudo` only -- never add Facelock to `login` or display manager PAM until `sudo` works reliably.

### All checks at once

```bash
just check  # runs test + clippy + fmt
```

## Security considerations

Read `docs/security.md` before implementing any auth-related code. Key rules:

- `security.require_ir` defaults to **true**. Never weaken this default.
- Frame variance checks must remain in the auth path.
- Model files are SHA256-verified at load time.
- IPC messages have size limits (10MB max). Never allocate unbounded buffers.
- D-Bus system bus policy restricts daemon access.
- The PAM module logs all auth attempts to syslog.
- Rate limiting is enforced in the daemon (5 attempts/user/60s default).

## Contracts

Do not change binary names, paths, config keys, database schema, or auth semantics without updating `docs/contracts.md`.

## Submitting changes

1. Run `just check` (or at minimum `cargo test --workspace && cargo clippy --workspace -- -D warnings`).
2. Run container tests if your change touches PAM, daemon, or IPC code.
3. Keep commits focused. Separate refactoring from behavioral changes.
4. Write clear commit messages that explain *why*, not just *what*.
