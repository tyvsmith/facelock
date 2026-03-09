use std::path::Path;

use howdy_core::error::{HowdyError, Result};
use howdy_core::types::FaceEmbedding;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;

use crate::align::AlignedFace;

pub struct FaceEmbedder {
    session: Session,
}

impl FaceEmbedder {
    /// Load an ArcFace ONNX model from the given path.
    pub fn load(model_path: &Path) -> Result<Self> {
        let session = Session::builder()
            .map_err(|e| HowdyError::Embedding(format!("Failed to create session builder: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| HowdyError::Embedding(format!("Failed to set optimization level: {e}")))?
            .with_intra_threads(4)
            .map_err(|e| HowdyError::Embedding(format!("Failed to set intra threads: {e}")))?
            .commit_from_file(model_path)
            .map_err(|e| {
                HowdyError::Embedding(format!(
                    "Failed to load model {}: {e}",
                    model_path.display()
                ))
            })?;

        Ok(Self { session })
    }

    /// Extract a 512-D embedding from an aligned face image.
    pub fn embed(&mut self, aligned: &AlignedFace) -> Result<FaceEmbedding> {
        let w = aligned.width as usize;
        let h = aligned.height as usize;
        let channels = 3usize;

        // Build NCHW tensor with normalization to [-1, 1]
        let mut data = vec![0.0f32; channels * h * w];
        for y in 0..h {
            for x in 0..w {
                for c in 0..channels {
                    let pixel = aligned.rgb[(y * w + x) * 3 + c] as f32;
                    let normalized = (pixel - 127.5) / 127.5;
                    data[c * h * w + y * w + x] = normalized;
                }
            }
        }

        let shape = [1i64, channels as i64, h as i64, w as i64];
        let input_value = Tensor::from_array((shape.as_slice(), data.into_boxed_slice()))
            .map_err(|e| HowdyError::Embedding(format!("Failed to create input tensor: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![input_value])
            .map_err(|e| HowdyError::Embedding(format!("Inference failed: {e}")))?;

        let (_shape, embedding_data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| HowdyError::Embedding(format!("Failed to extract embedding: {e}")))?;

        let mut embedding: FaceEmbedding = [0.0f32; 512];
        for (i, val) in embedding.iter_mut().enumerate() {
            *val = embedding_data[i];
        }

        l2_normalize(&mut embedding);

        Ok(embedding)
    }
}

/// L2-normalize a 512-D vector in-place. Zero vectors are left as-is.
pub fn l2_normalize(v: &mut [f32; 512]) {
    let norm_sq: f32 = v.iter().map(|x| x * x).sum();
    if norm_sq < 1e-10 {
        return;
    }
    let norm = norm_sq.sqrt();
    for x in v.iter_mut() {
        *x /= norm;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_produces_unit_norm() {
        let mut v = [0.0f32; 512];
        for (i, val) in v.iter_mut().enumerate() {
            *val = (i as f32 + 1.0) * 0.01;
        }

        l2_normalize(&mut v);

        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "l2_normalize should produce unit norm, got {norm}"
        );
    }

    #[test]
    fn l2_normalize_zero_vector_stays_zero() {
        let mut v = [0.0f32; 512];
        l2_normalize(&mut v);

        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            norm.abs() < 1e-10,
            "zero vector should stay zero after l2_normalize, got norm {norm}"
        );
    }
}
