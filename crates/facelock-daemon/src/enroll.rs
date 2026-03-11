use std::time::{Duration, Instant};

use facelock_core::config::Config;
use facelock_core::ipc::DaemonResponse;
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_store::FaceStore;
use tracing::{debug, info, warn};

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

        if C::is_dark(&frame) {
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
            warn!(count = faces.len(), "multiple faces detected during enroll, skipping frame");
            continue;
        }

        let (_det, embedding) = &faces[0];

        // First face: create the model. Subsequent faces: add embeddings.
        match model_id {
            None => match store.add_model(user, label, embedding) {
                Ok(id) => {
                    model_id = Some(id);
                    stored_count += 1;
                    info!(capture_ms, detect_ms, model_id = id, "created model with first embedding");
                }
                Err(e) => {
                    warn!("failed to create model: {e}");
                    return DaemonResponse::Error {
                        message: format!("storage error: {e}"),
                    };
                }
            },
            Some(id) => match store.add_embedding(id, embedding) {
                Ok(()) => {
                    stored_count += 1;
                    debug!(capture_ms, detect_ms, count = stored_count, "stored embedding");
                }
                Err(e) => {
                    warn!("failed to store embedding: {e}");
                }
            },
        }

        last_capture = Instant::now();
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
            message: format!(
                "only captured {stored_count} frames, need at least {MIN_CAPTURES}"
            ),
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
