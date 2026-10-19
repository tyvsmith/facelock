//! Shared IPC payload types: data that crosses the daemon/CLI boundary in
//! both directions (preview faces, device enumeration). The daemon handler's
//! own request/response enums are internal to `facelock-daemon` (D5).

use serde::Serialize;

/// A detected face in a preview frame with its recognition status.
#[derive(Debug, Clone)]
pub struct PreviewFace {
    /// Bounding box in original (pre-JPEG) frame coordinates.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Detection confidence from SCRFD.
    pub confidence: f32,
    /// Best cosine similarity against stored embeddings (0.0 if no models).
    pub similarity: f32,
    /// Whether similarity exceeded the recognition threshold.
    pub recognized: bool,
}

/// Information about a V4L2 video device, returned via IPC.
///
/// `Serialize` backs `facelock devices --json` (serde-derived, unlike the
/// hand-rolled `format!` JSON `list --json` used to build — see E10).
#[derive(Debug, Clone, Serialize)]
pub struct IpcDeviceInfo {
    pub path: String,
    pub name: String,
    pub driver: String,
    pub is_ir: bool,
    pub formats: Vec<IpcFormatInfo>,
    /// Kernel modalias of the node's parent device, for a non-USB node with a
    /// real parent (`auxiliary:intel_ipu6.isys`, `of:...`); `None` for USB
    /// and virtual nodes. Paired with `node_name` as the quirk keys of the
    /// same names.
    pub modalias: Option<String>,
    /// The node's sysfs `name`, read alongside `modalias`; `None` whenever
    /// `modalias` is.
    pub node_name: Option<String>,
}

/// A supported pixel format with available resolutions.
#[derive(Debug, Clone, Serialize)]
pub struct IpcFormatInfo {
    pub fourcc: String,
    pub description: String,
    pub sizes: Vec<(u32, u32)>,
}
