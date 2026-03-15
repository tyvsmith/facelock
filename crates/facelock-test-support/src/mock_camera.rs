use facelock_core::error::Result;
use facelock_core::traits::CameraSource;
use facelock_core::types::Frame;

use crate::fixtures;

/// A mock camera that replays pre-built frames.
pub struct MockCamera {
    frames: Vec<Frame>,
    index: usize,
    #[allow(dead_code)]
    dark_threshold: f32,
}

impl MockCamera {
    /// Create a mock camera that returns bright frames.
    pub fn bright(width: u32, height: u32, count: usize) -> Self {
        let frames = (0..count)
            .map(|_| fixtures::bright_frame(width, height))
            .collect();
        Self {
            frames,
            index: 0,
            dark_threshold: 0.4,
        }
    }

    /// Create a mock camera that returns dark frames.
    pub fn dark(width: u32, height: u32, count: usize) -> Self {
        let frames = (0..count)
            .map(|_| fixtures::dark_frame(width, height))
            .collect();
        Self {
            frames,
            index: 0,
            dark_threshold: 0.4,
        }
    }

    /// Create a mock camera with custom frames.
    pub fn with_frames(frames: Vec<Frame>) -> Self {
        Self {
            frames,
            index: 0,
            dark_threshold: 0.4,
        }
    }

    /// How many frames have been captured.
    pub fn captures(&self) -> usize {
        self.index
    }
}

impl CameraSource for MockCamera {
    fn capture(&mut self) -> Result<Frame> {
        if self.index >= self.frames.len() {
            // Wrap around to allow repeated captures
            self.index = 0;
        }
        let frame = self.frames[self.index].clone();
        self.index += 1;
        Ok(frame)
    }

    fn capture_rgb_only(&mut self) -> Result<Frame> {
        let mut frame = self.capture()?;
        frame.gray = Vec::new();
        Ok(frame)
    }

    fn is_dark(frame: &Frame) -> bool {
        if frame.gray.is_empty() {
            return true;
        }
        let dark_count = frame.gray.iter().filter(|&&p| p < 10).count();
        let ratio = dark_count as f32 / frame.gray.len() as f32;
        ratio > 0.4
    }
}
