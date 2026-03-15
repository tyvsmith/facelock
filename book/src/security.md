# Security Model

## Threat Model

Facelock is a **local biometric authentication system**. The threat model assumes:

- **Attacker has physical access** to the machine (the entire point of face auth is physical-presence scenarios like unlocking a laptop)
- **Attacker may have a photo or video** of the enrolled user
- **Attacker does not have root** (if they do, game over regardless)
- **Attacker cannot modify files** in `/etc/facelock/`, `/var/lib/facelock/`, or `/lib/security/`

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

**Rationale**: Phone screens and printed photos do not emit infrared light correctly. An IR camera sees a flat, textureless surface where a real face would have depth and skin texture in IR. This single check eliminates the vast majority of spoofing attacks.

**Limitation**: IR camera detection by format/name is heuristic. Some cameras report YUYV but are actually IR. The `facelock devices` command should display whether each camera is detected as IR.

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
# Database owned by root, readable only by root and facelock group
chown root:facelock /var/lib/facelock/facelock.db
chmod 640 /var/lib/facelock/facelock.db
```

#### B. Embedding Sensitivity Warning

Face embeddings are **biometric data**. Unlike passwords, they cannot be changed. The database contains irreversible biometric templates -- if compromised, the user's face embeddings cannot be "rotated" like a password.

#### C. Encryption at Rest (Implemented)

For high-security deployments, embeddings can be encrypted with AES-256-GCM using either a plaintext key file (`encryption.method = "keyfile"`) or a TPM-sealed key (`encryption.method = "tpm"`). The TPM method seals the AES key at rest; it is unsealed at daemon startup and held in memory. See [Configuration](configuration.md) for the `[encryption]` and `[tpm]` sections.

### 4. D-Bus IPC Security

**Attack**: Unauthorized user connects to the daemon via D-Bus to trigger auth, enroll faces, or extract data.

**Mitigations**:

#### A. D-Bus System Bus Policy (Required)

The D-Bus system bus policy (`/etc/dbus-1/system.d/org.facelock.Daemon.conf`) restricts which users and groups can own the bus name and invoke methods. Only root and members of the `facelock` group are granted access.

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
require_landmark_liveness = true # Require landmark movement between frames
min_auth_frames = 3          # Minimum frames before accepting (variance check)

[notification]
enabled = true               # Show "Identifying face..." on login screen

[security.pam_policy]
allowed_services = ["sudo", "polkit-1"]
denied_services = ["login", "sshd"]

[security.rate_limit]
max_attempts = 5             # Max auth attempts per user
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
