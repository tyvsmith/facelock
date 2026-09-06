# Security Model

## Threat Model

Facelock is a **local biometric authentication system**. The threat model assumes:

- **Attacker has physical access** to the machine (the entire point of face auth is physical-presence scenarios like unlocking a laptop)
- **Attacker may have a photo or video** of the enrolled user
- **Attacker does not have root** (if they do, game over regardless)
- **Attacker cannot modify files** in `/etc/facelock/`, `/var/lib/facelock/`,
  or the distribution's PAM module directories

## Privacy Guarantees

Facelock is designed to keep biometric data under the user's exclusive control:

- **Local-only inference**: All face detection and recognition runs on-device via ONNX Runtime. No images, embeddings, or metadata are ever transmitted over the network.
- **No telemetry**: Facelock contains zero analytics, tracking, or phone-home
  code. Authentication makes no network request. The separately invoked
  `sudo facelock setup` flow downloads model files when they are missing.
- **No cloud dependencies**: Authentication works fully offline. No account registration, no API keys, no external services.
- **Data stays on disk**: Face embeddings are stored in a local SQLite database (`/var/lib/facelock/facelock.db`) with restrictive permissions (600, root:root). New enrollments use AES-256-GCM keyfile encryption by default; TPM sealing is optional.
- **Open source**: Facelock's source is dual-licensed under MIT or Apache-2.0. Dependencies and model weights have separate licenses; see the [model notice](https://github.com/tyvsmith/facelock/blob/main/models/NOTICE.md). Privacy claims can be checked against the source.

## Attack Vectors & Mitigations

### 1. Photo/Video Spoofing (CRITICAL)

**Attack**: Hold a photo or video of the enrolled user in front of the camera.

**Why this matters**: This is the #1 attack against face authentication. Without mitigation, anyone with a Facebook photo can unlock the machine.

**Mitigations** (layered, implement all):

#### A. IR Camera Enforcement (Required)

`security.require_ir` config flag, **default true**:

```toml
[security]
require_ir = true  # Refuse to authenticate on RGB-only cameras
```

**Rationale**: Requiring an IR-classified capture path and applying the IR
texture threshold raises the bar against static RGB presentations. It is one
layer, not proof of sensor authenticity or resistance to video replay.

**Limitation**: IR classification uses an exclusively mono advertised format
set or an exact hardware quirk; the device name is never evidence. A mixed
YUYV/mono node is not auto-classified without a matching quirk. Use
`sudo facelock devices` to inspect the result. `Y16` authentication additionally
requires a hardware-verified `y16_bit_depth`; `Y8`, `Y10`, and `Y12` are
classification evidence but are not decoded.

#### B. Frame Variance Check (Required)

Require minimum variance across consecutive frames during authentication. Real faces have micro-movements causing slight embedding variation. A static photo produces near-identical embeddings (similarity > 0.99).

Config:
```toml
[security]
require_frame_variance = true  # Reject static images (photo attack defense)
min_auth_frames = 3            # Minimum frames before accepting match
```

#### C. Dark Frame / IR Texture Validation (Recommended)

In IR mode, verify that the face region has expected IR texture characteristics:
- Real skin has micro-texture visible in IR
- Photos/screens appear as flat, uniform surfaces in IR
- Compute standard deviation of pixel intensity within the face bounding box
- Reject faces with abnormally low texture variance

### 2. Model Tampering

**Attack**: Replace ONNX model files with adversarial models that always match (or match specific attackers).

**Mitigations**:

#### A. SHA256 Verification at Load Time (Required)

Verify model integrity not just at download, but every time the daemon loads models. Tampered files are rejected before any inference runs.

#### B. File Permissions on Model Directory (Required)

```bash
# Models owned by root, not writable by others
chown -R root:root /var/lib/facelock/models
chmod 755 /var/lib/facelock/models
chmod 644 /var/lib/facelock/models/*.onnx
```

### 3. Embedding / Database Security

**Attack**: Read or modify the SQLite database to extract biometric data or inject fake embeddings.

**Mitigations**:

#### A. Database File Permissions (Required)

```bash
# Database owned by root, readable by root only
chown root:root /var/lib/facelock/facelock.db
chmod 600 /var/lib/facelock/facelock.db
```

#### B. Embedding Sensitivity Warning

Face embeddings are **biometric data**. Unlike passwords, they cannot be changed. The database contains irreversible biometric templates -- if compromised, the user's face embeddings cannot be "rotated" like a password.

#### C. Encryption at Rest (Implemented)

Templates are encrypted at rest by default. The key lives in a plaintext key file (`encryption.method = "keyfile"`, the default) or sealed to the TPM (`encryption.method = "tpm"`), and either way the embeddings themselves are AES-256-GCM. The keyfile is generated at mode `0600` on first use. A TPM-sealed key is unsealed once at daemon startup and held in memory.

Plaintext storage (`encryption.method = "none"`) is an explicit opt-out: enrollment refuses to write unencrypted templates unless `security.allow_plaintext = true`. Auth is never affected by any of this. A decrypt failure falls back to the password, never to a lockout.

