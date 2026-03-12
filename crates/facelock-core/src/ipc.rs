use crate::types::{FaceModelInfo, MatchResult};

#[derive(Debug, Clone)]
pub enum DaemonRequest {
    Authenticate { user: String },
    Enroll { user: String, label: String },
    ListModels { user: String },
    RemoveModel { user: String, model_id: u32 },
    ClearModels { user: String },
    PreviewFrame,
    /// Preview with face detection + recognition against the given user's models.
    PreviewDetectFrame { user: String },
    ListDevices,
    ReleaseCamera,
    Ping,
    Shutdown,
}

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
#[derive(Debug, Clone)]
pub struct IpcDeviceInfo {
    pub path: String,
    pub name: String,
    pub driver: String,
    pub is_ir: bool,
    pub formats: Vec<IpcFormatInfo>,
}

/// A supported pixel format with available resolutions.
#[derive(Debug, Clone)]
pub struct IpcFormatInfo {
    pub fourcc: String,
    pub description: String,
    pub sizes: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub enum DaemonResponse {
    AuthResult(MatchResult),
    Enrolled { model_id: u32, embedding_count: u32 },
    Models(Vec<FaceModelInfo>),
    Removed,
    Frame { jpeg_data: Vec<u8> },
    /// Preview frame with face detection results.
    DetectFrame {
        jpeg_data: Vec<u8>,
        faces: Vec<PreviewFace>,
    },
    Devices(Vec<IpcDeviceInfo>),
    Ok,
    Error { message: String },
}
