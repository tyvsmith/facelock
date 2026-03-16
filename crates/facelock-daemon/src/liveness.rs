//! Passive liveness detection via landmark stability analysis.
//!
//! Real faces exhibit micro-movements (saccades, micro-expressions, breathing)
//! that cause subtle landmark position shifts between frames. Photos and screens
//! show perfectly static landmarks.

use facelock_core::types::Point2D;

/// Tracks landmark positions across frames for liveness analysis.
pub struct LandmarkTracker {
    history: Vec<[Point2D; 5]>,
    max_frames: usize,
    displacement_threshold: f32,
    min_moving_landmarks: usize,
}

impl LandmarkTracker {
    pub fn new(max_frames: usize, displacement_threshold: f32, min_moving_landmarks: usize) -> Self {
        Self {
            history: Vec::with_capacity(max_frames),
            max_frames,
            displacement_threshold,
            min_moving_landmarks,
        }
    }

    pub fn push(&mut self, landmarks: [Point2D; 5]) {
        if self.history.len() >= self.max_frames {
            self.history.remove(0);
        }
        self.history.push(landmarks);
    }

    /// Returns true if enough landmark movement detected across any consecutive
    /// frame pair (live face). Returns false if all pairs are too static (photo/screen).
    pub fn check_liveness(&self) -> bool {
        if self.history.len() < 2 {
            return false;
        }
        // Check consecutive frame pairs. A real face will show movement in at
        // least one pair. Photos are static across all pairs.
        for window in self.history.windows(2) {
            let moving_count = window[0]
                .iter()
                .zip(window[1].iter())
                .filter(|(a, b)| {
                    let dx = a.x - b.x;
                    let dy = a.y - b.y;
                    (dx * dx + dy * dy).sqrt() >= self.displacement_threshold
                })
                .count();
            if moving_count >= self.min_moving_landmarks {
                return true;
            }
        }
        false
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }

    pub fn frame_count(&self) -> usize {
        self.history.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facelock_core::types::Point2D;

    fn static_landmarks() -> [Point2D; 5] {
        [
            Point2D { x: 100.0, y: 100.0 },
            Point2D { x: 200.0, y: 100.0 },
            Point2D { x: 150.0, y: 150.0 },
            Point2D { x: 120.0, y: 200.0 },
            Point2D { x: 180.0, y: 200.0 },
        ]
    }

    fn moved_landmarks() -> [Point2D; 5] {
        [
            Point2D { x: 102.0, y: 101.5 }, // ~2.5px from static
            Point2D { x: 202.0, y: 101.5 }, // ~2.5px
            Point2D { x: 152.0, y: 151.5 }, // ~2.5px
            Point2D { x: 120.2, y: 200.1 }, // ~0.22px (not moving)
            Point2D { x: 180.1, y: 200.1 }, // ~0.14px (not moving)
        ]
    }

    #[test]
    fn rejects_static_landmarks() {
        let mut tracker = LandmarkTracker::new(10, 1.5, 3);
        tracker.push(static_landmarks());
        tracker.push(static_landmarks());
        assert!(!tracker.check_liveness());
    }

    #[test]
    fn accepts_moving_landmarks() {
        let mut tracker = LandmarkTracker::new(10, 1.5, 3);
        tracker.push(static_landmarks());
        tracker.push(moved_landmarks());
        assert!(tracker.check_liveness());
    }

    #[test]
    fn insufficient_frames() {
        let mut tracker = LandmarkTracker::new(10, 1.5, 3);
        tracker.push(static_landmarks());
        assert!(!tracker.check_liveness());
    }

    #[test]
    fn reset_clears_history() {
        let mut tracker = LandmarkTracker::new(10, 1.5, 3);
        tracker.push(static_landmarks());
        tracker.push(moved_landmarks());
        tracker.reset();
        assert_eq!(tracker.frame_count(), 0);
        assert!(!tracker.check_liveness());
    }

    #[test]
    fn max_frames_respected() {
        let mut tracker = LandmarkTracker::new(3, 1.5, 3);
        for _ in 0..5 {
            tracker.push(static_landmarks());
        }
        assert_eq!(tracker.frame_count(), 3);
    }
}
