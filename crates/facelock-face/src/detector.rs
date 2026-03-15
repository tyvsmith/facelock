use std::path::Path;

use facelock_core::error::{FacelockError, Result};
use facelock_core::types::{BoundingBox, Detection, Frame, Point2D};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

pub struct FaceDetector {
    session: Session,
    input_width: u32,
    input_height: u32,
    confidence_threshold: f32,
    nms_threshold: f32,
}

/// Strides used by the SCRFD model.
const STRIDES: [u32; 3] = [8, 16, 32];

impl FaceDetector {
    /// Load an SCRFD ONNX model from the given path.
    ///
    /// `threads` controls the number of intra-op threads for ORT inference.
    pub fn load(
        model_path: &Path,
        confidence: f32,
        nms: f32,
        threads: u32,
        execution_provider: &str,
    ) -> Result<Self> {
        let builder = Session::builder()
            .map_err(|e| {
                FacelockError::Detection(format!("Failed to create session builder: {e}"))
            })?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| {
                FacelockError::Detection(format!("Failed to set optimization level: {e}"))
            })?
            .with_intra_threads(threads as usize)
            .map_err(|e| FacelockError::Detection(format!("Failed to set intra threads: {e}")))?;

        let mut builder = crate::provider::register_execution_provider(builder, execution_provider)
            .map_err(|e| {
                FacelockError::Detection(format!("Failed to set execution provider: {e}"))
            })?;

        let session = builder.commit_from_file(model_path).map_err(|e| {
            FacelockError::Detection(format!(
                "Failed to load model {}: {e}",
                model_path.display()
            ))
        })?;

