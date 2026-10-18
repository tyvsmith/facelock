use crate::error::Result;
use crate::types::{CameraCaps, Detection, FaceEmbedding, Frame};

/// Abstraction over camera frame capture.
///
/// Object-safe: capabilities are computed once at construction and asked of
/// the camera, never threaded alongside it as loose parameters (gap D8).
/// Frame darkness is a property of a frame, not a camera — see the free
/// function `facelock_camera::is_dark_with_config`.
pub trait CameraSource {
    /// Capabilities of the underlying device, computed at construction.
    fn capabilities(&self) -> &CameraCaps;

    /// Capture a frame with full preprocessing (RGB + grayscale + CLAHE).
    fn capture(&mut self) -> Result<Frame>;

    /// Capture a frame with RGB only (no grayscale/CLAHE).
    fn capture_rgb_only(&mut self) -> Result<Frame>;

    /// Frames the camera will dequeue and throw away on its own before its
    /// next capture returns — an unverified Y16 stream's calibration burst.
    /// Those frames settle exposure exactly as a warmup discard would, so
    /// [`warmup_discard_count`] counts them against the caller's warmup.
    fn pending_settle_frames(&self) -> u32 {
        0
    }
}

/// How many warmup frames a caller still has to discard after a cold open.
///
/// The one place both cold-open callers (the daemon's camera lease and the
/// one-shot CLI path) size their warmup discard, so a burst the camera runs
/// itself is the warmup rather than a second settling period in front of it
/// (#163).
pub fn warmup_discard_count<C: CameraSource + ?Sized>(camera: &C, warmup: u32) -> u32 {
    warmup.saturating_sub(camera.pending_settle_frames())
}

/// Abstraction over face detection + embedding extraction.
pub trait FaceProcessor {
    /// Detect faces and extract embeddings from a frame.
    fn process(&mut self, frame: &Frame) -> Result<Vec<(Detection, FaceEmbedding)>>;
}
