use visage_core::error::Result;
use visage_core::traits::FaceProcessor;
use visage_core::types::{Detection, FaceEmbedding, Frame};

use crate::fixtures;

/// A mock face engine that returns configurable detections and embeddings.
pub struct MockFaceEngine {
    /// If set, process() returns this many detections per frame.
    detections_per_frame: usize,
    /// Base confidence for detections.
    confidence: f32,
    /// Embeddings to cycle through. If empty, uses known_embedding(seed).
    embeddings: Vec<FaceEmbedding>,
    /// Current embedding index (for cycling).
    embed_index: usize,
    /// Number of process() calls made.
    call_count: usize,
}

impl MockFaceEngine {
    /// Create a mock engine that detects one face per frame with the given embedding.
    pub fn one_face(embedding: FaceEmbedding) -> Self {
        Self {
            detections_per_frame: 1,
            confidence: 0.95,
            embeddings: vec![embedding],
            embed_index: 0,
            call_count: 0,
        }
    }

    /// Create a mock engine that detects no faces.
    pub fn no_faces() -> Self {
        Self {
            detections_per_frame: 0,
            confidence: 0.0,
            embeddings: Vec::new(),
            embed_index: 0,
            call_count: 0,
        }
    }

    /// Create a mock engine that cycles through multiple embeddings (for variance testing).
    pub fn cycling(embeddings: Vec<FaceEmbedding>) -> Self {
        Self {
            detections_per_frame: 1,
            confidence: 0.95,
            embeddings,
            embed_index: 0,
            call_count: 0,
        }
    }

    pub fn call_count(&self) -> usize {
        self.call_count
    }
}

impl FaceProcessor for MockFaceEngine {
    fn process(&mut self, _frame: &Frame) -> Result<Vec<(Detection, FaceEmbedding)>> {
        self.call_count += 1;

        if self.detections_per_frame == 0 {
            return Ok(Vec::new());
        }

        let mut results = Vec::with_capacity(self.detections_per_frame);
        for _ in 0..self.detections_per_frame {
            let embedding = if self.embeddings.is_empty() {
                fixtures::known_embedding(0)
            } else {
                let emb = self.embeddings[self.embed_index % self.embeddings.len()];
                self.embed_index += 1;
                emb
            };

            results.push((fixtures::center_detection(self.confidence), embedding));
        }

        Ok(results)
    }
}
