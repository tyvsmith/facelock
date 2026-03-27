pub mod align;
pub mod detector;
pub mod embedder;
pub mod models;
mod provider;

use std::path::Path;

use facelock_core::config::RecognitionConfig;
use facelock_core::error::{FacelockError, Result};
use facelock_core::traits::FaceProcessor;
use facelock_core::types::{Detection, FaceEmbedding, Frame};

pub use align::{AlignedFace, align_face, compute_affine_matrix};
pub use detector::FaceDetector;
pub use embedder::FaceEmbedder;
pub use models::{ModelManifest, resolve_model_sha256, verify_model};

/// Full face-processing pipeline: detect, align, embed.
pub struct FaceEngine {
    detector: FaceDetector,
    embedder: FaceEmbedder,
}

impl FaceEngine {
    /// Load models with SHA256 integrity verification.
    pub fn load(config: &RecognitionConfig, model_dir: &Path) -> Result<Self> {
        let manifest = ModelManifest::load()?;

        let detector_path = model_dir.join(&config.detector_model);
        let embedder_path = model_dir.join(&config.embedder_model);
        let detector_sha256 = resolve_model_sha256(
            &manifest,
            &config.detector_model,
            config.detector_sha256.as_deref(),
        )?;
        let embedder_sha256 = resolve_model_sha256(
            &manifest,
            &config.embedder_model,
            config.embedder_sha256.as_deref(),
        )?;

        if !verify_model(&detector_path, &detector_sha256)? {
            return Err(FacelockError::Detection(format!(
                "Model integrity check failed for {}. Expected SHA256: {}. \
                 Re-run `facelock setup` to re-download or update the configured checksum.",
                config.detector_model, detector_sha256
            )));
        }

        if !verify_model(&embedder_path, &embedder_sha256)? {
            return Err(FacelockError::Detection(format!(
                "Model integrity check failed for {}. Expected SHA256: {}. \
                 Re-run `facelock setup` to re-download or update the configured checksum.",
                config.embedder_model, embedder_sha256
            )));
        }

        let detector = FaceDetector::load(
            &detector_path,
            config.detection_confidence,
            config.nms_threshold,
            config.threads,
            &config.execution_provider,
        )?;
        let embedder =
            FaceEmbedder::load(&embedder_path, config.threads, &config.execution_provider)?;

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
        let model_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
        let model_dir = model_dir.as_path();
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
