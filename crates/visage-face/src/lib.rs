pub mod align;
pub mod detector;
pub mod embedder;
pub mod models;

use std::path::Path;

use visage_core::config::RecognitionConfig;
use visage_core::error::{VisageError, Result};
use visage_core::traits::FaceProcessor;
use visage_core::types::{Detection, FaceEmbedding, Frame};

pub use align::{align_face, compute_affine_matrix, AlignedFace};
pub use detector::FaceDetector;
pub use embedder::FaceEmbedder;
pub use models::{ModelManifest, verify_model};

/// Full face-processing pipeline: detect, align, embed.
pub struct FaceEngine {
    detector: FaceDetector,
    embedder: FaceEmbedder,
}

impl FaceEngine {
    /// Load models with SHA256 integrity verification.
    pub fn load(config: &RecognitionConfig, model_dir: &Path) -> Result<Self> {
        let manifest = ModelManifest::load()?;

        for model in manifest.default_models() {
            let path = model_dir.join(&model.filename);
            if !verify_model(&path, &model.sha256)? {
                return Err(VisageError::Detection(format!(
                    "Model integrity check failed for {}. Expected SHA256: {}. \
                     Re-run `visage setup` to re-download.",
                    model.filename, model.sha256
                )));
            }
        }

        let detector_path = model_dir.join("scrfd_2.5g_bnkps.onnx");
        let embedder_path = model_dir.join("w600k_r50.onnx");

        let detector = FaceDetector::load(
            &detector_path,
            config.detection_confidence,
            config.nms_threshold,
        )?;
        let embedder = FaceEmbedder::load(&embedder_path)?;

        Ok(Self { detector, embedder })
    }

    /// Run the full pipeline: detect faces, align each, extract embeddings.
    pub fn process(&mut self, frame: &Frame) -> Result<Vec<(Detection, FaceEmbedding)>> {
        let detections = self.detector.detect(frame)?;
        let mut results = Vec::with_capacity(detections.len());

        for det in detections {
            let aligned = align_face(frame, &det.landmarks)?;
            let embedding = self.embedder.embed(&aligned)?;
            results.push((det, embedding));
        }

        Ok(results)
    }
}

impl FaceProcessor for FaceEngine {
    fn process(&mut self, frame: &Frame) -> Result<Vec<(Detection, FaceEmbedding)>> {
        FaceEngine::process(self, frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn full_pipeline_requires_models() {
        // This test requires actual ONNX model files
        let config = RecognitionConfig::default();
        let model_dir = Path::new("/usr/share/visage/models");
        let mut engine = FaceEngine::load(&config, model_dir).unwrap();

        let frame = Frame {
            rgb: vec![128u8; 640 * 480 * 3],
            gray: vec![128u8; 640 * 480],
            width: 640,
            height: 480,
        };

        let _results = engine.process(&frame).unwrap();
    }
}
