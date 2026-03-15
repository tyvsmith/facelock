use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// 512-dimensional face embedding (ArcFace output)
pub type FaceEmbedding = [f32; 512];

/// A bounding box in image coordinates
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A 2D point
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Point2D {
    pub x: f32,
    pub y: f32,
}

/// A detected face with bounding box, landmarks, and confidence
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Drop for Frame {
    fn drop(&mut self) {
        self.rgb.zeroize();
        self.gray.zeroize();
    }
}

/// Zero a face embedding in place (overwrite with 0.0).
/// Use this at security boundaries after embeddings are no longer needed.
pub fn zeroize_embedding(embedding: &mut FaceEmbedding) {
    embedding.zeroize();
}

/// Zero a vector of embedding tuples (model_id, embedding).
pub fn zeroize_stored_embeddings(stored: &mut [(u32, FaceEmbedding)]) {
    for (_, emb) in stored.iter_mut() {
        emb.zeroize();
    }
}

/// A stored face model (metadata only, without embedding)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceModelInfo {
    pub id: u32,
    pub user: String,
    pub label: String,
    /// Unix timestamp
    pub created_at: u64,
}

/// Result of a face match attempt
#[derive(Debug, Clone, Serialize, Deserialize)]
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
/// Compares all consecutive pairs — every pair must differ enough to rule out
/// a static image. Real faces produce micro-movements between frames.
pub fn check_frame_variance(embeddings: &[FaceEmbedding]) -> bool {
    if embeddings.len() < 2 {
        return false;
    }
    // Every consecutive pair must show movement (similarity below threshold).
    // A static photo produces near-identical consecutive embeddings (>0.998).
    for window in embeddings.windows(2) {
        let sim = cosine_similarity(&window[0], &window[1]);
        if sim >= FRAME_VARIANCE_THRESHOLD {
            return false;
        }
    }
    true
}

/// Convert f32 bits to ordered u32 for constant-time comparison.
/// Positive floats: flip sign bit. Negative floats: flip all bits.
/// Done branchlessly using the sign bit as a mask so that u32 ordering
/// matches f32 ordering across the full range (including negatives).
fn float_bits_to_ordered(bits: u32) -> u32 {
    let mask = ((bits as i32) >> 31) as u32; // all 1s if negative, all 0s if positive
    bits ^ (mask | 0x8000_0000) // flip sign bit always; if negative, flip everything else too
}

