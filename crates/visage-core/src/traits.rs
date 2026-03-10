use crate::error::Result;
use crate::types::{Detection, FaceEmbedding, Frame};

/// Abstraction over camera frame capture.
pub trait CameraSource {
    /// Capture a frame with full preprocessing (RGB + grayscale + CLAHE).
    fn capture(&mut self) -> Result<Frame>;

    /// Capture a frame with RGB only (no grayscale/CLAHE).
    fn capture_rgb_only(&mut self) -> Result<Frame>;

    /// Check if a frame is too dark.
    fn is_dark(frame: &Frame) -> bool
    where
        Self: Sized;
}

/// Abstraction over face detection + embedding extraction.
pub trait FaceProcessor {
    /// Detect faces and extract embeddings from a frame.
    fn process(&mut self, frame: &Frame) -> Result<Vec<(Detection, FaceEmbedding)>>;
}
