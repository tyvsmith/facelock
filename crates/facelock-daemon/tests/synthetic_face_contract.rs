//! The loopback tier's fixture, checked against the real models.
//!
//! `test/loopback/` feeds a v4l2loopback node with the sequence
//! `facelock_test_support::synthetic_face` renders, and the camera-required
//! E2E scripts then enroll and authenticate against it. Those scripts assert
//! outcomes; this test pins the bands the fixture has to sit in for the
//! outcomes to mean anything, using the same detector, embedder and gate
//! arithmetic the daemon runs:
//!
//! - every frame detects exactly one face, above the default confidence
//! - every frame passes the IR texture gate at its default cutoff
//! - every frame meets the enrollment quality floor
//! - every frame matches frame 0 above the container's recognition threshold
//! - every consecutive pair drifts at or below the frame-variance cutoff, so
//!   `require_frame_variance = true` can pass on a full window
//! - some pair inside an enrollment window sits below the angle-diversity
//!   ceiling
//!
//! A fixture that fails here would make the loopback tier pass for the wrong
//! reason or fail for a reason that is not a product bug. Needs the ONNX
//! models in `models/`, so it is `#[ignore]` like the other model-backed
//! tests: `cargo test -p facelock-daemon --test synthetic_face_contract -- --ignored`.

use std::path::Path;

use facelock_camera::check_ir_texture;
use facelock_core::config::{RecognitionConfig, SecurityConfig};
use facelock_core::types::{DEFAULT_FRAME_VARIANCE_MAX_SIMILARITY, Frame, cosine_similarity};
use facelock_daemon::quality;
use facelock_face::FaceEngine;
use facelock_test_support::synthetic_face::{FRAMES, HEIGHT, Pose, WIDTH, render};

/// `recognition.threshold` in `test/container-config.toml`, the config the
/// loopback tier runs under.
const CONTAINER_RECOGNITION_THRESHOLD: f32 = 0.80;
/// How far above `recognition.detection_confidence` every frame has to
/// score, so a resampling difference on the capture path cannot drop a
/// frame under the cutoff.
const DETECTION_MARGIN: f32 = 0.08;
/// `quality::MAX_EMBEDDING_SIMILARITY`, the enrollment diversity ceiling.
const ANGLE_DIVERSITY_CEILING: f32 = 0.95;
/// `enroll::MAX_CAPTURES`: how many consecutive accepted frames an
/// enrollment window can span at most.
const ENROLL_WINDOW: usize = 10;

fn mono_frame(gray: Vec<u8>) -> Frame {
    let rgb = gray.iter().flat_map(|&g| [g, g, g]).collect();
    Frame {
        rgb,
        gray,
        width: WIDTH,
        height: HEIGHT,
    }
}

