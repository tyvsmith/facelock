use facelock_core::types::{BoundingBox, Detection, FaceEmbedding, cosine_similarity};

/// Quality score for a single enrollment frame.
#[derive(Debug, Clone)]
pub struct FrameQuality {
    /// How centered the face is (0.0 = edge, 1.0 = perfectly centered)
    pub centering: f32,
    /// Relative face size (face area / frame area)
    pub face_size: f32,
    /// Average brightness (0.0 = dark, 1.0 = bright)
    pub brightness: f32,
    /// Sharpness score (Laplacian variance, higher = sharper)
    pub sharpness: f32,
    /// Overall quality score (weighted combination)
    pub overall: f32,
}

/// Minimum quality threshold for enrollment frames.
const MIN_QUALITY_SCORE: f32 = 0.35;

/// Maximum cosine similarity between any two enrollment embeddings.
/// Rejects if all embeddings are too similar (insufficient angle diversity).
const MAX_EMBEDDING_SIMILARITY: f32 = 0.95;

/// Score the quality of a detected face in a frame.
pub fn score_frame(
    detection: &Detection,
    gray: &[u8],
    frame_width: u32,
    frame_height: u32,
) -> FrameQuality {
    let centering = score_centering(&detection.bbox, frame_width, frame_height);
    let face_size = score_face_size(&detection.bbox, frame_width, frame_height);
    let brightness = score_brightness(gray, &detection.bbox, frame_width);
    let sharpness = score_sharpness(gray, &detection.bbox, frame_width);

    // Weighted overall score
    let overall = centering * 0.2 + face_size * 0.3 + brightness * 0.2 + sharpness * 0.3;

    FrameQuality {
        centering,
        face_size,
        brightness,
        sharpness,
        overall,
    }
}

/// Check if a frame meets minimum quality for enrollment.
pub fn meets_quality_threshold(quality: &FrameQuality) -> bool {
    quality.overall >= MIN_QUALITY_SCORE
}

/// Check angle diversity among a set of embeddings.
/// Returns true if embeddings show sufficient diversity (not all from same angle).
pub fn check_angle_diversity(embeddings: &[FaceEmbedding]) -> bool {
    if embeddings.len() < 2 {
        return true; // Can't check diversity with < 2 embeddings
    }

    // Check that at least one pair has similarity below the threshold
    for i in 0..embeddings.len() {
        for j in (i + 1)..embeddings.len() {
            let sim = cosine_similarity(&embeddings[i], &embeddings[j]);
            if sim < MAX_EMBEDDING_SIMILARITY {
                return true;
            }
        }
    }

    false // All pairs too similar
}

/// Generate a human-readable quality hint for the user.
pub fn quality_hint(quality: &FrameQuality) -> Option<&'static str> {
    if quality.centering < 0.3 {
        return Some("Center your face in the frame");
    }
    if quality.face_size < 0.2 {
        return Some("Move closer to the camera");
    }
    if quality.face_size > 0.8 {
        return Some("Move further from the camera");
    }
    if quality.brightness < 0.2 {
        return Some("Improve lighting conditions");
    }
    if quality.sharpness < 0.2 {
        return Some("Hold still for a sharper image");
    }
    None
}

/// Score how centered the face bounding box is in the frame.
fn score_centering(bbox: &BoundingBox, frame_width: u32, frame_height: u32) -> f32 {
    let face_cx = bbox.x + bbox.width / 2.0;
    let face_cy = bbox.y + bbox.height / 2.0;
    let frame_cx = frame_width as f32 / 2.0;
    let frame_cy = frame_height as f32 / 2.0;

    let dx = (face_cx - frame_cx).abs() / frame_cx;
    let dy = (face_cy - frame_cy).abs() / frame_cy;

    // 1.0 when perfectly centered, 0.0 at edge
    (1.0 - (dx + dy) / 2.0).clamp(0.0, 1.0)
}

