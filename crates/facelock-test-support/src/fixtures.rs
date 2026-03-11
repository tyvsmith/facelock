use facelock_core::types::{BoundingBox, Detection, FaceEmbedding, Frame, Point2D};

/// Create a synthetic bright frame (all pixels at 128).
pub fn bright_frame(width: u32, height: u32) -> Frame {
    let pixels = (width * height) as usize;
    Frame {
        rgb: vec![128u8; pixels * 3],
        gray: vec![128u8; pixels],
        width,
        height,
    }
}

/// Create a synthetic dark frame (all pixels at 2).
pub fn dark_frame(width: u32, height: u32) -> Frame {
    let pixels = (width * height) as usize;
    Frame {
        rgb: vec![2u8; pixels * 3],
        gray: vec![2u8; pixels],
        width,
        height,
    }
}

/// Create a known embedding (unit vector with value at given index perturbed).
pub fn known_embedding(seed: u8) -> FaceEmbedding {
    let val = 1.0 / (512.0f32).sqrt();
    let mut emb = [val; 512];
    // Perturb a few dimensions based on seed for uniqueness
    emb[0] += seed as f32 * 0.01;
    emb[1] -= seed as f32 * 0.005;
    // Re-normalize
    let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in &mut emb {
        *x /= norm;
    }
    emb
}

/// Create a detection at the center of a 640x480 frame.
pub fn center_detection(confidence: f32) -> Detection {
    Detection {
        bbox: BoundingBox {
            x: 200.0,
            y: 100.0,
            width: 240.0,
            height: 280.0,
        },
        confidence,
        landmarks: [
            Point2D { x: 270.0, y: 200.0 }, // left eye
            Point2D { x: 370.0, y: 200.0 }, // right eye
            Point2D { x: 320.0, y: 250.0 }, // nose
            Point2D { x: 280.0, y: 310.0 }, // left mouth
            Point2D { x: 360.0, y: 310.0 }, // right mouth
        ],
    }
}

/// Create a pair of embeddings that are very similar (> 0.998 cosine similarity).
/// Used to test frame variance rejection (static photo attack).
pub fn identical_embedding_pair() -> (FaceEmbedding, FaceEmbedding) {
    let emb = known_embedding(0);
    (emb, emb)
}

/// Create a pair of embeddings that differ enough to pass variance check (< 0.998).
pub fn varied_embedding_pair() -> (FaceEmbedding, FaceEmbedding) {
    let emb1 = known_embedding(0);
    let emb2 = known_embedding(50);
    (emb1, emb2)
}

/// Create a test config TOML string suitable for daemon integration tests.
pub fn test_config_toml(db_path: &str, socket_path: &str) -> String {
    format!(
        r#"
[device]
path = "/dev/video99"

[recognition]
threshold = 0.45
timeout_secs = 2

[daemon]
socket_path = "{socket_path}"
model_dir = "/tmp/facelock-test-models"

[storage]
db_path = "{db_path}"

[security]
disabled = false
require_ir = false
require_frame_variance = true
min_auth_frames = 2
abort_if_ssh = false
abort_if_lid_closed = false

[security.rate_limit]
max_attempts = 5
window_secs = 60

"#
    )
}
