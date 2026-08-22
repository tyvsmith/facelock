---
paths:
  - "crates/facelock-daemon/**/*.rs"
  - "crates/pam-facelock/**/*.rs"
  - "crates/facelock-tpm/**/*.rs"
  - "crates/facelock-cli/src/commands/auth.rs"
derives-from:
  - dbus/org.facelock.Daemon.conf
  - dist/facelock.tmpfiles
  - systemd/facelock-daemon.service
  - docs/adr/**
reviewed: 2026-08-20
---

# Security Rules

Read `docs/security.md` before implementing any auth-related code. The ADR
directory under `docs/adr/` records why these are what they are; ADR 010 is the
most recent to change them.

- **Read `docs/security.md`** before implementing any auth-related code.
- `security.require_ir` defaults to **true**. Never weaken this default.
- Frame variance checks must be in the auth path.
- Model files SHA256-verified at load time.
- D-Bus system bus policy: deny-all default; every local user may send `Authenticate` only (daemon checks caller UID == target user), root gets the whole interface incl. signals; no group policy (ADR 010).
- D-Bus daemon verifies caller UID via `GetConnectionUnixUser` before executing methods.
- D-Bus message size limits enforced by the bus daemon.
- PAM module logs all auth attempts to syslog.
- Database is 0600 root:root, model files 0644 under a 0755 root:root directory; the state directory is 0711 root:root (ADR 010).
- Rate limiting enforced in daemon (5 attempts/user/60s default).
- Constant-time embedding comparison via `subtle` crate (prevents timing side-channels).
- systemd service hardened with ProtectSystem=strict, NoNewPrivileges, InaccessiblePaths, etc.