/// Score the relative size of the face in the frame.
fn score_face_size(bbox: &BoundingBox, frame_width: u32, frame_height: u32) -> f32 {
    let face_area = bbox.width * bbox.height;
    let frame_area = frame_width as f32 * frame_height as f32;
    let ratio = face_area / frame_area;

    // Ideal range: 5% to 40% of frame
    if ratio < 0.05 {
        ratio / 0.05
    } else if ratio > 0.40 {
        (1.0 - (ratio - 0.40) / 0.60).max(0.0)
    } else {
        1.0
    }
}

/// Score brightness of the face region.
fn score_brightness(gray: &[u8], bbox: &BoundingBox, frame_width: u32) -> f32 {
    let pixels = extract_face_pixels(gray, bbox, frame_width);
    if pixels.is_empty() {
        return 0.5; // Default if we can't extract pixels
    }

    let sum: u64 = pixels.iter().map(|&p| p as u64).sum();
    let mean = sum as f32 / pixels.len() as f32 / 255.0;

    // Ideal brightness: 0.3-0.7
    if mean < 0.1 {
        mean / 0.1
    } else if mean > 0.9 {
        (1.0 - mean) / 0.1
    } else {
        1.0
    }
}

/// Score sharpness using Laplacian variance approximation.
fn score_sharpness(gray: &[u8], bbox: &BoundingBox, frame_width: u32) -> f32 {
    let x_start = (bbox.x as u32).min(frame_width.saturating_sub(1));
    let y_start = (bbox.y as u32).min(frame_width.saturating_sub(1)); // approximate
    let w = (bbox.width as u32).min(frame_width - x_start);
    let h = bbox.height as u32;

    if w < 3 || h < 3 {
        return 0.0;
    }

    // Simplified Laplacian: sum of absolute second derivatives
    let mut variance_sum: f64 = 0.0;
    let mut count: u64 = 0;

    for y in (y_start + 1)..(y_start + h).saturating_sub(1) {
        let row = y as usize * frame_width as usize;
        if row + (x_start as usize + w as usize) > gray.len() {
            break;
        }
        for x in (x_start + 1)..(x_start + w).saturating_sub(1) {
            let idx = row + x as usize;
            if idx + frame_width as usize >= gray.len() || idx < frame_width as usize {
                continue;
            }
            let center = gray[idx] as f64;
            let laplacian = (gray[idx - 1] as f64
                + gray[idx + 1] as f64
                + gray[idx - frame_width as usize] as f64
                + gray[idx + frame_width as usize] as f64
                - 4.0 * center)
                .abs();
            variance_sum += laplacian;
            count += 1;
        }
    }

    if count == 0 {
        return 0.0;
    }

    let avg_laplacian = variance_sum / count as f64;

    // Normalize: ~5-20 is typical for good sharpness
    (avg_laplacian as f32 / 15.0).clamp(0.0, 1.0)
}

