use std::time::{Duration, Instant};

use howdy_camera::Camera;
use howdy_core::ipc::DaemonResponse;
use howdy_face::FaceEngine;
use howdy_store::FaceStore;
use tracing::{debug, info, warn};

const ENROLL_DURATION: Duration = Duration::from_secs(3);
const MIN_CAPTURES: usize = 3;
const MAX_CAPTURES: usize = 10;
const INTER_FRAME_DELAY: Duration = Duration::from_millis(200);

pub fn enroll(
    camera: &mut Camera<'_>,
    engine: &mut FaceEngine,
    store: &FaceStore,
    user: &str,
    label: &str,
) -> DaemonResponse {
    let deadline = Instant::now() + ENROLL_DURATION;
    let mut stored_count: u32 = 0;
    let mut model_id: Option<u32> = None;
    let mut last_capture = Instant::now() - INTER_FRAME_DELAY; // allow immediate first capture

    while Instant::now() < deadline && (stored_count as usize) < MAX_CAPTURES {
        // Delay between captures for varied angles
        let since_last = Instant::now().duration_since(last_capture);
        if since_last < INTER_FRAME_DELAY {
            std::thread::sleep(INTER_FRAME_DELAY - since_last);
        }

        let frame = match camera.capture() {
            Ok(f) => f,
            Err(e) => {
                debug!("capture error during enroll: {e}");
                continue;
            }
        };

        if Camera::is_dark(&frame) {
            debug!("skipping dark frame during enroll");
            continue;
        }

        let faces = match engine.process(&frame) {
            Ok(f) => f,
            Err(e) => {
                debug!("face engine error during enroll: {e}");
                continue;
            }
        };

        // Require exactly 1 face
        if faces.is_empty() {
            debug!("no face detected during enroll");
            continue;
        }
        if faces.len() > 1 {
            warn!("multiple faces detected during enroll, skipping frame");
            continue;
        }

        let (_det, embedding) = &faces[0];

        match store.add_model(user, label, embedding) {
            Ok(id) => {
                if model_id.is_none() {
                    model_id = Some(id);
                }
                stored_count += 1;
                debug!(user, label, count = stored_count, "stored embedding");
            }
            Err(e) => {
                // On duplicate label, the first add succeeds and subsequent ones may fail.
                // This is expected since we store multiple embeddings under one model.
                debug!("store error: {e}");
            }
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
