# Spec 00: Workspace Setup

**Phase**: 1 (Foundation) | **Effort**: Small | **Sequential with**: 01

## Goal

Establish the Cargo workspace with 8 crate scaffolds, dev configuration, and project infrastructure.

## Deliverables

### Workspace Structure

```
facelock/
├── Cargo.toml                    # Workspace definition
├── CLAUDE.md                     # Agent instructions (copy from AGENTS.md)
├── dev/
│   └── config.toml               # Development config (local paths, no root)
├── crates/
│   ├── facelock-core/               # Library: config, types, errors, IPC
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs         # Stub
│   │       ├── error.rs          # Stub
│   │       ├── types.rs          # Stub
│   │       ├── ipc.rs            # Stub
│   │       └── paths.rs          # Stub
│   ├── facelock-camera/             # Library: V4L2 capture
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── capture.rs        # Stub
│   │       ├── preprocess.rs     # Stub
│   │       └── device.rs         # Stub
│   ├── facelock-face/               # Library: ONNX inference
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── detector.rs       # Stub
│   │       ├── embedder.rs       # Stub
│   │       ├── align.rs          # Stub
│   │       └── models.rs         # Stub
│   ├── facelock-store/              # Library: SQLite storage
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── db.rs             # Stub
│   │       └── migrations.rs     # Stub
│   ├── facelock-daemon/             # Binary: persistent daemon
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── handler.rs        # Stub
│   │       ├── auth.rs           # Stub
│   │       └── enroll.rs         # Stub
│   ├── facelock-cli/                # Binary: CLI tool
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       └── commands/
│   │           ├── mod.rs
│   │           ├── setup.rs      # Stub
│   │           ├── enroll.rs     # Stub
│   │           ├── remove.rs     # Stub
│   │           ├── clear.rs      # Stub
│   │           ├── list.rs       # Stub
│   │           ├── test_cmd.rs   # Stub
│   │           ├── preview.rs    # Stub
│   │           ├── config.rs     # Stub
│   │           └── status.rs     # Stub
│   ├── pam-facelock/                # cdylib: PAM module
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   └── facelock-bench/              # Binary: benchmarks
│       ├── Cargo.toml
│       └── src/
│           └── main.rs
├── models/
│   └── manifest.toml             # Model URLs, checksums, metadata
├── config/
│   └── facelock.toml                # Default config template
├── systemd/
│   └── facelock-daemon.service      # systemd service unit
└── .gitignore
```

### Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/facelock-core",
    "crates/facelock-camera",
    "crates/facelock-face",
    "crates/facelock-store",
    "crates/facelock-daemon",
    "crates/facelock-cli",
    "crates/pam-facelock",
    "crates/facelock-bench",
]

[workspace.package]
edition = "2024"
rust-version = "1.85"
license = "MIT"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
toml = "0.8"
thiserror = "2"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
bincode = "2.0.0-rc.3"
```

### Key Points

- Edition 2024, Rust 1.85 minimum
- pam-facelock uses `crate-type = ["cdylib"]`
- Shared workspace dependencies minimize duplication
- Dev config uses local paths (`/tmp/facelock-dev.sock`, `./models`, `/tmp/facelock-dev.db`)
- `.gitignore`: `/target`, `*.onnx`, `*.dat`, `dev/*.db`

### systemd Service Unit

```ini
[Unit]
Description=Facelock Face Authentication Daemon
After=local-fs.target

[Service]
Type=notify
ExecStart=/usr/bin/facelock-daemon
Restart=on-failure
RestartSec=3

# Filesystem isolation
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/facelock /run/facelock /var/log/facelock
PrivateTmp=yes

# Device access
DeviceAllow=/dev/video* rw

# Security hardening
NoNewPrivileges=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
MemoryDenyWriteExecute=yes
LockPersonality=yes
SystemCallFilter=@system-service @io-event

[Install]
WantedBy=multi-user.target
```

## Acceptance Criteria

1. `cargo build --workspace` succeeds
2. `cargo test --workspace` succeeds (empty tests OK)
3. `cargo clippy --workspace` succeeds
4. All 8 crates exist with proper Cargo.toml
5. `dev/config.toml` exists with local paths
6. `cargo run --bin facelock -- --help` shows stubbed commands
7. `.gitignore` covers target, ONNX files, dev DB

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace
cargo run --bin facelock -- --help
```