fn extract_face_pixels(gray: &[u8], bbox: &BoundingBox, frame_width: u32) -> Vec<u8> {
    let x_start = (bbox.x as usize).min(gray.len());
    let y_start = (bbox.y as usize).min(gray.len());
    let w = bbox.width as usize;
    let h = bbox.height as usize;

    let mut pixels = Vec::with_capacity(w * h);
    for y in y_start..(y_start + h) {
        let row_start = y * frame_width as usize + x_start;
        let row_end = (row_start + w).min(gray.len());
        if row_start < gray.len() {
            pixels.extend_from_slice(&gray[row_start..row_end]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centering_score_centered() {
        let bbox = BoundingBox {
            x: 270.0,
            y: 190.0,
            width: 100.0,
            height: 100.0,
        };
        let score = score_centering(&bbox, 640, 480);
        assert!(score > 0.9, "centered face should score high: {score}");
    }

    #[test]
    fn centering_score_edge() {
        let bbox = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let score = score_centering(&bbox, 640, 480);
        assert!(score < 0.5, "edge face should score low: {score}");
    }

    #[test]
    fn face_size_score_ideal() {
        let bbox = BoundingBox {
            x: 200.0,
            y: 100.0,
            width: 200.0,
            height: 200.0,
        };
        // 200*200 / (640*480) = ~13% - in ideal range
        let score = score_face_size(&bbox, 640, 480);
        assert!(score > 0.9, "ideal face size should score high: {score}");
    }

    #[test]
    fn face_size_score_too_small() {
        let bbox = BoundingBox {
            x: 300.0,
            y: 200.0,
            width: 30.0,
            height: 30.0,
        };
        let score = score_face_size(&bbox, 640, 480);
        assert!(score < 0.5, "tiny face should score low: {score}");
    }

    #[test]
    fn angle_diversity_identical() {
        let emb = [0.5f32; 512];
        assert!(!check_angle_diversity(&[emb, emb, emb]));
    }

    #[test]
    fn angle_diversity_varied() {
        let mut emb1 = [0.0f32; 512];
        let mut emb2 = [0.0f32; 512];
        emb1[0] = 1.0;
        emb2[1] = 1.0; // Orthogonal = very different
        assert!(check_angle_diversity(&[emb1, emb2]));
    }

    #[test]
    fn score_frame_produces_weighted_overall() {
        let det = Detection {
            bbox: BoundingBox {
                x: 220.0,
                y: 140.0,
                width: 200.0,
                height: 200.0,
            },
            confidence: 0.95,
            landmarks: [facelock_core::types::Point2D { x: 0.0, y: 0.0 }; 5],
        };
        let gray = vec![128u8; 640 * 480]; // uniform mid-brightness
        let quality = score_frame(&det, &gray, 640, 480);

        // Overall should be in [0, 1]
        assert!(quality.overall >= 0.0 && quality.overall <= 1.0);
        // Each component should be in [0, 1]
        assert!(quality.centering >= 0.0 && quality.centering <= 1.0);
        assert!(quality.face_size >= 0.0 && quality.face_size <= 1.0);
        assert!(quality.brightness >= 0.0 && quality.brightness <= 1.0);
        assert!(quality.sharpness >= 0.0 && quality.sharpness <= 1.0);
    }

    #[test]
    fn meets_quality_threshold_boundary() {
        let below = FrameQuality {
            centering: 0.3,
            face_size: 0.3,
            brightness: 0.3,
            sharpness: 0.3,
            overall: 0.34,
        };
        assert!(!meets_quality_threshold(&below));

        let above = FrameQuality {
            centering: 0.5,
            face_size: 0.5,
            brightness: 0.5,
            sharpness: 0.5,
            overall: 0.36,
        };
        assert!(meets_quality_threshold(&above));

        let at = FrameQuality {
            centering: 0.5,
            face_size: 0.5,
            brightness: 0.5,
            sharpness: 0.5,
            overall: 0.35,
        };
        assert!(meets_quality_threshold(&at));
    }

    #[test]
    fn brightness_score_dark_is_low() {
        // All-black face region
        let gray = vec![0u8; 640 * 480];
        let bbox = BoundingBox {
            x: 200.0,
            y: 100.0,
            width: 200.0,
            height: 200.0,
        };
        let score = score_brightness(&gray, &bbox, 640);
        assert!(score < 0.3, "all-dark should score low: {score}");
    }

    #[test]
    fn brightness_score_overexposed_is_low() {
        // All-white face region
        let gray = vec![255u8; 640 * 480];
        let bbox = BoundingBox {
            x: 200.0,
            y: 100.0,
            width: 200.0,
            height: 200.0,
        };
        let score = score_brightness(&gray, &bbox, 640);
        assert!(score < 0.3, "overexposed should score low: {score}");
    }

    #[test]
    fn brightness_score_mid_is_high() {
        // Mid-brightness face region
        let gray = vec![128u8; 640 * 480];
        let bbox = BoundingBox {
            x: 200.0,
            y: 100.0,
            width: 200.0,
            height: 200.0,
        };
        let score = score_brightness(&gray, &bbox, 640);
        assert!(score > 0.8, "mid-brightness should score high: {score}");
    }

    #[test]
    fn sharpness_score_uniform_is_low() {
        // Uniform region -> zero Laplacian -> low sharpness
        let gray = vec![128u8; 640 * 480];
        let bbox = BoundingBox {
            x: 100.0,
            y: 100.0,
            width: 100.0,
            height: 100.0,
        };
        let score = score_sharpness(&gray, &bbox, 640);
        assert!(
            score < 0.1,
            "uniform region should have low sharpness: {score}"
        );
    }

    #[test]
    fn sharpness_score_small_region_returns_zero() {
        let gray = vec![128u8; 640 * 480];
        let bbox = BoundingBox {
            x: 100.0,
            y: 100.0,
            width: 2.0, // too small for Laplacian
            height: 2.0,
        };
        let score = score_sharpness(&gray, &bbox, 640);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn face_size_too_large() {
        let bbox = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 600.0,
            height: 450.0,
        };
        // 600*450 / (640*480) = ~88% - way too large
        let score = score_face_size(&bbox, 640, 480);
        assert!(score < 0.5, "oversized face should score low: {score}");
    }

    #[test]
    fn angle_diversity_single_embedding() {
        let emb = [0.5f32; 512];
        assert!(
            check_angle_diversity(&[emb]),
            "single embedding should pass diversity check"
        );
    }

    #[test]
    fn angle_diversity_at_threshold() {
        // Create two embeddings with similarity exactly at threshold
        let mut emb1 = [0.0f32; 512];

        // Construct embeddings with specific similarity
        // For unit vectors that share most components but differ slightly
        let val = 1.0 / (512.0f32).sqrt();
        for x in emb1.iter_mut() {
            *x = val;
        }
        let mut emb2 = emb1;
        // Perturb one component slightly — this should change similarity below 0.95
        emb2[0] = -val * 5.0;
        // Re-normalize (approximate)
        let norm: f32 = emb2.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in emb2.iter_mut() {
            *x /= norm;
        }

        let sim = cosine_similarity(&emb1, &emb2);
        // The exact similarity depends on the perturbation; just verify diversity check works
        if sim < MAX_EMBEDDING_SIMILARITY {
            assert!(check_angle_diversity(&[emb1, emb2]));
        } else {
            assert!(!check_angle_diversity(&[emb1, emb2]));
        }
    }

    #[test]
    fn quality_hint_too_far() {
        let q = FrameQuality {
            centering: 0.5,
            face_size: 0.9, // > 0.8 threshold
            brightness: 0.5,
            sharpness: 0.5,
            overall: 0.5,
        };
        assert_eq!(quality_hint(&q), Some("Move further from the camera"));
    }

    #[test]
    fn quality_hint_bad_lighting() {
        let q = FrameQuality {
            centering: 0.5,
            face_size: 0.5,
            brightness: 0.1, // < 0.2
            sharpness: 0.5,
            overall: 0.4,
        };
        assert_eq!(quality_hint(&q), Some("Improve lighting conditions"));
    }

    #[test]
    fn quality_hint_blurry() {
        let q = FrameQuality {
            centering: 0.5,
            face_size: 0.5,
            brightness: 0.5,
            sharpness: 0.1, // < 0.2
            overall: 0.4,
        };
        assert_eq!(quality_hint(&q), Some("Hold still for a sharper image"));
    }

    #[test]
    fn quality_hint_no_issue() {
        let q = FrameQuality {
            centering: 0.8,
            face_size: 0.5,
            brightness: 0.5,
            sharpness: 0.5,
            overall: 0.6,
        };
        assert_eq!(quality_hint(&q), None);
    }

    #[test]
    fn quality_hints() {
        let low_centering = FrameQuality {
            centering: 0.1,
            face_size: 0.5,
            brightness: 0.5,
            sharpness: 0.5,
            overall: 0.4,
        };
        assert_eq!(
            quality_hint(&low_centering),
            Some("Center your face in the frame")
        );

        let low_size = FrameQuality {
            centering: 0.5,
            face_size: 0.1,
            brightness: 0.5,
            sharpness: 0.5,
            overall: 0.4,
        };
        assert_eq!(quality_hint(&low_size), Some("Move closer to the camera"));
    }
}
