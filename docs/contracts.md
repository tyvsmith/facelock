# System Contracts

Stable contracts that agents must not change without orchestrator approval and explicit documentation of the reason.

## Binary Names

| Binary | Package | Purpose |
|--------|---------|---------|
| `visage` | visage-cli | User-facing CLI |
| `visage-daemon` | visage-daemon | Persistent authentication daemon |
| `visage-auth` | visage-daemon | One-shot auth binary (PAM helper for oneshot mode) |
| `pam_visage.so` | pam-visage | PAM authentication module |
| `visage-bench` | visage-bench | Benchmark and calibration tool |

## Operating Modes

| Mode | Config | PAM Path | Latency | Use Case |
|------|--------|----------|---------|----------|
| Daemon | `daemon.mode = "daemon"` (default) | pam_visage.so → socket → visage-daemon | ~50ms warm | Frequent auth, systemd systems |
| Socket activation | systemd `.socket` unit | Same as daemon, systemd manages lifecycle | ~700ms cold, ~50ms warm | Systemd systems, minimal memory |
| Oneshot | `daemon.mode = "oneshot"` | pam_visage.so → fork/exec visage-auth | ~700ms | No daemon, no systemd, minimal setups |

In oneshot mode, the CLI also operates directly (no daemon needed for any command).

### visage-auth Exit Codes

| Code | Meaning | PAM Code |
|------|---------|----------|
| 0 | Face matched | PAM_SUCCESS |
| 1 | No match / timeout / dark frames | PAM_AUTH_ERR |
| 2 | Error (no camera, no models, config error) | PAM_IGNORE |

## Filesystem Paths

| Path | Owner | Purpose |
|------|-------|---------|
| `/etc/visage/config.toml` | root | System configuration |
| `/var/lib/visage/visage.db` | root:visage | SQLite face embedding database |
| `/var/lib/visage/models/` | root | ONNX model files |
| `/var/log/visage/snapshots/` | root:visage | Auth snapshot images |
| `/run/visage/visage.sock` | visage-daemon | Unix domain socket (daemon mode only) |
| `/usr/bin/visage` | package | CLI binary |
| `/usr/bin/visage-daemon` | package | Daemon binary |
| `/usr/bin/visage-auth` | package | One-shot auth binary |
| `/lib/security/pam_visage.so` | package | PAM module |

All paths overridable via config. `VISAGE_CONFIG` env var overrides config file location for development.

## Config Schema

TOML format. All keys are optional — `device.path` is auto-detected if omitted.

Sections: `device`, `recognition`, `daemon`, `storage`, `security`, `notification`, `snapshots`, `debug`, `tpm`.

### Key Config Fields

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `device.path` | `Option<String>` | None (auto-detect) | Camera device path. Auto-detects if omitted, preferring IR cameras. |
| `daemon.mode` | `String` | `"daemon"` | `"daemon"` or `"oneshot"`. Oneshot runs visage-auth per PAM call, no daemon needed. |
| `daemon.socket_path` | `String` | `/run/visage/visage.sock` | IPC socket (daemon mode only). |
| `daemon.idle_timeout_secs` | `u64` | `0` | Auto-shutdown after idle period when socket-activated. 0 = disabled. |
| `security.require_ir` | `bool` | `true` | Refuse auth on RGB-only cameras. |
| `security.require_frame_variance` | `bool` | `true` | Reject static images (photo attacks). |
| `tpm.seal_database` | `bool` | `false` | Encrypt embeddings at rest with TPM. |

### Camera Auto-Detection

When `device.path` is omitted:
1. Enumerate `/dev/video0` through `/dev/video63`
2. Filter to VIDEO_CAPTURE devices
3. Prefer devices with IR indicators: name contains "ir"/"infrared", or supports GREY/Y16 format
4. Fall back to first available device
5. Log which device was selected

## Database Schema

SQLite. Two core tables:

```sql
CREATE TABLE face_models (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user TEXT NOT NULL,
    label TEXT NOT NULL,
    created_at INTEGER NOT NULL,  -- Unix timestamp
    UNIQUE(user, label)
);

CREATE TABLE face_embeddings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_id INTEGER NOT NULL REFERENCES face_models(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL,  -- 512 x f32 = 2048 bytes
    UNIQUE(model_id)
);

CREATE INDEX idx_face_models_user ON face_models(user);
CREATE INDEX idx_face_embeddings_model ON face_embeddings(model_id);
```

## IPC Protocol

Unix domain socket at `daemon.socket_path`. Length-prefixed bincode messages:

