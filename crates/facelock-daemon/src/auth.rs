use std::path::Path;
use std::time::Instant;

use facelock_camera::capture::is_dark_with_config;
use facelock_camera::preprocess::check_ir_texture;
use facelock_core::config::{Config, SnapshotConfig};
use facelock_core::ipc::DaemonResponse;
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_core::types::{
    FaceEmbedding, Frame, MatchResult, best_match, check_frame_variance, zeroize_embedding,
    zeroize_stored_embeddings,
};
use facelock_store::FaceStore;
use image::codecs::jpeg::JpegEncoder;
use tracing::{debug, info, warn};

use crate::audit::{self, AuditEntry};
use crate::liveness::LandmarkTracker;
use crate::rate_limit::RateLimiter;

/// Run pre-flight checks that don't need the camera.
/// Returns Some(response) to short-circuit, or None to proceed with auth.
pub fn pre_check(
    config: &Config,
    store: &FaceStore,
    user: &str,
    rate_limiter: &mut RateLimiter,
    device_is_ir: bool,
) -> Option<DaemonResponse> {
    if config.security.disabled {
        warn!(user, "facelock is disabled");
        return Some(DaemonResponse::Error {
            message: "facelock is disabled".into(),
        });
    }

    if config.security.abort_if_ssh && is_ssh_session() {
        info!(user, "SSH session detected, aborting");
        return Some(DaemonResponse::Error {
            message: "SSH session detected".into(),
        });
    }

    if config.security.abort_if_lid_closed && is_lid_closed() {
        info!(user, "lid closed, aborting");
        return Some(DaemonResponse::Error {
            message: "lid closed".into(),
        });
    }

    let has_models = match store.has_models(user) {
        Ok(v) => v,
        Err(e) => {
            return Some(DaemonResponse::Error {
                message: format!("storage error: {e}"),
            });
        }
    };
    if !has_models {
        if config.security.suppress_unknown {
            info!(user, "no enrolled models, suppressing (suppress_unknown=true)");
            return Some(DaemonResponse::Suppressed);
        }
        return Some(DaemonResponse::AuthResult(MatchResult {
            matched: false,
            model_id: None,
            label: None,
            similarity: 0.0,
        }));
    }

    if !rate_limiter.check_and_record(user) {
        warn!(user, "rate limited");
        return Some(DaemonResponse::Error {
            message: "rate limited".into(),
        });
    }

    if config.security.require_ir && !device_is_ir {
        warn!(user, "IR camera required but device is not IR");
        return Some(DaemonResponse::Error {
            message: "IR camera required for authentication. Set security.require_ir = false to override (NOT RECOMMENDED).".into(),
        });
    }

    None
}

/// Run the camera-based authentication loop.
/// Called after pre_check returns None.
/// Loads embeddings from the store (plaintext only — does not handle encryption).
pub fn authenticate<C: CameraSource, E: FaceProcessor>(
    camera: &mut C,
    engine: &mut E,
    store: &FaceStore,
    config: &Config,
    user: &str,
    device_is_ir: bool,
) -> DaemonResponse {
    let mut stored = match store.get_user_embeddings(user) {
        Ok(v) => v,
        Err(e) => {
            return DaemonResponse::Error {
                message: format!("storage error: {e}"),
            };
        }
    };
    let models = store.list_models(user).unwrap_or_default();
    authenticate_inner(camera, engine, &mut stored, &models, config, user, device_is_ir)
}

/// Run the camera-based authentication loop with pre-loaded (decrypted) embeddings.
/// Called by the handler when encryption is active so embeddings are already decrypted.
pub fn authenticate_with_embeddings<C: CameraSource, E: FaceProcessor>(
    camera: &mut C,
    engine: &mut E,
    stored: &[(u32, FaceEmbedding)],
    models: &[facelock_core::types::FaceModelInfo],
    config: &Config,
    user: &str,
    device_is_ir: bool,
) -> DaemonResponse {
    let mut stored = stored.to_vec();
    let models = models.to_vec();
    authenticate_inner(camera, engine, &mut stored, &models, config, user, device_is_ir)
}

/// Save a snapshot of the last captured frame to disk.
/// Failures are logged but never propagate — snapshots must not block auth.
fn save_snapshot(snapshot_config: &SnapshotConfig, user: &str, similarity: f32, frame: &Frame) {
    let dir = Path::new(&snapshot_config.dir);
    if let Err(e) = std::fs::create_dir_all(dir) {
        warn!(dir = %dir.display(), error = %e, "failed to create snapshot directory");
        return;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let filename = format!("{user}_{timestamp}_{similarity:.2}.jpg");
    let path = dir.join(&filename);

    let mut buf = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, 80);
    if let Err(e) = encoder.encode(
        &frame.rgb,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgb8,
    ) {
        warn!(path = %path.display(), error = %e, "failed to encode snapshot JPEG");
        return;
    }

    if let Err(e) = std::fs::write(&path, &buf) {
        warn!(path = %path.display(), error = %e, "failed to write snapshot");
        return;
    }

    debug!(path = %path.display(), "saved auth snapshot");
}

