use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};

/// 512-dimensional face embedding (ArcFace output)
pub type FaceEmbedding = [f32; 512];

/// A bounding box in image coordinates
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Encode, Decode)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A 2D point
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Encode, Decode)]
pub struct Point2D {
    pub x: f32,
    pub y: f32,
}

/// A detected face with bounding box, landmarks, and confidence
#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct Detection {
    pub bbox: BoundingBox,
    pub confidence: f32,
    /// 5-point landmarks: left eye, right eye, nose, left mouth, right mouth
    pub landmarks: [Point2D; 5],
}

/// A camera frame
#[derive(Debug, Clone)]
pub struct Frame {
    /// RGB pixel data (width * height * 3)
    pub rgb: Vec<u8>,
    /// Grayscale pixel data (width * height)
    pub gray: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A stored face model (metadata only, without embedding)
#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct FaceModelInfo {
    pub id: u32,
    pub user: String,
    pub label: String,
    /// Unix timestamp
    pub created_at: u64,
}

/// Result of a face match attempt
#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct MatchResult {
    pub matched: bool,
    pub model_id: Option<u32>,
    pub label: Option<String>,
    pub similarity: f32,
}

/// Cosine similarity between two L2-normalized embeddings (= dot product)
pub fn cosine_similarity(a: &FaceEmbedding, b: &FaceEmbedding) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Threshold below which consecutive embeddings are considered "varied enough"
/// to rule out a static photo attack.
pub const FRAME_VARIANCE_THRESHOLD: f32 = 0.998;

/// Check that matched embeddings show sufficient variance (anti-photo-attack).
/// Compares first vs last embedding — real faces produce micro-movements.
pub fn check_frame_variance(embeddings: &[FaceEmbedding]) -> bool {
    if embeddings.len() < 2 {
        return false;
    }
    let sim = cosine_similarity(&embeddings[0], &embeddings[embeddings.len() - 1]);
    sim < FRAME_VARIANCE_THRESHOLD
}

/// Find the best cosine similarity between an embedding and a set of stored embeddings.
/// Returns (best_similarity, matching_model_id).
pub fn best_match(
    embedding: &FaceEmbedding,
    stored: &[(u32, FaceEmbedding)],
) -> (f32, Option<u32>) {
    let mut best_sim: f32 = 0.0;
    let mut best_id: Option<u32> = None;
    for (model_id, stored_emb) in stored {
        let sim = cosine_similarity(embedding, stored_emb);
        if sim > best_sim {
            best_sim = sim;
            best_id = Some(*model_id);
        }
    }
    (best_sim, best_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_identical() {
        let mut a = [0.0f32; 512];
        // Create a unit vector
        let val = 1.0 / (512.0f32).sqrt();
        for x in &mut a {
            *x = val;
        }
        let result = cosine_similarity(&a, &a);
        assert!((result - 1.0).abs() < 1e-5, "identical vectors should have similarity ~1.0, got {result}");
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let mut a = [0.0f32; 512];
        let mut b = [0.0f32; 512];
        // First half nonzero in a, second half nonzero in b
        for i in 0..256 {
            a[i] = 1.0 / (256.0f32).sqrt();
        }
        for i in 256..512 {
            b[i] = 1.0 / (256.0f32).sqrt();
        }
        let result = cosine_similarity(&a, &b);
        assert!(result.abs() < 1e-5, "orthogonal vectors should have similarity ~0.0, got {result}");
    }

    #[test]
    fn cosine_similarity_opposite() {
        let mut a = [0.0f32; 512];
        let val = 1.0 / (512.0f32).sqrt();
        for x in &mut a {
            *x = val;
        }
        let mut b = a;
        for x in &mut b {
            *x = -*x;
        }
        let result = cosine_similarity(&a, &b);
        assert!((result + 1.0).abs() < 1e-5, "opposite vectors should have similarity ~-1.0, got {result}");
    }
}