        Ok(Self {
            session,
            input_width: 640,
            input_height: 640,
            confidence_threshold: confidence,
            nms_threshold: nms,
        })
    }

    /// Detect faces in a frame.
    pub fn detect(&mut self, frame: &Frame) -> Result<Vec<Detection>> {
        let (tensor_data, scale, pad_x, pad_y) =
            letterbox(&frame.rgb, frame.width, frame.height, self.input_width);

        let shape = [1i64, 3, self.input_height as i64, self.input_width as i64];
        let input_value = Tensor::from_array((shape.as_slice(), tensor_data.into_boxed_slice()))
            .map_err(|e| FacelockError::Detection(format!("Failed to create input tensor: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![input_value])
            .map_err(|e| FacelockError::Detection(format!("Inference failed: {e}")))?;

        let mut detections = Vec::new();

        // SCRFD outputs 9 tensors: 3 strides x (scores, bboxes, landmarks)
        // Order: score_8, score_16, score_32, bbox_8, bbox_16, bbox_32, kps_8, kps_16, kps_32
        for (stride_idx, &stride) in STRIDES.iter().enumerate() {
            let score_idx = stride_idx;
            let bbox_idx = stride_idx + 3;
            let kps_idx = stride_idx + 6;

            let (_score_shape, scores) =
                outputs[score_idx]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| {
                        FacelockError::Detection(format!("Failed to extract scores tensor: {e}"))
                    })?;
            let (_bbox_shape, bboxes) =
                outputs[bbox_idx].try_extract_tensor::<f32>().map_err(|e| {
                    FacelockError::Detection(format!("Failed to extract bbox tensor: {e}"))
                })?;
            let (_kps_shape, landmarks_data) =
                outputs[kps_idx].try_extract_tensor::<f32>().map_err(|e| {
                    FacelockError::Detection(format!("Failed to extract landmarks tensor: {e}"))
                })?;

            let feat_h = self.input_height / stride;
            let feat_w = self.input_width / stride;
            let anchors = generate_anchors(feat_h, feat_w, stride);
            let num_anchors = anchors.len();

            // Scores are 1 per anchor; determine stride from flat length
            let score_stride = scores.len() / num_anchors;

            for (i, (anchor_x, anchor_y)) in anchors.iter().enumerate() {
                let score = scores[i * score_stride];

                if score < self.confidence_threshold {
                    continue;
                }

                let stride_f = stride as f32;

                // Bboxes: [1, N, 4] -> flat index: i * 4 + offset
                let b_base = i * 4;
                let x1 = (anchor_x - bboxes[b_base]) * stride_f;
                let y1 = (anchor_y - bboxes[b_base + 1]) * stride_f;
                let x2 = (anchor_x + bboxes[b_base + 2]) * stride_f;
                let y2 = (anchor_y + bboxes[b_base + 3]) * stride_f;

                // Undo letterbox transform
                let x1_orig = (x1 - pad_x) / scale;
                let y1_orig = (y1 - pad_y) / scale;
                let x2_orig = (x2 - pad_x) / scale;
                let y2_orig = (y2 - pad_y) / scale;

                // Clamp to image bounds
                let x1_clamped = x1_orig.max(0.0).min(frame.width as f32);
                let y1_clamped = y1_orig.max(0.0).min(frame.height as f32);
                let x2_clamped = x2_orig.max(0.0).min(frame.width as f32);
                let y2_clamped = y2_orig.max(0.0).min(frame.height as f32);

                let bbox = BoundingBox {
                    x: x1_clamped,
                    y: y1_clamped,
                    width: (x2_clamped - x1_clamped).max(0.0),
                    height: (y2_clamped - y1_clamped).max(0.0),
                };

                // Decode landmarks: [1, N, 10] -> flat index: i * 10 + offset
                let mut lms = [Point2D { x: 0.0, y: 0.0 }; 5];
                let l_base = i * 10;
                for j in 0..5 {
                    let lx = (anchor_x + landmarks_data[l_base + j * 2]) * stride_f;
                    let ly = (anchor_y + landmarks_data[l_base + j * 2 + 1]) * stride_f;
                    lms[j] = Point2D {
                        x: (lx - pad_x) / scale,
                        y: (ly - pad_y) / scale,
                    };
                }

                detections.push(Detection {
                    bbox,
                    confidence: score,
                    landmarks: lms,
                });
            }
        }

        nms(&mut detections, self.nms_threshold);

        Ok(detections)
    }
}

/// Letterbox-resize an RGB image to target_size x target_size, preserving aspect ratio.
/// Returns (NCHW f32 tensor, scale, pad_x, pad_y).
pub fn letterbox(
    rgb: &[u8],
    src_w: u32,
    src_h: u32,
    target_size: u32,
) -> (Vec<f32>, f32, f32, f32) {
    let scale = (target_size as f32 / src_w as f32).min(target_size as f32 / src_h as f32);
    let new_w = (src_w as f32 * scale).round() as u32;
    let new_h = (src_h as f32 * scale).round() as u32;
    let pad_x = (target_size - new_w) as f32 / 2.0;
    let pad_y = (target_size - new_h) as f32 / 2.0;

    let pad_x_int = pad_x as u32;
    let pad_y_int = pad_y as u32;

    let ts = target_size as usize;
    let channels = 3usize;
    let mut output = vec![0.0f32; channels * ts * ts];

    // Bilinear resize + place into padded output
    for dy in 0..new_h {
        for dx in 0..new_w {
            // Map back to source coordinates
            let sx = dx as f32 * (src_w as f32 / new_w as f32);
            let sy = dy as f32 * (src_h as f32 / new_h as f32);

            let x0 = sx.floor() as u32;
            let y0 = sy.floor() as u32;
            let x1 = (x0 + 1).min(src_w - 1);
            let y1 = (y0 + 1).min(src_h - 1);

            let fx = sx - x0 as f32;
            let fy = sy - y0 as f32;

            let out_x = dx + pad_x_int;
            let out_y = dy + pad_y_int;
            if out_x >= target_size || out_y >= target_size {
                continue;
            }

            for c in 0..3 {
                let p00 = rgb[(y0 as usize * src_w as usize + x0 as usize) * 3 + c] as f32;
                let p10 = rgb[(y0 as usize * src_w as usize + x1 as usize) * 3 + c] as f32;
                let p01 = rgb[(y1 as usize * src_w as usize + x0 as usize) * 3 + c] as f32;
                let p11 = rgb[(y1 as usize * src_w as usize + x1 as usize) * 3 + c] as f32;

                let val = p00 * (1.0 - fx) * (1.0 - fy)
                    + p10 * fx * (1.0 - fy)
                    + p01 * (1.0 - fx) * fy
                    + p11 * fx * fy;

                // Normalize: (pixel - 127.5) / 128.0
                let normalized = (val - 127.5) / 128.0;
                // NCHW layout: [c, out_y, out_x]
                output[c * ts * ts + out_y as usize * ts + out_x as usize] = normalized;
            }
        }
    }

    (output, scale, pad_x, pad_y)
}

/// Generate anchor centers for a feature map of the given size and stride.
pub fn generate_anchors(feat_h: u32, feat_w: u32, _stride: u32) -> Vec<(f32, f32)> {
    // SCRFD uses 2 anchors per location
    let mut anchors = Vec::with_capacity((feat_h * feat_w * 2) as usize);
    for y in 0..feat_h {
        for x in 0..feat_w {
            anchors.push((x as f32, y as f32));
            anchors.push((x as f32, y as f32));
        }
    }
    anchors
}

/// Compute Intersection-over-Union of two bounding boxes.
pub fn compute_iou(a: &BoundingBox, b: &BoundingBox) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.width).min(b.x + b.width);
    let y2 = (a.y + a.height).min(b.y + b.height);

    let inter_w = (x2 - x1).max(0.0);
    let inter_h = (y2 - y1).max(0.0);
    let intersection = inter_w * inter_h;

    let area_a = a.width * a.height;
    let area_b = b.width * b.height;
    let union = area_a + area_b - intersection;

    if union <= 0.0 {
        return 0.0;
    }

    intersection / union
}

