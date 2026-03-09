# Spec 01: Core Types, Config, and IPC Protocol

**Phase**: 1 (Foundation) | **Crate**: howdy-core | **Depends on**: 00

## Goal

Define all shared types, configuration parsing, error handling, IPC protocol, and filesystem path constants used across the workspace.

## Modules

### `types.rs` -- Domain Types

```rust
/// 512-dimensional face embedding (ArcFace output)
pub type FaceEmbedding = [f32; 512];

/// A bounding box in image coordinates
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A 2D point
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Point2D {
    pub x: f32,
    pub y: f32,
}

/// A detected face with bounding box, landmarks, and confidence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub bbox: BoundingBox,
    pub confidence: f32,
    pub landmarks: [Point2D; 5],  // L eye, R eye, nose, L mouth, R mouth
}

/// A camera frame
#[derive(Debug, Clone)]
pub struct Frame {
    pub rgb: Vec<u8>,      // width * height * 3
    pub gray: Vec<u8>,     // width * height
    pub width: u32,
    pub height: u32,
}

/// A stored face model (metadata only, without embedding)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceModelInfo {
    pub id: u32,
    pub user: String,
    pub label: String,
    pub created_at: u64,  // Unix timestamp
}

/// Result of a face match attempt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    pub matched: bool,
    pub model_id: Option<u32>,
    pub label: Option<String>,
    pub similarity: f32,
}

/// Cosine similarity between two L2-normalized embeddings (= dot product)
pub fn cosine_similarity(a: &FaceEmbedding, b: &FaceEmbedding) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}
```

### `config.rs` -- Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub device: DeviceConfig,
    #[serde(default)]
    pub recognition: RecognitionConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub notification: NotificationConfig,
    #[serde(default)]
    pub snapshots: SnapshotConfig,
    #[serde(default)]
    pub debug: DebugConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub path: String,  // Required, e.g. "/dev/video2"
    #[serde(default = "default_max_height")]
    pub max_height: u32,           // Default: 480
    #[serde(default)]
    pub rotation: u16,             // 0, 90, 180, 270
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecognitionConfig {
    #[serde(default = "default_threshold")]
    pub threshold: f32,            // Default: 0.45
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,         // Default: 5
    #[serde(default = "default_confidence")]
    pub detection_confidence: f32, // Default: 0.5
    #[serde(default = "default_nms")]
    pub nms_threshold: f32,        // Default: 0.4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_socket")]
    pub socket_path: String,       // Default: "/run/howdy/howdy.sock"
    #[serde(default = "default_model_dir")]
    pub model_dir: String,         // Default: "/var/lib/howdy/models"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,           // Default: "/var/lib/howdy/howdy.db"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default)]
    pub disabled: bool,
    #[serde(default = "default_true")]
    pub abort_if_ssh: bool,
    #[serde(default = "default_true")]
    pub abort_if_lid_closed: bool,
    #[serde(default)]
    pub suppress_unknown: bool,
    #[serde(default = "default_true")]
    pub detection_notice: bool,
}
// ... NotificationConfig, SnapshotConfig, DebugConfig with similar patterns
```

**Config loading**: Check `HOWDY_CONFIG` env var first, then `/etc/howdy/config.toml`.

**Validation**: Reject empty `device.path`, `threshold` outside 0.0-1.0, `rotation` not in {0, 90, 180, 270}, `timeout_secs` of 0.

### `error.rs` -- Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum HowdyError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),
    #[error("camera error: {0}")]
    Camera(String),
    #[error("detection error: {0}")]
    Detection(String),
    #[error("alignment error: {0}")]
    Alignment(String),
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("IPC error: {0}")]
    Ipc(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    NotFound(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("validation error: {0}")]
    Validation(String),
}
```

### `ipc.rs` -- IPC Protocol

Length-prefixed bincode over Unix domain socket:

```rust
/// Wire format: [4 bytes: u32 LE message length][N bytes: bincode payload]

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonRequest {
    Authenticate { user: String },
    Enroll { user: String, label: String },
    ListModels { user: String },
    RemoveModel { user: String, model_id: u32 },
    ClearModels { user: String },
    PreviewFrame,
    Ping,
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonResponse {
    AuthResult(MatchResult),
    Enrolled { model_id: u32, embedding_count: u32 },
    Models(Vec<FaceModelInfo>),
    Removed,
    Frame { jpeg_data: Vec<u8> },
    Ok,
    Error { message: String },
}

/// Maximum IPC message size (10MB -- generous for JPEG preview frames)
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

pub fn send_message<W: Write>(writer: &mut W, data: &[u8]) -> Result<()>;

/// Reads a length-prefixed message. Rejects messages > MAX_MESSAGE_SIZE.
pub fn recv_message<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(HowdyError::Ipc(format!("message too large: {} bytes", len)));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}
```

### `paths.rs` -- Path Constants

```rust
pub const DEFAULT_CONFIG_PATH: &str = "/etc/howdy/config.toml";
pub const DEFAULT_SOCKET_PATH: &str = "/run/howdy/howdy.sock";
pub const DEFAULT_MODEL_DIR: &str = "/var/lib/howdy/models";
pub const DEFAULT_DB_PATH: &str = "/var/lib/howdy/howdy.db";
pub const DEFAULT_SNAPSHOT_DIR: &str = "/var/log/howdy/snapshots";

pub fn config_path() -> PathBuf {
    std::env::var("HOWDY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH))
}
```

## Tests

- `cosine_similarity`: identical vectors = 1.0, orthogonal = 0.0, opposite = -1.0
- Config parsing: valid minimal, valid full, missing optional sections, validation errors
- IPC round-trip: serialize Request, deserialize, verify equality
- IPC large payload: PreviewFrame with JPEG data
- `HOWDY_CONFIG` env var respected

## Acceptance Criteria

1. All types compile and derive required traits (Debug, Clone, Serialize, Deserialize)
2. Cosine similarity passes unit tests
3. Config loads from TOML with serde defaults
4. Config validates and rejects invalid input with clear errors
5. IPC send/recv round-trips correctly
6. `HOWDY_CONFIG` env var works

## Verification

```bash
cargo test -p howdy-core
cargo clippy -p howdy-core
```
