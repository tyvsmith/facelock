use serde::{Deserialize, Serialize};
use zvariant::Type;

/// Result of an authentication attempt.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AuthResult {
    pub matched: bool,
    /// -1 if no match (D-Bus doesn't have Option).
    pub model_id: i32,
    /// Empty string if no match.
    pub label: String,
    /// Cosine similarity score (D-Bus 'd' type).
    pub similarity: f64,
}

/// Info about an enrolled face model.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ModelInfo {
    pub id: u32,
    pub user: String,
    pub label: String,
    pub created_at: u64,
    pub embedder_model: String,
}

/// Info about a detected face in preview.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct PreviewFaceInfo {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub confidence: f64,
    pub similarity: f64,
    pub recognized: bool,
}

/// Info about a camera device.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct DeviceInfo {
    pub path: String,
    pub name: String,
    pub driver: String,
    pub is_ir: bool,
}

pub const INTERFACE_NAME: &str = "org.facelock.Daemon";
pub const OBJECT_PATH: &str = "/org/facelock/Daemon";
pub const BUS_NAME: &str = "org.facelock.Daemon";