/// Non-Maximum Suppression: sort by confidence descending, remove overlapping detections.
pub fn nms(detections: &mut Vec<Detection>, iou_threshold: f32) {
    detections.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut keep = Vec::with_capacity(detections.len());
    let det_snapshot: Vec<BoundingBox> = detections.iter().map(|d| d.bbox).collect();

    for i in 0..detections.len() {
        let mut suppressed = false;
        for &k in &keep {
            if compute_iou(&det_snapshot[i], &det_snapshot[k]) > iou_threshold {
                suppressed = true;
                break;
            }
        }
        if !suppressed {
            keep.push(i);
        }
    }

    let mut idx = 0;
    let keep_set: std::collections::HashSet<usize> = keep.into_iter().collect();
    detections.retain(|_| {
        let result = keep_set.contains(&idx);
        idx += 1;
        result
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_detection(x: f32, y: f32, w: f32, h: f32, conf: f32) -> Detection {
        Detection {
            bbox: BoundingBox {
                x,
                y,
                width: w,
                height: h,
            },
            confidence: conf,
            landmarks: [Point2D { x: 0.0, y: 0.0 }; 5],
        }
    }

    #[test]
    fn iou_identical_boxes() {
        let b = BoundingBox {
            x: 10.0,
            y: 10.0,
            width: 50.0,
            height: 50.0,
        };
        let iou = compute_iou(&b, &b);
        assert!(
            (iou - 1.0).abs() < 1e-5,
            "identical boxes should have IoU 1.0, got {iou}"
        );
    }

    #[test]
    fn iou_non_overlapping() {
        let a = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b = BoundingBox {
            x: 100.0,
            y: 100.0,
            width: 10.0,
            height: 10.0,
        };
        let iou = compute_iou(&a, &b);
        assert!(
            iou.abs() < 1e-5,
            "non-overlapping boxes should have IoU 0.0, got {iou}"
        );
    }

    #[test]
    fn iou_partial_overlap() {
        let a = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b = BoundingBox {
            x: 5.0,
            y: 5.0,
            width: 10.0,
            height: 10.0,
        };
        let expected = 25.0 / 175.0;
        let iou = compute_iou(&a, &b);
        assert!(
            (iou - expected).abs() < 1e-5,
            "partial overlap IoU expected {expected}, got {iou}"
        );
    }

    #[test]
    fn nms_keeps_highest_confidence() {
        let mut dets = vec![
            make_detection(0.0, 0.0, 100.0, 100.0, 0.8),
            make_detection(5.0, 5.0, 100.0, 100.0, 0.9),
            make_detection(2.0, 2.0, 100.0, 100.0, 0.7),
        ];
        nms(&mut dets, 0.5);
        assert_eq!(dets.len(), 1, "NMS should keep only 1 detection");
        assert!(
            (dets[0].confidence - 0.9).abs() < 1e-5,
            "NMS should keep the highest confidence detection"
        );
    }

    #[test]
    fn letterbox_square_image_no_padding() {
        let size = 640u32;
        let rgb = vec![128u8; (size * size * 3) as usize];
        let (_, scale, pad_x, pad_y) = letterbox(&rgb, size, size, size);
        assert!(
            (scale - 1.0).abs() < 1e-5,
            "square image scale should be 1.0, got {scale}"
        );
        assert!(
            pad_x.abs() < 1e-5,
            "square image should have no x padding, got {pad_x}"
        );
        assert!(
            pad_y.abs() < 1e-5,
            "square image should have no y padding, got {pad_y}"
        );
    }

    #[test]
    fn letterbox_wide_image_vertical_padding() {
        let rgb = vec![128u8; (1280 * 480 * 3) as usize];
        let (_, scale, _pad_x, pad_y) = letterbox(&rgb, 1280, 480, 640);
        assert!(
            (scale - 0.5).abs() < 1e-5,
            "wide image scale should be 0.5, got {scale}"
        );
        assert!(
            pad_y > 0.0,
            "wide image should have vertical padding, got {pad_y}"
        );
    }
}