fn authenticate_inner<C: CameraSource, E: FaceProcessor>(
    camera: &mut C,
    engine: &mut E,
    stored: &mut [(u32, FaceEmbedding)],
    models: &[facelock_core::types::FaceModelInfo],
    config: &Config,
    user: &str,
    device_is_ir: bool,
) -> DaemonResponse {
    let start = Instant::now();
    let label_for =
        |id: u32| -> Option<String> { models.iter().find(|m| m.id == id).map(|m| m.label.clone()) };

    let deadline =
        Instant::now() + std::time::Duration::from_secs(config.recognition.timeout_secs as u64);
    let threshold = config.recognition.threshold;
    let mut best_similarity: f32 = 0.0;
    let mut matched_frame_embeddings: Vec<FaceEmbedding> =
        Vec::with_capacity(config.security.min_auth_frames as usize);
    let mut dark_count: u32 = 0;
    let mut frame_count: u32 = 0;
    let mut best_model_id: Option<u32> = None;
    let mut landmark_tracker = LandmarkTracker::new(10);
    #[allow(unused_assignments)]
    let mut last_frame: Option<Frame> = None;

    while Instant::now() < deadline {
        let frame = match camera.capture() {
            Ok(f) => f,
            Err(e) => {
                debug!("capture error: {e}");
                continue;
            }
        };
        frame_count += 1;

        if is_dark_with_config(
            &frame,
            config.device.dark_threshold,
            config.device.dark_pixel_value,
        ) {
            dark_count += 1;
            debug!(frame = frame_count, "dark frame, skipping");
            continue;
        }

        last_frame = Some(frame.clone());

        let faces = match engine.process(&frame) {
            Ok(f) => f,
            Err(e) => {
                debug!(frame = frame_count, "face engine error: {e}");
                continue;
            }
        };

        if faces.is_empty() {
            debug!(frame = frame_count, "no faces detected");
            continue;
        }

        // Push landmarks from the first detected face for liveness tracking
        if let Some((det, _)) = faces.first() {
            landmark_tracker.push(det.landmarks);
        }

        // IR texture check: when using an IR camera, verify each detected face
        // has real skin texture (not a flat photo/screen replay attack).
        // Only applied to IR frames — RGB texture varies too much and would
        // cause false positives.
        if device_is_ir {
            let all_flat = faces
                .iter()
                .all(|(det, _)| !check_ir_texture(&frame.gray, &det.bbox, frame.width));
            if all_flat {
                debug!(
                    frame = frame_count,
                    "IR texture check failed on all faces, skipping frame"
                );
                continue;
            }
        }

        let mut frame_matched = false;
        for (det, embedding) in &faces {
            // Skip individual faces that fail IR texture check
            if device_is_ir && !check_ir_texture(&frame.gray, &det.bbox, frame.width) {
                debug!(
                    frame = frame_count,
                    "IR texture check failed for face, skipping"
                );
                continue;
            }
            let (frame_best_sim, frame_best_id) = best_match(embedding, stored);

            if frame_best_sim > best_similarity {
                best_similarity = frame_best_sim;
                best_model_id = frame_best_id;
            }

            if frame_best_sim >= threshold && !frame_matched {
                matched_frame_embeddings.push(*embedding);
                frame_matched = true;
            }

            debug!(
                frame = frame_count,
                similarity = format!("{frame_best_sim:.4}"),
                matched_frames = matched_frame_embeddings.len(),
                "face comparison"
            );
        }

        // Frame variance check + landmark liveness check
        if config.security.require_frame_variance {
            if matched_frame_embeddings.len() >= config.security.min_auth_frames as usize
                && check_frame_variance(&matched_frame_embeddings)
            {
                // If landmark liveness is required, check it too
                if config.security.require_landmark_liveness && !landmark_tracker.check_liveness() {
                    debug!(
                        frame = frame_count,
                        landmark_frames = landmark_tracker.frame_count(),
                        "landmark liveness not yet satisfied, continuing"
                    );
                    continue;
                }

                let duration = start.elapsed();
                info!(
                    user,
                    similarity = format!("{best_similarity:.4}"),
                    frames = frame_count,
                    matched = matched_frame_embeddings.len(),
                    duration_ms = duration.as_millis() as u64,
                    "authentication succeeded"
                );
                audit::write_audit_entry(
                    &config.audit,
                    &AuditEntry {
                        timestamp: audit::now_iso8601(),
                        user: user.to_string(),
                        result: "success".into(),
                        similarity: Some(best_similarity),
                        frame_count: Some(frame_count),
                        duration_ms: Some(duration.as_millis() as u64),
                        device: config.device.path.clone(),
                        model_label: best_model_id.and_then(&label_for),
                        error: None,
                    },
                );
                if config.snapshots.should_save(true) {
                    if let Some(ref snap_frame) = last_frame {
                        save_snapshot(&config.snapshots, user, best_similarity, snap_frame);
                    }
                }
                let response = DaemonResponse::AuthResult(MatchResult {
                    matched: true,
                    model_id: best_model_id,
                    label: best_model_id.and_then(&label_for),
                    similarity: best_similarity,
                });
                // Zero sensitive data before returning
                zeroize_stored_embeddings(stored);
                for emb in &mut matched_frame_embeddings {
                    zeroize_embedding(emb);
                }
                return response;
            }
        } else if best_similarity >= threshold {
            // If landmark liveness is required, check it even without variance
            if config.security.require_landmark_liveness && !landmark_tracker.check_liveness() {
                debug!(
                    frame = frame_count,
                    landmark_frames = landmark_tracker.frame_count(),
                    "landmark liveness not yet satisfied, continuing"
                );
                continue;
            }

            let duration = start.elapsed();
            info!(
                user,
                similarity = format!("{best_similarity:.4}"),
                frames = frame_count,
                duration_ms = duration.as_millis() as u64,
                "authentication succeeded (no variance check)"
            );
            audit::write_audit_entry(
                &config.audit,
                &AuditEntry {
                    timestamp: audit::now_iso8601(),
                    user: user.to_string(),
                    result: "success".into(),
                    similarity: Some(best_similarity),
                    frame_count: Some(frame_count),
                    duration_ms: Some(duration.as_millis() as u64),
                    device: config.device.path.clone(),
                    model_label: best_model_id.and_then(&label_for),
                    error: None,
                },
            );
            if config.snapshots.should_save(true) {
                if let Some(ref snap_frame) = last_frame {
                    save_snapshot(&config.snapshots, user, best_similarity, snap_frame);
                }
            }
            let response = DaemonResponse::AuthResult(MatchResult {
                matched: true,
                model_id: best_model_id,
                label: best_model_id.and_then(&label_for),
                similarity: best_similarity,
            });
            zeroize_stored_embeddings(stored);
            for emb in &mut matched_frame_embeddings {
                zeroize_embedding(emb);
            }
            return response;
        }
    }

    let duration = start.elapsed();

    // Zero sensitive data before returning
    zeroize_stored_embeddings(stored);
    for emb in &mut matched_frame_embeddings {
        zeroize_embedding(emb);
    }

    if dark_count == frame_count && frame_count > 0 {
        warn!(
            user,
            frames = frame_count,
            duration_ms = duration.as_millis() as u64,
            "all frames were dark"
        );
        audit::write_audit_entry(
            &config.audit,
            &AuditEntry {
                timestamp: audit::now_iso8601(),
                user: user.to_string(),
                result: "error".into(),
                similarity: None,
                frame_count: Some(frame_count),
                duration_ms: Some(duration.as_millis() as u64),
                device: config.device.path.clone(),
                model_label: None,
                error: Some("all frames dark".into()),
            },
        );
        // No snapshot for all-dark: last_frame is None since dark frames are skipped
        return DaemonResponse::Error {
            message: "all frames dark".into(),
        };
    }

    info!(
        user,
        similarity = format!("{best_similarity:.4}"),
        frames = frame_count,
        matched = matched_frame_embeddings.len(),
        duration_ms = duration.as_millis() as u64,
        "authentication failed"
    );

    audit::write_audit_entry(
        &config.audit,
        &AuditEntry {
            timestamp: audit::now_iso8601(),
            user: user.to_string(),
            result: "failure".into(),
            similarity: Some(best_similarity),
            frame_count: Some(frame_count),
            duration_ms: Some(duration.as_millis() as u64),
            device: config.device.path.clone(),
            model_label: None,
            error: None,
        },
    );

    if config.snapshots.should_save(false) {
        if let Some(ref snap_frame) = last_frame {
            save_snapshot(&config.snapshots, user, best_similarity, snap_frame);
        }
    }

    DaemonResponse::AuthResult(MatchResult {
        matched: false,
        model_id: None,
        label: None,
        similarity: best_similarity,
    })
}

