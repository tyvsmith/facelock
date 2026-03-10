# Security Model

## Threat Model

visage is a **local biometric authentication system**. The threat model assumes:

- **Attacker has physical access** to the machine (the entire point of face auth is physical-presence scenarios like unlocking a laptop)
- **Attacker may have a photo or video** of the enrolled user
- **Attacker does not have root** (if they do, game over regardless)
- **Attacker cannot modify files** in `/etc/visage/`, `/var/lib/visage/`, or `/lib/security/`

## Attack Vectors & Mitigations

### 1. Photo/Video Spoofing (CRITICAL)

**Attack**: Hold a photo or video of the enrolled user in front of the camera.

**Why this matters**: This is the #1 attack against face authentication. Without mitigation, anyone with a Facebook photo can unlock the machine.

**Mitigations** (layered, implement all):

#### A. IR Camera Enforcement (Required)

Add `security.require_ir` config flag, **default true**:

```toml
[security]
require_ir = true  # Refuse to authenticate on RGB-only cameras
```

Implementation:
```rust
// In camera capture, check if the negotiated format indicates IR
fn is_ir_camera(device: &DeviceInfo) -> bool {
    // IR cameras typically support GREY (8-bit grayscale) or Y16 (16-bit)
    // as their native format. RGB-only cameras are not IR.
    device.formats.iter().any(|f| {
        matches!(f.fourcc.as_str(), "GREY" | "Y16 " | "YUYV")
            && device.name.to_lowercase().contains("ir")
            || device.name.to_lowercase().contains("infrared")
    })
}

// In daemon auth flow, before attempting recognition:
if config.security.require_ir && !is_ir_camera(&device_info) {
    return DaemonResponse::Error {
        message: "IR camera required for authentication. Set security.require_ir = false to override (NOT RECOMMENDED).".into()
    };
}
```

**Rationale**: Phone screens and printed photos do not emit infrared light correctly. An IR camera sees a flat, textureless surface where a real face would have depth and skin texture in IR. This single check eliminates the vast majority of spoofing attacks.

**Limitation**: IR camera detection by format/name is heuristic. Some cameras report YUYV but are actually IR. The `visage devices` command should display whether each camera is detected as IR.

#### B. Frame Variance Check (Required)

Require minimum variance across consecutive frames during authentication:

```rust
/// Check that frames have sufficient variance (not a static image)
fn check_frame_variance(embeddings: &[(Detection, FaceEmbedding)], min_frames: usize) -> bool {
    if embeddings.len() < min_frames {
        return false;
    }
    // Compute pairwise similarity between consecutive embeddings
    // Real faces have micro-movements causing slight embedding variation
    // A static photo produces near-identical embeddings (similarity > 0.99)
    let mut max_similarity = 0.0f32;
    for window in embeddings.windows(2) {
        let sim = cosine_similarity(&window[0].1, &window[1].1);
        max_similarity = max_similarity.max(sim);
    }
    // If ALL consecutive frames are too similar, likely a static image
    // Real faces typically vary by 0.02-0.10 between frames
    max_similarity < 0.995
}
```

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

```rust
fn check_ir_texture(gray: &[u8], bbox: &BoundingBox, width: u32) -> bool {
    // Extract face region pixels
    let face_pixels = extract_region(gray, bbox, width);
    // Compute standard deviation
    let mean: f32 = face_pixels.iter().map(|&p| p as f32).sum::<f32>() / face_pixels.len() as f32;
    let variance: f32 = face_pixels.iter().map(|&p| (p as f32 - mean).powi(2)).sum::<f32>() / face_pixels.len() as f32;
    let std_dev = variance.sqrt();
    // Real IR faces have std_dev > ~15; flat surfaces are < 5
    std_dev > 10.0
}
```

### 2. Model Tampering

**Attack**: Replace ONNX model files with adversarial models that always match (or match specific attackers).

**Mitigations**:

#### A. SHA256 Verification at Load Time (Required)

Verify model integrity not just at download, but every time the daemon loads models:

```rust
impl FaceEngine {
    pub fn load(config: &RecognitionConfig, model_dir: &Path) -> Result<Self> {
        let manifest = load_manifest();

        for model in &manifest.default_models() {
            let path = model_dir.join(&model.filename);
            if !verify_model(&path, &model.sha256)? {
                return Err(VisageError::Detection(format!(
                    "Model integrity check failed for {}. Expected SHA256: {}. \
                     Re-run `visage setup` to re-download.",
                    model.filename, model.sha256
                )));
            }
        }
        // ... load models
    }
}
```

#### B. File Permissions on Model Directory (Required)

```bash
# Models owned by root, not writable by others
chown -R root:root /var/lib/visage/models
chmod 755 /var/lib/visage/models
chmod 644 /var/lib/visage/models/*.onnx
```

### 3. Embedding / Database Security

**Attack**: Read or modify the SQLite database to extract biometric data or inject fake embeddings.

**Mitigations**:

#### A. Database File Permissions (Required)

```bash
# Database owned by root, readable only by root and visage group
chown root:visage /var/lib/visage/visage.db
chmod 640 /var/lib/visage/visage.db
```

#### B. Embedding Sensitivity Warning (Required)

Face embeddings are **biometric data**. Unlike passwords, they cannot be changed. Document this:
- The database contains irreversible biometric templates
- If compromised, the user's face embeddings cannot be "rotated" like a password
- Embeddings should be treated as sensitive personal data

#### C. Optional: Encryption at Rest (Future)

For high-security deployments, consider encrypting the database with a key derived from a system secret (e.g., TPM-backed key). Not MVP, but design the storage layer so it could be added later.

### 4. IPC / Socket Security

