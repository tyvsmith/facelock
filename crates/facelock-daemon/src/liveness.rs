//! Passive liveness detection via landmark stability analysis.
//!
//! Real faces exhibit micro-movements (saccades, micro-expressions, breathing)
//! that cause subtle landmark position shifts between frames. Photos and screens
//! show perfectly static landmarks.

use facelock_core::types::Point2D;

/// Minimum Euclidean displacement (pixels) between frames to count as "movement".
const MIN_DISPLACEMENT_PX: f32 = 0.5;

/// Number of landmarks (out of 5) that must show movement.
const MIN_MOVING_LANDMARKS: usize = 3;

/// Tracks landmark positions across frames for liveness analysis.
pub struct LandmarkTracker {
    history: Vec<[Point2D; 5]>,
    max_frames: usize,
}

impl LandmarkTracker {
    pub fn new(max_frames: usize) -> Self {
        Self {
            history: Vec::with_capacity(max_frames),
            max_frames,
        }
    }

    pub fn push(&mut self, landmarks: [Point2D; 5]) {
        if self.history.len() >= self.max_frames {
            self.history.remove(0);
        }
        self.history.push(landmarks);
    }

    /// Returns true if enough landmark movement detected (live face).
    /// Returns false if landmarks are too static (photo/screen).
    pub fn check_liveness(&self) -> bool {
        if self.history.len() < 2 {
            return false;
        }
        let first = &self.history[0];
        let last = &self.history[self.history.len() - 1];
        let moving_count = first
            .iter()
            .zip(last.iter())
            .filter(|(a, b)| {
                let dx = a.x - b.x;
                let dy = a.y - b.y;
                (dx * dx + dy * dy).sqrt() >= MIN_DISPLACEMENT_PX
            })
            .count();
        moving_count >= MIN_MOVING_LANDMARKS
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
            Point2D {
                x: 100.0,
                y: 100.0,
            },
            Point2D {
                x: 200.0,
                y: 100.0,
            },
            Point2D {
                x: 150.0,
                y: 150.0,
            },
            Point2D {
                x: 120.0,
                y: 200.0,
            },
            Point2D {
                x: 180.0,
                y: 200.0,
            },
        ]
    }

    fn moved_landmarks() -> [Point2D; 5] {
        [
            Point2D {
                x: 101.0,
                y: 100.5,
            },
            Point2D {
                x: 201.0,
                y: 100.3,
            },
            Point2D {
                x: 150.8,
                y: 150.6,
            },
            Point2D {
                x: 120.2,
                y: 200.1,
            },
            Point2D {
                x: 180.1,
                y: 200.1,
            },
        ]
    }

    #[test]
    fn rejects_static_landmarks() {
        let mut tracker = LandmarkTracker::new(10);
        tracker.push(static_landmarks());
        tracker.push(static_landmarks());
        assert!(!tracker.check_liveness());
    }

    #[test]
    fn accepts_moving_landmarks() {
        let mut tracker = LandmarkTracker::new(10);
        tracker.push(static_landmarks());
        tracker.push(moved_landmarks());
        assert!(tracker.check_liveness());
    }

    #[test]
    fn insufficient_frames() {
        let mut tracker = LandmarkTracker::new(10);
        tracker.push(static_landmarks());
        assert!(!tracker.check_liveness());
    }

    #[test]
    fn reset_clears_history() {
        let mut tracker = LandmarkTracker::new(10);
        tracker.push(static_landmarks());
        tracker.push(moved_landmarks());
        tracker.reset();
        assert_eq!(tracker.frame_count(), 0);
        assert!(!tracker.check_liveness());
    }

    #[test]
    fn max_frames_respected() {
        let mut tracker = LandmarkTracker::new(3);
        for _ in 0..5 {
            tracker.push(static_landmarks());
        }
        assert_eq!(tracker.frame_count(), 3);
    }
}
