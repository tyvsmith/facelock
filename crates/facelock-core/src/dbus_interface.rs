use serde::{Deserialize, Serialize};
use zbus::Type;

/// Result of an authentication attempt.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AuthResult {
    pub matched: bool,
    /// -1 if no match (D-Bus doesn't have Option).
    pub model_id: i32,
    /// Empty string if no match.
    pub label: String,
    /// Cosine similarity score (D-Bus 'd' type). Root-only: redacted to 0.0
    /// for every other caller — see [`AuthResult::redact_similarity_unless_root`].
    pub similarity: f64,
}

impl AuthResult {
    /// The similarity score is a hill-climbing oracle: a caller that learns
    /// how close a probe face got can tune a spoof incrementally, where PAM
    /// only ever reveals pass/fail. Only root may see the score; every other
    /// caller gets `0.0` and the boolean outcome. The unredacted score still
    /// reaches the root-only audit log, which is written upstream of this
    /// redaction.
    #[must_use]
    pub fn redact_similarity_unless_root(mut self, caller_is_root: bool) -> Self {
        if !caller_is_root {
            self.similarity = 0.0;
        }
        self
    }
}

/// Info about an enrolled face model.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ModelInfo {
    pub id: u32,
    pub user: String,
    pub label: String,
    pub created_at: u64,
    pub embedder_model: String,
    /// Canonical device fingerprint (`"vid:pid:serial"`) of the enrolling
    /// camera, or empty string for legacy/unidentified templates. D-Bus has no
    /// Option type, so empty string is the NULL sentinel (matching the
    /// `AuthResult` convention).
    pub device_id: String,
    /// Id of the key that sealed this model (16 hex characters), or empty
    /// string for a pre-V7 or plaintext model — the same NULL sentinel as
    /// `device_id`. Appended last so every earlier field keeps its position.
    pub key_id: String,
}

/// Info about a detected face in preview.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct PreviewFaceInfo {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub confidence: f64,
    /// Root-only, like [`AuthResult::similarity`] — and more urgent here:
    /// preview runs per-frame with no rate limit, so an unredacted score
    /// would be a live tuning feed at camera framerate.
    pub similarity: f64,
    pub recognized: bool,
}

impl PreviewFaceInfo {
    /// See [`AuthResult::redact_similarity_unless_root`].
    #[must_use]
    pub fn redact_similarity_unless_root(mut self, caller_is_root: bool) -> Self {
        if !caller_is_root {
            self.similarity = 0.0;
        }
        self
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_result() -> AuthResult {
        AuthResult {
            matched: true,
            model_id: 4,
            label: "me".into(),
            similarity: 0.93,
        }
    }

    fn preview_face() -> PreviewFaceInfo {
        PreviewFaceInfo {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
            confidence: 0.9,
            similarity: 0.87,
            recognized: true,
        }
    }

    #[test]
    fn auth_result_similarity_zeroed_for_non_root() {
        let redacted = auth_result().redact_similarity_unless_root(false);
        assert_eq!(redacted.similarity, 0.0);
        // Only the score is redacted — the boolean outcome and identity of
        // the matched model survive.
        assert!(redacted.matched);
        assert_eq!(redacted.model_id, 4);
        assert_eq!(redacted.label, "me");
    }

    #[test]
    fn auth_result_similarity_kept_for_root() {
        let kept = auth_result().redact_similarity_unless_root(true);
        assert_eq!(kept.similarity, 0.93);
        assert!(kept.matched);
    }

    #[test]
    fn preview_face_similarity_zeroed_for_non_root() {
        let redacted = preview_face().redact_similarity_unless_root(false);
        assert_eq!(redacted.similarity, 0.0);
        // Detection geometry and the recognized boolean survive.
        assert!(redacted.recognized);
        assert_eq!(redacted.confidence, 0.9);
        assert_eq!(redacted.x, 1.0);
    }

    #[test]
    fn preview_face_similarity_kept_for_root() {
        let kept = preview_face().redact_similarity_unless_root(true);
        assert_eq!(kept.similarity, 0.87);
        assert!(kept.recognized);
    }
}
