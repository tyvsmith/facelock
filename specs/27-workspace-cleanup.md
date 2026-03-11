# Spec 27: Workspace Cleanup

## Scope

Use workspace-level version inheritance. Remove redundant version declarations.

## Changes

### Root `Cargo.toml`

Add version to `[workspace.package]`:
```toml
[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "MIT"
```

### All crate `Cargo.toml` files

Replace `version = "0.1.0"` with `version.workspace = true` in every crate:
- facelock-core
- facelock-camera
- facelock-face
- facelock-store
- facelock-daemon
- facelock-cli
- pam-facelock
- facelock-bench
- facelock-test-support
- facelock-tpm

### Inter-crate path references

Keep relative paths (`path = "../facelock-core"`). This is idiomatic Cargo workspace convention. Absolute paths from repo root are not supported by Cargo.

## Acceptance

- Version declared once in root `Cargo.toml`
- All crates inherit via `version.workspace = true`
- `cargo build --workspace` passes
- `cargo publish --dry-run` would work (paths resolve correctly)