#[test]
#[ignore = "needs the ONNX models in models/"]
fn synthetic_sequence_sits_inside_every_live_gate() {
    let model_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
    let recognition = RecognitionConfig::default();
    let security = SecurityConfig::default();
    let mut engine = FaceEngine::load(&recognition, &model_dir).expect("models load");

    let mut embeddings = Vec::with_capacity(FRAMES as usize);
    for i in 0..FRAMES {
        let frame = mono_frame(render(&Pose::for_frame(i, FRAMES)));
        let faces = engine.process(&frame).expect("engine runs");
        assert_eq!(
            faces.len(),
            1,
            "frame {i}: expected one face, got {}",
            faces.len()
        );
        let (det, embedding) = &faces[0];
        assert!(
            det.confidence >= recognition.detection_confidence + DETECTION_MARGIN,
            "frame {i}: confidence {:.3} is within {DETECTION_MARGIN} of the cutoff {}",
            det.confidence,
            recognition.detection_confidence
        );
        assert!(
            check_ir_texture(
                &frame.gray,
                &det.bbox,
                WIDTH,
                security.ir_texture_min_stddev
            ),
            "frame {i}: IR texture gate would reject the face"
        );
        let q = quality::score_frame(det, &frame.gray, WIDTH, HEIGHT);
        assert!(
            quality::meets_quality_threshold(&q),
            "frame {i}: enrollment quality {q:?} below the floor"
        );
        eprintln!(
            "frame {i:2}: conf={:.3} bbox=({:.0},{:.0} {:.0}x{:.0}) quality={:.2}",
            det.confidence, det.bbox.x, det.bbox.y, det.bbox.width, det.bbox.height, q.overall
        );
        embeddings.push(*embedding);
    }

    let mut min_consecutive = 1.0f32;
    let mut max_consecutive = 0.0f32;
    for (i, pair) in embeddings.windows(2).enumerate() {
        let sim = cosine_similarity(&pair[0], &pair[1]);
        min_consecutive = min_consecutive.min(sim);
        max_consecutive = max_consecutive.max(sim);
        assert!(
            sim <= DEFAULT_FRAME_VARIANCE_MAX_SIMILARITY,
            "frames {i}->{}: consecutive similarity {sim:.4} reads as a static input",
            i + 1
        );
    }
    // The loop wraps: the feeder plays the last frame before the first.
    let wrap = cosine_similarity(&embeddings[FRAMES as usize - 1], &embeddings[0]);
    assert!(
        wrap <= DEFAULT_FRAME_VARIANCE_MAX_SIMILARITY,
        "loop seam similarity {wrap:.4} reads as a static input"
    );
    eprintln!(
        "consecutive similarity: min={min_consecutive:.4} max={max_consecutive:.4} seam={wrap:.4}"
    );

    // Enrollment stores whichever consecutive window the feeder was on, and
    // authentication takes the best match of a live frame against that
    // window. The worst case is a live frame against a window that does not
    // contain it, so every pair of frames has to clear the threshold, not
    // just every frame against frame 0.
    let mut min_pair = 1.0f32;
    for (i, a) in embeddings.iter().enumerate() {
        for (j, b) in embeddings.iter().enumerate().skip(i + 1) {
            let sim = cosine_similarity(a, b);
            min_pair = min_pair.min(sim);
            assert!(
                sim >= CONTAINER_RECOGNITION_THRESHOLD,
                "frames {i} and {j}: similarity {sim:.4} is below the recognition threshold"
            );
        }
    }
    eprintln!("pairwise similarity: min={min_pair:.4}");

    // Enrollment accepts up to ENROLL_WINDOW consecutive frames, starting
    // wherever the feeder happens to be. Every such window needs one pair
    // under the ceiling or `insufficient angle diversity` refuses the enroll.
    for start in 0..FRAMES as usize {
        let window: Vec<_> = (0..ENROLL_WINDOW)
            .map(|k| embeddings[(start + k) % FRAMES as usize])
            .collect();
        assert!(
            quality::check_angle_diversity(&window),
            "enrollment window starting at frame {start} has no pair under {ANGLE_DIVERSITY_CEILING}"
        );
    }
}

/// A paused replay: the same frame again and again. The fixture has no
/// per-frame noise, so this is byte-identical input, and the gate has to
/// read it as static or the moving sequence passing would prove nothing.
#[test]
#[ignore = "needs the ONNX models in models/"]
fn paused_replay_reads_as_static() {
    let model_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models");
    let recognition = RecognitionConfig::default();
    let mut engine = FaceEngine::load(&recognition, &model_dir).expect("models load");

    let pose = Pose::for_frame(0, FRAMES);
    let mut embeddings = Vec::new();
    for _ in 0..3 {
        let frame = mono_frame(render(&pose));
        let faces = engine.process(&frame).expect("engine runs");
        assert_eq!(faces.len(), 1, "expected one face");
        embeddings.push(faces[0].1);
    }
    for pair in embeddings.windows(2) {
        let sim = cosine_similarity(&pair[0], &pair[1]);
        assert!(
            sim > DEFAULT_FRAME_VARIANCE_MAX_SIMILARITY,
            "a repeated frame scored {sim:.4}, under the variance cutoff"
        );
    }
}