```
[4 bytes: u32 LE message length][N bytes: bincode-encoded Request or Response]
```

Only used in daemon mode. Oneshot mode bypasses IPC entirely.

### Request Variants
- `Authenticate { user: String }` -- run face auth for user
- `Enroll { user: String, label: String }` -- capture and store face
- `ListModels { user: String }` -- list stored face models
- `RemoveModel { user: String, model_id: u32 }` -- delete specific model
- `ClearModels { user: String }` -- delete all models for user
- `PreviewFrame` -- capture and return one JPEG frame
- `PreviewDetectFrame { user: String }` -- frame with face detection overlay
- `ListDevices` -- enumerate V4L2 devices
- `ReleaseCamera` -- release camera resource
- `Ping` -- health check
- `Shutdown` -- graceful daemon shutdown

### Response Variants
- `AuthResult { matched: bool, model_id: Option<u32>, label: Option<String>, similarity: f32 }`
- `Enrolled { model_id: u32, embedding_count: u32 }`
- `Models { models: Vec<FaceModelInfo> }`
- `Removed`
- `Frame { jpeg_data: Vec<u8> }`
- `DetectFrame { jpeg_data: Vec<u8>, faces: Vec<PreviewFace> }`
- `Devices { devices: Vec<IpcDeviceInfo> }`
- `Ok`
- `Error { message: String }`

## PAM Semantics

| Outcome | PAM Code | Meaning |
|---------|----------|---------|
| Face matched | `PAM_SUCCESS` | Auth succeeded |
| Face not matched | `PAM_AUTH_ERR` | Valid attempt, no match |
| Daemon unavailable (daemon mode) | `PAM_IGNORE` | Graceful fallback to next module |
| visage-auth error (oneshot mode) | `PAM_IGNORE` | Graceful fallback |
| Config/setup error | `PAM_IGNORE` | Graceful fallback |
| Timeout | `PAM_AUTH_ERR` | Treated as failed match |

The PAM module must NEVER block indefinitely. All operations have timeouts.

## Model Pack

Default models (InsightFace, ONNX format):

| Model | File | Size | Purpose |
|-------|------|------|---------|
| SCRFD 2.5G | `scrfd_2.5g_bnkps.onnx` | ~3MB | Face detection + 5-point landmarks |
| ArcFace R50 | `w600k_r50.onnx` | ~166MB | 512-D face embedding |

## Matching Contract

- Cosine similarity on L2-normalized 512-D embeddings (= dot product)
- Compare live embedding against all active embeddings for target user
- Match threshold from `recognition.threshold` config (default 0.45)
- Return best match above threshold, or no-match if none exceed

## File Permissions Contract

| Path | Owner | Mode | Rationale |
|------|-------|------|-----------|
| `/etc/visage/config.toml` | root:root | 644 | Config readable by all, writable by root |
| `/var/lib/visage/visage.db` | root:visage | 640 | Biometric data, restricted read |
| `/var/lib/visage/models/*.onnx` | root:root | 644 | Models readable by daemon |
| `/run/visage/visage.sock` | root:visage | 660 | IPC restricted to root + visage group |
| `/lib/security/pam_visage.so` | root:root | 755 | PAM module |
| `/var/log/visage/` | root:visage | 750 | Auth snapshots, restricted |

## Anti-Spoofing Contract

- `security.require_ir` defaults to **true** — refuse auth on RGB-only cameras
- `security.require_frame_variance` defaults to **true** — reject static images
- `security.min_auth_frames` defaults to **3** — require multiple frames before accepting
- IR detection heuristic: camera supports GREY/Y16 format AND/OR name contains "ir"/"infrared"
- Frame variance: first and last matched embeddings must have cosine similarity < 0.998
- These defaults must NOT be weakened without explicit security review

## Audit Logging Contract

All PAM authentication attempts must be logged to syslog (LOG_AUTH facility):
- Format: `pam_visage(<service>): <result> for user <username>`
- Results: `success`, `no_match`, `timeout`, `error: <reason>`, `rate_limited`, `disabled`, `ir_required`
- Oneshot results: `success (oneshot)`, `no_match (oneshot)`, `timeout (oneshot)`
- Daemon must also log auth attempts with timestamps and similarity scores via tracing

## Host Safety Contract

- Host/container for unit tests, integration tests, CLI work
- Container for PAM module smoke testing (pamtester)
- Disposable VM with snapshots for full PAM integration testing
- Host PAM edits forbidden until container + VM validation passes