**Attack**: Unauthorized user connects to daemon socket to trigger auth, enroll faces, or extract data.

**Mitigations**:

#### A. Socket Permissions (Required)

```rust
// Create socket with restricted permissions
let socket_path = &config.daemon.socket_path;
// Remove stale socket
let _ = std::fs::remove_file(socket_path);
// Bind
let listener = UnixListener::bind(socket_path)?;
// Set permissions: owner (root) + group (visage) only
std::fs::set_permissions(socket_path, Permissions::from_mode(0o660))?;
// Set ownership
nix::unistd::chown(socket_path, Some(Uid::from_raw(0)), Some(gid_of("visage")))?;
```

#### B. Peer Credential Verification (Required)

Verify the connecting process's UID via `SO_PEERCRED`:

```rust
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};

fn verify_peer(stream: &UnixStream) -> Result<()> {
    let cred = getsockopt(stream.as_raw_fd(), PeerCredentials)?;
    let peer_uid = cred.uid();

    // Only root (PAM context) and members of visage group can connect
    if peer_uid != 0 && !is_in_visage_group(peer_uid) {
        return Err(VisageError::Daemon("unauthorized connection".into()));
    }
    Ok(())
}
```

#### C. Message Size Limits (Required)

Prevent oversized messages from consuming memory:

```rust
pub fn recv_message<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;

    // Reject messages larger than 10MB (generous for JPEG preview frames)
    const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;
    if len > MAX_MESSAGE_SIZE {
        return Err(VisageError::Ipc(format!("message too large: {} bytes", len)));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}
```

#### D. Rate Limiting (Recommended)

Throttle authentication attempts to prevent brute-force:

```rust
struct RateLimiter {
    attempts: HashMap<String, Vec<Instant>>,
    max_attempts: usize,    // 5
    window: Duration,       // 60 seconds
}

impl RateLimiter {
    fn check(&mut self, user: &str) -> bool {
        let now = Instant::now();
        let attempts = self.attempts.entry(user.to_string()).or_default();
        attempts.retain(|t| now.duration_since(*t) < self.window);
        if attempts.len() >= self.max_attempts {
            return false;  // Rate limited
        }
        attempts.push(now);
        true
    }
}
```

### 5. PAM Module Hardening

#### A. Audit Logging (Required)

Log all authentication attempts with outcomes:

```rust
fn identify(pamh: *mut libc::c_void) -> libc::c_int {
    let user = pam_get_user(pamh);
    let service = pam_get_service(pamh);  // "sudo", "login", etc.
    let result = do_auth(user, service);

    // Log to syslog (PAM convention)
    // Format: pam_visage(service): auth result for user
    syslog(LOG_AUTH | severity, "pam_visage({}): {} for user {}",
           service, result_str, user);

    result
}
```

This creates an audit trail in `/var/log/auth.log` or journald.

#### B. Service-Specific Policy (Recommended)

Allow different PAM services to have different security levels:

```toml
[security.pam_policy]
# Only allow face auth for these PAM services
allowed_services = ["sudo", "polkit-1"]
# Never allow face auth for these (always fall through to password)
denied_services = ["login", "sshd", "su"]
```

### 6. Daemon Process Hardening

#### A. Capability Dropping (Recommended)

After initialization, drop unnecessary capabilities:

```rust
// After opening camera, loading models, creating socket:
// Drop all capabilities except what's needed for ongoing operation
use caps::{CapSet, Capability};
caps::clear(None, CapSet::Effective)?;
caps::clear(None, CapSet::Permitted)?;
// Only keep what's needed: nothing (camera fd already open, socket already bound)
```

#### B. systemd Hardening (Required, already partially in place)

The systemd unit should include:
```ini
[Service]
# Already present:
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/visage /run/visage /var/log/visage
DeviceAllow=/dev/video* rw

# Add these:
NoNewPrivileges=yes
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
SystemCallFilter=@system-service @io-event
MemoryDenyWriteExecute=yes
LockPersonality=yes
```

## Security Configuration Reference

```toml
[security]
disabled = false
abort_if_ssh = true          # Refuse face auth over SSH
abort_if_lid_closed = true   # Refuse if laptop lid closed
require_ir = true            # CRITICAL: refuse RGB-only cameras (anti-spoof)
require_frame_variance = true # Reject static images (photo defense)
min_auth_frames = 3          # Minimum frames before accepting (variance check)
detection_notice = true      # Show "Identifying face..." on login screen

[security.pam_policy]
allowed_services = ["sudo", "polkit-1"]
denied_services = ["login", "sshd"]

[security.rate_limit]
max_attempts = 5             # Max auth attempts per user
window_secs = 60             # Rate limit window
```

## Summary: Security Implementation Priority

| Priority | Mitigation | Spec |
|----------|-----------|------|
| **P0** | IR camera enforcement (`require_ir`) | 02-camera, 05-daemon |
| **P0** | Frame variance check (anti-photo) | 05-daemon |
| **P0** | Model SHA256 at load time | 03-face-engine |
| **P0** | Socket permissions + peer creds | 05-daemon |
| **P0** | IPC message size limits | 01-core-types |
| **P0** | PAM audit logging | 06-pam-module |
| **P0** | Database file permissions | 10-build-install |
| **P1** | IR texture validation | 02-camera, 05-daemon |
| **P1** | Rate limiting | 05-daemon |
| **P1** | systemd hardening | 10-build-install |
| **P1** | Capability dropping | 05-daemon |
| **P1** | Service-specific PAM policy | 06-pam-module |
| **P2** | Embedding encryption at rest | 04-face-store |
| **P2** | Memory zeroing on drop | 01-core-types |
| **P2** | Constant-time similarity comparison | 01-core-types |
