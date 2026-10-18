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
    /// Frames the next capture spends on a simulated calibration burst before
    /// it returns one (an unverified Y16 `Camera`'s burst). Cleared by it.
    settle_burst: u32,
    /// Frames dequeued by simulated bursts, which `served` does not count.
    burst_dequeued: usize,
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
            settle_burst: 0,
            burst_dequeued: 0,
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
            settle_burst: 0,
            burst_dequeued: 0,
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
            settle_burst: 0,
            burst_dequeued: 0,
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

    /// Simulate an unverified Y16 camera: report `frames` as pending settle
    /// frames, and spend them on the first capture before it returns.
    pub fn with_settle_burst(mut self, frames: u32) -> Self {
        self.settle_burst = frames;
        self
    }

    /// How many frames have been captured.
    pub fn captures(&self) -> usize {
        self.served
    }

    /// Every frame dequeued from the simulated device: captures served plus
    /// frames a settle burst consumed without returning them.
    pub fn dequeued(&self) -> usize {
        self.served + self.burst_dequeued
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
        self.burst_dequeued += std::mem::take(&mut self.settle_burst) as usize;
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

    fn pending_settle_frames(&self) -> u32 {
        self.settle_burst
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn capture_hook_runs_before_each_capture_with_its_ordinal() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let mut cam =
            MockCamera::bright(8, 8, 2).with_capture_hook(move |n| sink.lock().unwrap().push(n));

        cam.capture().unwrap();
        cam.capture_rgb_only().unwrap();
        cam.capture().unwrap();

        // 1-based, one per capture, the RGB-only path included.
        assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn captures_counts_across_wrap_arounds() {
        let mut cam = MockCamera::bright(8, 8, 2);
        for _ in 0..5 {
            cam.capture().unwrap();
        }
        // Five frames served from a two-frame replay: the count is the
        // number served, not the replay index (which has wrapped to 1).
        assert_eq!(cam.captures(), 5);
    }

    #[test]
    fn a_settle_burst_is_spent_once_by_the_first_capture() {
        let mut cam = MockCamera::bright(8, 8, 1).with_settle_burst(8);
        assert_eq!(cam.pending_settle_frames(), 8);
        cam.capture().unwrap();
        assert_eq!(cam.pending_settle_frames(), 0);
        cam.capture().unwrap();
        assert_eq!(cam.captures(), 2);
        assert_eq!(cam.dequeued(), 10);
    }

    #[test]
    fn a_camera_without_a_hook_still_counts() {
        let mut cam = MockCamera::bright(8, 8, 1);
        assert_eq!(cam.captures(), 0);
        cam.capture().unwrap();
        assert_eq!(cam.captures(), 1);
    }
}