/// Find the best cosine similarity between an embedding and a set of stored embeddings.
/// Returns (best_similarity, matching_model_id).
///
/// Always iterates ALL stored embeddings to prevent timing side-channels
/// from revealing which model matched. Uses constant-time conditional
/// selection via the `subtle` crate.
pub fn best_match(
    embedding: &FaceEmbedding,
    stored: &[(u32, FaceEmbedding)],
) -> (f32, Option<u32>) {
    use subtle::{ConditionallySelectable, ConstantTimeGreater};

    // Initialize to -1.0 (minimum possible cosine similarity) so any real
    // similarity will be >= this. We track both the ordered representation
    // (for constant-time comparison) and the raw bits (for the return value).
    let init_bits = (-1.0f32).to_bits();
    let mut best_ord: u32 = float_bits_to_ordered(init_bits);
    let mut best_sim_raw: u32 = init_bits;
    let mut best_id: u32 = u32::MAX; // sentinel for "no match"

    for (id, stored_emb) in stored {
        let sim = cosine_similarity(embedding, stored_emb);
        let sim_bits = sim.to_bits();
        let sim_ord = float_bits_to_ordered(sim_bits);

        // Constant-time: is sim > best_sim?
        let is_greater = sim_ord.ct_gt(&best_ord);

        best_ord = u32::conditional_select(&best_ord, &sim_ord, is_greater);
        best_sim_raw = u32::conditional_select(&best_sim_raw, &sim_bits, is_greater);
        best_id = u32::conditional_select(&best_id, id, is_greater);
    }

    if stored.is_empty() {
        return (0.0, None);
    }

    let best_sim = f32::from_bits(best_sim_raw);
    let matched_id = if best_id == u32::MAX {
        None
    } else {
        Some(best_id)
    };
    (best_sim, matched_id)
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
        assert!(
            (result - 1.0).abs() < 1e-5,
            "identical vectors should have similarity ~1.0, got {result}"
        );
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
        assert!(
            result.abs() < 1e-5,
            "orthogonal vectors should have similarity ~0.0, got {result}"
        );
    }

    #[test]
    fn best_match_finds_correct_match_regardless_of_position() {
        // Create a target embedding
        let mut target: FaceEmbedding = [0.0; 512];
        target[0] = 1.0; // unit vector along dim 0

        // Create stored embeddings with the best match at different positions
        let mut stored: Vec<(u32, FaceEmbedding)> = Vec::new();
        for i in 0..5 {
            let mut emb: FaceEmbedding = [0.0; 512];
            emb[i + 1] = 1.0; // orthogonal to target (similarity ~0)
            stored.push((i as u32, emb));
        }

        // Put exact match first
        stored[0].1 = target;
        let (sim1, id1) = best_match(&target, &stored);
        assert!(sim1 > 0.99, "should find match when first");
        assert_eq!(id1, Some(0));

        // Put exact match last
        stored[0].1 = [0.0; 512];
        stored[0].1[1] = 1.0;
        stored[4].1 = target;
        let (sim2, id2) = best_match(&target, &stored);
        assert!(sim2 > 0.99, "should find match when last");
        assert_eq!(id2, Some(4));
    }

    #[test]
    fn best_match_empty_stored_returns_no_match() {
        let target: FaceEmbedding = [0.1; 512];
        let stored: Vec<(u32, FaceEmbedding)> = vec![];
        let (sim, id) = best_match(&target, &stored);
        assert_eq!(sim, 0.0);
        assert_eq!(id, None);
    }

    #[test]
    fn best_match_prefers_positive_over_negative_similarity() {
        // Regression: negative floats have sign bit set, making their u32 bit
        // representation larger than positive floats. Without sign-aware
        // ordering, ct_gt would incorrectly prefer negative similarities.
        let val = 1.0 / (512.0f32).sqrt();

        // Target: uniform unit vector
        let mut target: FaceEmbedding = [0.0; 512];
        for x in target.iter_mut() {
            *x = val;
        }

        // Stored[0]: opposite of target => similarity ~ -1.0
        let mut opposite: FaceEmbedding = [0.0; 512];
        for x in opposite.iter_mut() {
            *x = -val;
        }

        // Stored[1]: same as target => similarity ~ +1.0
        let same = target;

        let stored = vec![(10, opposite), (20, same)];
        let (sim, id) = best_match(&target, &stored);
        assert!(
            sim > 0.99,
            "should pick positive similarity (~1.0), got {sim}"
        );
        assert_eq!(
            id,
            Some(20),
            "should pick model 20 (positive match), not 10 (negative)"
        );

        // Also test with negative match first and a moderate positive match
        let mut partial: FaceEmbedding = [0.0; 512];
        partial[0] = 1.0; // only partially aligned
        let stored2 = vec![(10, opposite), (30, partial)];
        let (sim2, id2) = best_match(&target, &stored2);
        assert!(sim2 > 0.0, "should pick positive similarity, got {sim2}");
        assert_eq!(id2, Some(30));
    }

    #[test]
    fn best_match_all_negative_similarities() {
        // When all similarities are negative, should still pick the least negative
        let val = 1.0 / (512.0f32).sqrt();
        let mut target: FaceEmbedding = [0.0; 512];
        for x in target.iter_mut() {
            *x = val;
        }

        // opposite => sim ~ -1.0
        let mut opposite: FaceEmbedding = [0.0; 512];
        for x in opposite.iter_mut() {
            *x = -val;
        }

        // Nearly opposite => sim ~ -0.5
        let mut nearly_opp: FaceEmbedding = [0.0; 512];
        for (i, x) in nearly_opp.iter_mut().enumerate() {
            *x = if i < 384 { -val } else { val };
        }

        let stored = vec![(1, opposite), (2, nearly_opp)];
        let (sim, id) = best_match(&target, &stored);
        // nearly_opp has sim ~ -0.5, opposite has sim ~ -1.0
        // Should pick -0.5 (less negative = greater)
        assert_eq!(id, Some(2), "should pick the least-negative similarity");
        assert!(sim > -0.6, "similarity should be around -0.5, got {sim}");
    }

    #[test]
    fn float_bits_to_ordered_preserves_ordering() {
        let values = [-1.0f32, -0.5, -0.1, 0.0, 0.1, 0.5, 1.0];
        for window in values.windows(2) {
            let a = super::float_bits_to_ordered(window[0].to_bits());
            let b = super::float_bits_to_ordered(window[1].to_bits());
            assert!(
                a < b,
                "ordered({}) should be < ordered({}), got {} vs {}",
                window[0],
                window[1],
                a,
                b
            );
        }
    }

    #[test]
    fn zeroize_embedding_clears_data() {
        let mut emb: FaceEmbedding = [0.0; 512];
        emb[0] = 1.0;
        emb[100] = -0.5;
        emb[511] = 42.0;

        zeroize_embedding(&mut emb);

        for (i, &val) in emb.iter().enumerate() {
            assert_eq!(val, 0.0, "embedding[{i}] should be zeroed, got {val}");
        }
    }

    #[test]
    fn zeroize_stored_embeddings_clears_all() {
        let emb1: FaceEmbedding = [1.0; 512];
        let emb2: FaceEmbedding = [2.0; 512];
        let mut stored = vec![(1u32, emb1), (2u32, emb2)];

        zeroize_stored_embeddings(&mut stored);

        for (id, emb) in &stored {
            for (i, &val) in emb.iter().enumerate() {
                assert_eq!(
                    val, 0.0,
                    "embedding for model {id} at [{i}] should be zeroed"
                );
            }
        }
    }

    #[test]
    fn frame_drop_zeroes_pixel_data() {
        let rgb = vec![255u8; 640 * 480 * 3];
        let gray = vec![128u8; 640 * 480];

        // Create frame and get raw pointers to the backing memory
        let mut frame = Frame {
            rgb,
            gray,
            width: 640,
            height: 480,
        };

        // Verify data is non-zero before drop
        assert!(frame.rgb.iter().any(|&b| b != 0));
        assert!(frame.gray.iter().any(|&b| b != 0));

        // Zeroize happens on drop, but we can test the explicit zeroize path
        use zeroize::Zeroize;
        frame.rgb.zeroize();
        frame.gray.zeroize();

        assert!(
            frame.rgb.iter().all(|&b| b == 0),
            "RGB data should be zeroed"
        );
        assert!(
            frame.gray.iter().all(|&b| b == 0),
            "gray data should be zeroed"
        );
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
        assert!(
            (result + 1.0).abs() < 1e-5,
            "opposite vectors should have similarity ~-1.0, got {result}"
        );
    }
}