`tpm.pcr_binding` is **off by default**, and turning it on is a commitment rather than a hardening tweak. With it on, the sealed key is bound to a PCR selection recorded in the sealed blob, and unsealing replays a real `PolicyPCR` session against the machine's current PCRs. A firmware or kernel change to a bound PCR makes the key refuse to unseal. Face auth then falls through to the password, which is the safe failure, but the templates stay locked until you act.

Recovery is one command:

```bash
sudo facelock tpm reseal
```

It re-seals the key under the current PCR state, recovering the key from the existing blob if the PCRs still match and from the plaintext `encryption.key` backup if they do not.

**Keep that `encryption.key` backup.** It is the recommended setup and it is what makes a reseal painless: without it, a PCR change after a firmware update means re-enrolling every face. The honest cost is that while the backup exists, the `tpm` method's protection against anyone who can read the file is the backup's own `0600` root-only permissions, not the TPM. Deleting the backup buys stronger at-rest confidentiality and pays for it in re-enrollment.

See [Configuration](configuration.md) for the `[encryption]` and `[tpm]` sections, and `docs/security.md` for the full finding.

### 4. D-Bus IPC Security

**Attack**: Unauthorized user connects to the daemon via D-Bus to trigger auth, enroll faces, or extract data.

**Mitigations**:

#### A. D-Bus System Bus Policy (Required)

The D-Bus system bus policy (`/usr/share/dbus-1/system.d/org.facelock.Daemon.conf`) governs who may own the bus name and which methods each caller may send. Two grants (ADR 010): root may send anything on the interface and receive its signals; every local user may send exactly one method, `org.facelock.Daemon.Authenticate`, which is what lets screen lockers and the polkit agent unlock with no group and no re-login. There is no `facelock` group; signal receipt is root-only. (`/etc/dbus-1/system.d/` is the admin-override location for local customization.)

Because the bus admits every local user's `Authenticate`, the in-daemon per-method UID check is the boundary for that method. Before executing any method, the daemon calls `GetConnectionUnixUser` to verify the caller's UID. `Authenticate` allows root, or a non-root caller acting on its own username — a user must be able to request authentication for themselves, since screen lockers run their PAM stack as that user. Every other method, including `Enroll`, `Shutdown`, and the preview methods, is restricted to root (UID 0), so no non-root caller can enroll faces, pull camera frames, or shut down the daemon.

#### B. D-Bus Message Size Limits (Required)

The D-Bus bus daemon enforces message size limits, preventing memory exhaustion attacks.

#### C. Rate Limiting (Recommended)

Throttle authentication attempts: 5 per user per 60 seconds by default. Prevents brute-force and rapid-retry attacks.

### 5. PAM Module Hardening

#### A. Audit Logging (Required)

All authentication attempts are logged to syslog with user, service, and outcome:

```
pam_facelock(sudo): match for user alice
pam_facelock(sudo): no_match for user bob
```

This creates an audit trail in `/var/log/auth.log` or journald.

#### B. Service-Specific Policy (Recommended)

Allow different PAM services to have different security levels:

```toml
[security.pam_policy]
allowed_services = ["sudo", "polkit-1"]
denied_services = ["login", "sshd", "su"]
```

### 6. Daemon Process Hardening

#### A. Capability Dropping (Recommended)

After initialization, the daemon drops all unnecessary capabilities.

#### B. systemd Hardening (Required)

The systemd unit includes: `ProtectSystem=strict`, `ProtectHome=yes`, `NoNewPrivileges=yes`, `PrivateTmp=yes`, and other sandboxing directives.

## Security Configuration Reference

```toml
[security]
disabled = false
abort_if_ssh = true          # Refuse face auth over SSH
abort_if_lid_closed = true   # Refuse if laptop lid closed
require_ir = true            # CRITICAL: refuse RGB-only cameras (anti-spoof)
require_frame_variance = true # Reject static images (photo defense)
require_landmark_liveness = false # Require landmark movement between frames (off by default)
min_auth_frames = 3          # Minimum frames before accepting (variance check)

[notification]
mode = "terminal"            # Show "Identifying face..." on login screen

[security.pam_policy]
allowed_services = ["sudo", "polkit-1"]
denied_services = ["login", "sshd"]

[security.rate_limit]
max_attempts = 5             # Max face-detected auth failures per user per window
window_secs = 60             # Rate limit window
```

## Summary: Security Implementation Priority

| Priority | Mitigation |
|----------|-----------|
| **P0** | IR camera enforcement (`require_ir`) |
| **P0** | Frame variance check (anti-photo) |
| **P0** | Model SHA256 at load time |
| **P0** | D-Bus system bus policy |
| **P0** | D-Bus message size limits |
| **P0** | PAM audit logging |
| **P0** | Database file permissions |
| **P1** | IR texture validation |
| **P1** | Rate limiting |
| **P1** | systemd hardening |
| **P1** | Capability dropping |
| **P1** | Service-specific PAM policy |
| **P2** | Embedding encryption at rest |
| **P2** | Memory zeroing on drop |
| **P2** | Constant-time similarity comparison |
