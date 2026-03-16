use std::time::{Duration, Instant};

use facelock_camera::capture::is_dark_with_config;
use facelock_core::config::Config;
use facelock_core::ipc::DaemonResponse;
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_core::types::FaceEmbedding;
use facelock_store::FaceStore;
use facelock_tpm::SoftwareSealer;
use tracing::{debug, info, warn};

use crate::quality;

const MIN_CAPTURES: usize = 3;
const MAX_CAPTURES: usize = 10;
const INTER_FRAME_DELAY: Duration = Duration::from_millis(200);

pub fn enroll<C: CameraSource, E: FaceProcessor>(
    camera: &mut C,
    engine: &mut E,
    store: &FaceStore,
    config: &Config,
    user: &str,
    label: &str,
    sealer: Option<&SoftwareSealer>,
) -> DaemonResponse {
    // Clear any previous model with the same label (re-enrollment)
    match store.remove_model_by_label(user, label) {
        Ok(true) => info!(user, label, "removed existing model for re-enrollment"),
        Ok(false) => {}
        Err(e) => {
            warn!(user, label, "failed to remove existing model: {e}");
            return DaemonResponse::Error {
                message: format!("storage error clearing old model: {e}"),
            };
        }
    }

    // Use 3x the auth timeout for enrollment since we need multiple good captures
    let enroll_secs = (config.recognition.timeout_secs as u64).max(5) * 3;
    let deadline = Instant::now() + Duration::from_secs(enroll_secs);
    debug!(timeout_secs = enroll_secs, "starting enrollment");
    let mut stored_count: u32 = 0;
    let mut model_id: Option<u32> = None;
    let mut last_capture = Instant::now() - INTER_FRAME_DELAY; // allow immediate first capture
    let mut enrolled_embeddings: Vec<FaceEmbedding> = Vec::with_capacity(MAX_CAPTURES);

    while Instant::now() < deadline && (stored_count as usize) < MAX_CAPTURES {
        // Delay between captures for varied angles
        let since_last = Instant::now().duration_since(last_capture);
        if since_last < INTER_FRAME_DELAY {
            std::thread::sleep(INTER_FRAME_DELAY - since_last);
        }

        let capture_start = Instant::now();
        let frame = match camera.capture() {
            Ok(f) => f,
            Err(e) => {
                debug!("capture error during enroll: {e}");
                continue;
            }
        };
        let capture_ms = capture_start.elapsed().as_millis();

        if is_dark_with_config(
            &frame,
            config.device.dark_threshold,
            config.device.dark_pixel_value,
        ) {
            warn!(capture_ms, "skipping dark frame during enroll");
            continue;
        }

        let detect_start = Instant::now();
        let faces = match engine.process(&frame) {
            Ok(f) => f,
            Err(e) => {
                warn!("face engine error during enroll: {e}");
                continue;
            }
        };
        let detect_ms = detect_start.elapsed().as_millis();

        // Require exactly 1 face
        if faces.is_empty() {
            info!(capture_ms, detect_ms, "no face detected during enroll");
            continue;
        }
        if faces.len() > 1 {
            warn!(
                count = faces.len(),
                "multiple faces detected during enroll, skipping frame"
            );
            continue;
        }

        let (det, embedding) = &faces[0];

        // Quality gate: skip low-quality frames
        let frame_quality = quality::score_frame(det, &frame.gray, frame.width, frame.height);
        if !quality::meets_quality_threshold(&frame_quality) {
            if let Some(hint) = quality::quality_hint(&frame_quality) {
                debug!(
                    overall = format!("{:.2}", frame_quality.overall),
                    hint, "skipping low-quality enrollment frame"
                );
            } else {
                debug!(
                    overall = format!("{:.2}", frame_quality.overall),
                    "skipping low-quality enrollment frame"
                );
            }
            continue;
        }

        // First face: create the model. Subsequent faces: add embeddings.
        // When a sealer is provided, encrypt each embedding before storage.
        let store_result = if let Some(sealer) = sealer {
            match sealer.seal_embedding(embedding) {
                Ok(encrypted) => match model_id {
                    None => store
                        .add_model_raw(user, label, &encrypted, true, &config.recognition.embedder_model)
                        .map(Some),
                    Some(id) => store
                        .add_embedding_raw(id, &encrypted, true)
                        .map(|()| None),
                },
                Err(e) => {
                    warn!("failed to encrypt embedding: {e}");
                    return DaemonResponse::Error {
                        message: format!("encryption error: {e}"),
                    };
                }
            }
        } else {
            match model_id {
                None => store.add_model(user, label, embedding, &config.recognition.embedder_model).map(Some),
                Some(id) => store.add_embedding(id, embedding).map(|()| None),
            }
        };

        match store_result {
            Ok(Some(id)) => {
                model_id = Some(id);
                stored_count += 1;
                enrolled_embeddings.push(*embedding);
                info!(
                    capture_ms,
                    detect_ms,
                    model_id = id,
                    encrypted = sealer.is_some(),
                    "created model with first embedding"
                );
            }
            Ok(None) => {
                stored_count += 1;
                enrolled_embeddings.push(*embedding);
                debug!(
                    capture_ms,
                    detect_ms,
                    count = stored_count,
                    encrypted = sealer.is_some(),
                    "stored embedding"
                );
            }
            Err(e) => {
                if model_id.is_none() {
                    warn!("failed to create model: {e}");
                    return DaemonResponse::Error {
                        message: format!("storage error: {e}"),
                    };
                } else {
                    warn!("failed to store embedding: {e}");
                }
            }
        }

        last_capture = Instant::now();
    }

    // Check angle diversity: reject if all embeddings are too similar
    if stored_count >= MIN_CAPTURES as u32
        && !quality::check_angle_diversity(&enrolled_embeddings)
    {
        warn!(
            user,
            label,
            captured = stored_count,
            "insufficient angle diversity during enrollment"
        );
        return DaemonResponse::Error {
            message: "insufficient angle diversity: please move your head to different angles during enrollment".into(),
        };
    }

    if stored_count < MIN_CAPTURES as u32 {
        warn!(
            user,
            label,
            captured = stored_count,
            required = MIN_CAPTURES,
            "insufficient face captures during enrollment"
        );
        return DaemonResponse::Error {
            message: format!("only captured {stored_count} frames, need at least {MIN_CAPTURES}"),
        };
    }

    info!(
        user,
        label,
        model_id = model_id.unwrap_or(0),
        embedding_count = stored_count,
        "enrollment complete"
    );

    DaemonResponse::Enrolled {
        model_id: model_id.unwrap_or(0),
        embedding_count: stored_count,
    }
}
