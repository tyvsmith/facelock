use facelock_core::error::Result;
use facelock_core::traits::CameraSource;
use facelock_core::types::{CameraCaps, Frame};

use crate::fixtures;

/// The camera-factory box `Handler::new` takes in tests.
///
/// Named because it is spelled out at nine call sites across the daemon's
/// integration tests, where the bare `Box<dyn Fn(..) -> .. + Send + Sync>` is
/// both noise and a `clippy::type_complexity` error under the workspace's
/// `--all-targets` lint gate.
///
/// Those nine call sites are **not** on this branch: `crates/facelock-daemon`
/// belongs to a concurrent change, which carries an identically-named local
/// alias in each of its three test files. This is the definition all three
/// collapse into once the branches merge, which is why it is exported with no
/// in-tree caller yet.
///
/// `std::result::Result` is spelled out because this module imports
/// `facelock_core::error::Result`, a one-parameter alias that cannot express
/// the `String` error type.
pub type MockCameraFactory = Box<
    dyn Fn(&facelock_core::config::Config) -> std::result::Result<MockCamera, String> + Send + Sync,
>;

/// Runs before each capture with that capture's 1-based ordinal. `Send` so a
/// camera carrying one still satisfies the daemon's handler bounds.
type CaptureHook = Box<dyn FnMut(usize) + Send>;

/// A mock camera that replays pre-built frames.
pub struct MockCamera {
    frames: Vec<Frame>,
    index: usize,
    #[allow(dead_code)]
    dark_threshold: f32,
    caps: CameraCaps,
    capture_hook: Option<CaptureHook>,
    /// Total captures served, across wrap-arounds.
    served: usize,
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
            caps: CameraCaps::default(),
            capture_hook: None,
            served: 0,
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
            caps: CameraCaps::default(),
            capture_hook: None,
            served: 0,
        }
    }

    /// Create a mock camera with custom frames.
    pub fn with_frames(frames: Vec<Frame>) -> Self {
        Self {
            frames,
            index: 0,
            dark_threshold: 0.4,
            caps: CameraCaps::default(),
            capture_hook: None,
            served: 0,
        }
    }

    /// Attach capabilities (defaults to `CameraCaps::default()`: non-IR, no
    /// identity) — for IR-gating and device-coupling tests.
    pub fn with_caps(mut self, caps: CameraCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Run `hook` before each capture with that capture's 1-based ordinal, so
    /// a test can act in the middle of a capture loop — cancel the request
    /// on the Nth frame, say — without a second camera type.
    pub fn with_capture_hook(mut self, hook: impl FnMut(usize) + Send + 'static) -> Self {
        self.capture_hook = Some(Box::new(hook));
        self
    }

    /// How many frames have been captured.
    pub fn captures(&self) -> usize {
        self.served
    }

    /// Mock darkness check (fixed thresholds), kept as an inherent method now
    /// that `is_dark` is no longer part of `CameraSource`.
    pub fn is_dark(frame: &Frame) -> bool {
        if frame.gray.is_empty() {
            return true;
        }
        let dark_count = frame.gray.iter().filter(|&&p| p < 10).count();
        let ratio = dark_count as f32 / frame.gray.len() as f32;
        ratio > 0.4
    }
}

impl CameraSource for MockCamera {
    fn capabilities(&self) -> &CameraCaps {
        &self.caps
    }

    fn capture(&mut self) -> Result<Frame> {
        self.served += 1;
        if let Some(hook) = self.capture_hook.as_mut() {
            hook(self.served);
        }
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
}