fn is_ssh_session() -> bool {
    std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok()
}

fn is_lid_closed() -> bool {
    std::fs::read_to_string("/proc/acpi/button/lid/LID0/state")
        .map(|s| s.contains("closed"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_detection_with_env_vars() {
        let old_conn = std::env::var("SSH_CONNECTION").ok();
        let old_tty = std::env::var("SSH_TTY").ok();
        unsafe {
            std::env::remove_var("SSH_CONNECTION");
            std::env::remove_var("SSH_TTY");
        }

        assert!(!is_ssh_session());

        unsafe { std::env::set_var("SSH_CONNECTION", "1.2.3.4 5678 10.0.0.1 22") };
        assert!(is_ssh_session());
        unsafe { std::env::remove_var("SSH_CONNECTION") };

        unsafe { std::env::set_var("SSH_TTY", "/dev/pts/0") };
        assert!(is_ssh_session());
        unsafe { std::env::remove_var("SSH_TTY") };

        if let Some(v) = old_conn {
            unsafe { std::env::set_var("SSH_CONNECTION", v) };
        }
        if let Some(v) = old_tty {
            unsafe { std::env::set_var("SSH_TTY", v) };
        }
    }

    #[test]
    fn lid_closed_returns_false_on_missing_file() {
        let _result = is_lid_closed();
    }
}
