//! Integration tests for the face engine.
//!
//! All tests are marked `#[ignore]` because they require:
//! - ONNX model files in `./models/` (or `/usr/share/howdy/models/`)
//!   - `scrfd_2.5g_bnkps.onnx` (SCRFD face detector)
//!   - `w600k_r50.onnx` (ArcFace embedding extractor)
//! - Test images containing faces
//!
//! Run with: `cargo test -p howdy-face -- --ignored`

use std::path::Path;

use howdy_core::config::RecognitionConfig;
use howdy_core::types::Frame;
use howdy_face::FaceEngine;

const MODEL_DIR: &str = "/usr/share/howdy/models";

/// Helper to create a synthetic uniform frame (no real face).
fn uniform_frame(width: u32, height: u32, value: u8) -> Frame {
    let pixel_count = (width * height) as usize;
    Frame {
        rgb: vec![value; pixel_count * 3],
        gray: vec![value; pixel_count],
        width,
        height,
    }
}

#[test]
#[ignore]
fn load_engine_with_default_config() {
    let config = RecognitionConfig::default();
    let model_dir = Path::new(MODEL_DIR);
    let engine = FaceEngine::load(&config, model_dir);
    assert!(
        engine.is_ok(),
        "FaceEngine should load with default config: {:?}",
        engine.err()
    );
}

#[test]
#[ignore]
fn process_uniform_frame_detects_no_faces() {
    let config = RecognitionConfig::default();
    let model_dir = Path::new(MODEL_DIR);
    let mut engine = FaceEngine::load(&config, model_dir).unwrap();

    // A uniform gray frame should not contain any faces
    let frame = uniform_frame(640, 480, 128);
    let results = engine.process(&frame).unwrap();
    assert!(
        results.is_empty(),
        "uniform frame should have no face detections, got {}",
        results.len()
    );
}

#[test]
#[ignore]
fn embedding_has_expected_dimension() {
    // This test requires a real image with a face.
    // Placeholder: when a face is detected, the embedding should be 512-dimensional.
    let config = RecognitionConfig::default();
    let model_dir = Path::new(MODEL_DIR);
    let mut engine = FaceEngine::load(&config, model_dir).unwrap();

    // TODO: Load a real test image with a face here.
    // For now, process a uniform frame and verify no crash.
    let frame = uniform_frame(640, 480, 128);
    let results = engine.process(&frame);
    assert!(results.is_ok(), "process should not crash on uniform frame");
}

#[test]
#[ignore]
fn embedding_l2_norm_is_approximately_one() {
    // ArcFace embeddings should be L2-normalized (norm ~= 1.0).
    // This test requires a real face image to produce an embedding.
    //
    // When a test image is available:
    // 1. Load the image as a Frame
    // 2. Run engine.process(&frame)
    // 3. For each (detection, embedding) pair:
    //    let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    //    assert!((norm - 1.0).abs() < 0.01, "embedding L2 norm should be ~1.0, got {norm}");
    //
    // TODO: Add a test fixture image with a clear frontal face.
    let config = RecognitionConfig::default();
    let model_dir = Path::new(MODEL_DIR);
    let _engine = FaceEngine::load(&config, model_dir).unwrap();
}
