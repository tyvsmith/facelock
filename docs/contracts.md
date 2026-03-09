# System Contracts

Stable contracts that agents must not change without orchestrator approval and explicit documentation of the reason.

## Binary Names

| Binary | Package | Purpose |
|--------|---------|---------|
| `howdy` | howdy-cli | User-facing CLI |
| `howdy-daemon` | howdy-daemon | Persistent authentication daemon |
| `pam_howdy.so` | pam-howdy | PAM authentication module |

## Filesystem Paths

| Path | Owner | Purpose |
|------|-------|---------|
| `/etc/howdy/config.toml` | root | System configuration |
| `/var/lib/howdy/howdy.db` | root | SQLite face embedding database |
| `/var/lib/howdy/models/` | root | ONNX model files |
| `/var/log/howdy/snapshots/` | root | Auth snapshot images |
| `/run/howdy/howdy.sock` | howdy-daemon | Unix domain socket |
| `/usr/bin/howdy` | package | CLI binary |
| `/usr/bin/howdy-daemon` | package | Daemon binary |
| `/lib/security/pam_howdy.so` | package | PAM module |

All paths overridable via config. `HOWDY_CONFIG` env var overrides config file location for development.

## Config Schema

TOML format. Required key: `device.path`. All other keys have defaults.

Sections: `device`, `recognition`, `daemon`, `storage`, `security`, `notification`, `snapshots`, `debug`.

See README.md for full schema.

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

Future schema changes use a `schema_version` table and idempotent migrations.

## IPC Protocol

Unix domain socket at `daemon.socket_path`. Length-prefixed bincode messages:

```
[4 bytes: u32 LE message length][N bytes: bincode-encoded Request or Response]
```

### Request Variants
- `Authenticate { user: String }` -- run face auth for user
- `Enroll { user: String, label: String }` -- capture and store face
- `ListModels { user: String }` -- list stored face models
- `RemoveModel { user: String, model_id: u32 }` -- delete specific model
- `ClearModels { user: String }` -- delete all models for user
- `PreviewFrame` -- capture and return one JPEG frame
- `Ping` -- health check
- `Shutdown` -- graceful daemon shutdown

### Response Variants
- `AuthResult { matched: bool, model_id: Option<u32>, label: Option<String>, similarity: f32 }`
- `Enrolled { model_id: u32, embedding_count: u32 }`
- `Models { models: Vec<FaceModelInfo> }`
- `Removed`
- `Frame { jpeg_data: Vec<u8> }`
- `Ok`
- `Error { message: String }`

## PAM Semantics

| Outcome | PAM Code | Meaning |
|---------|----------|---------|
| Face matched | `PAM_SUCCESS` | Auth succeeded |
| Face not matched | `PAM_AUTH_ERR` | Valid attempt, no match |
| Daemon unavailable | `PAM_IGNORE` | Graceful fallback to next module |
| Config/setup error | `PAM_IGNORE` | Graceful fallback |
| Timeout | `PAM_AUTH_ERR` | Treated as failed match |

The PAM module must NEVER block indefinitely. All socket operations have timeouts.

## Model Pack

Default models (InsightFace, ONNX format):

| Model | File | Size | Purpose |
|-------|------|------|---------|
| SCRFD 2.5G | `scrfd_2.5g_bnkps.onnx` | ~3MB | Face detection + 5-point landmarks |
| ArcFace R50 | `w600k_r50.onnx` | ~166MB | 512-D face embedding |

Optional higher-accuracy models:
| Model | File | Size | Purpose |
|-------|------|------|---------|
| SCRFD 10G | `scrfd_10g_bnkps.onnx` | ~16MB | Higher accuracy detection |
| ArcFace R100 | `w600k_r100.onnx` | ~249MB | 99.77% LFW accuracy |

## Matching Contract

- Cosine similarity on L2-normalized 512-D embeddings (= dot product)
- Compare live embedding against all active embeddings for target user
- Match threshold from `recognition.threshold` config (default 0.45)
- Return best match above threshold, or no-match if none exceed

## File Permissions Contract

| Path | Owner | Mode | Rationale |
|------|-------|------|-----------|
| `/etc/howdy/config.toml` | root:root | 644 | Config readable by all, writable by root |
| `/var/lib/howdy/howdy.db` | root:howdy | 640 | Biometric data, restricted read |
| `/var/lib/howdy/models/*.onnx` | root:root | 644 | Models readable by daemon |
| `/var/lib/howdy/models/` | root:root | 755 | Directory listing |
| `/run/howdy/howdy.sock` | root:howdy | 660 | IPC restricted to root + howdy group |
| `/lib/security/pam_howdy.so` | root:root | 755 | PAM module |
| `/var/log/howdy/` | root:howdy | 750 | Auth snapshots, restricted |

## Anti-Spoofing Contract

- `security.require_ir` defaults to **true** — refuse auth on RGB-only cameras
- `security.require_frame_variance` defaults to **true** — reject static images
- `security.min_auth_frames` defaults to **3** — require multiple frames before accepting
- IR detection heuristic: camera supports GREY/Y16 format AND/OR name contains "ir"/"infrared"
- Frame variance: consecutive embeddings must have cosine similarity < 0.995 (micro-movements)
- These defaults must NOT be weakened without explicit security review

## Audit Logging Contract

All PAM authentication attempts must be logged to syslog (LOG_AUTH facility):
- Format: `pam_howdy(<service>): <result> for user <username>`
- Results: `success`, `no_match`, `timeout`, `error: <reason>`, `rate_limited`, `disabled`, `ir_required`
- Daemon must also log auth attempts with timestamps and similarity scores via tracing

## Host Safety Contract

- Host/container for unit tests, integration tests, CLI work
- Container for PAM module smoke testing (pamtester)
- Disposable VM with snapshots for full PAM integration testing
- Host PAM edits forbidden until container + VM validation passes
