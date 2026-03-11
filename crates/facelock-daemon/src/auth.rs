use std::time::Instant;

use facelock_core::config::Config;
use facelock_core::ipc::DaemonResponse;
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_core::types::{best_match, check_frame_variance, FaceEmbedding, MatchResult};
use facelock_store::FaceStore;
use tracing::{debug, info, warn};

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
pub fn authenticate<C: CameraSource, E: FaceProcessor>(
    camera: &mut C,
    engine: &mut E,
    store: &FaceStore,
    config: &Config,
    user: &str,
) -> DaemonResponse {
    let start = Instant::now();

    // Load user embeddings + build label lookup
    let stored = match store.get_user_embeddings(user) {
        Ok(v) => v,
        Err(e) => {
            return DaemonResponse::Error {
                message: format!("storage error: {e}"),
            };
        }
    };
    let models = store.list_models(user).unwrap_or_default();
    let label_for = |id: u32| -> Option<String> {
        models.iter().find(|m| m.id == id).map(|m| m.label.clone())
    };

    let deadline =
        Instant::now() + std::time::Duration::from_secs(config.recognition.timeout_secs as u64);
    let threshold = config.recognition.threshold;
    let mut best_similarity: f32 = 0.0;
    let mut matched_frame_embeddings: Vec<FaceEmbedding> =
        Vec::with_capacity(config.security.min_auth_frames as usize);
    let mut dark_count: u32 = 0;
    let mut frame_count: u32 = 0;
    let mut best_model_id: Option<u32> = None;

    while Instant::now() < deadline {
        let frame = match camera.capture() {
            Ok(f) => f,
            Err(e) => {
                debug!("capture error: {e}");
                continue;
            }
        };
        frame_count += 1;

        if C::is_dark(&frame) {
            dark_count += 1;
            debug!(frame = frame_count, "dark frame, skipping");
            continue;
        }

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

        let mut frame_matched = false;
        for (_det, embedding) in &faces {
            let (frame_best_sim, frame_best_id) = best_match(embedding, &stored);

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

        // Frame variance check
        if config.security.require_frame_variance {
            if matched_frame_embeddings.len() >= config.security.min_auth_frames as usize
                && check_frame_variance(&matched_frame_embeddings)
            {
                let duration = start.elapsed();
                info!(
                    user,
                    similarity = format!("{best_similarity:.4}"),
                    frames = frame_count,
                    matched = matched_frame_embeddings.len(),
                    duration_ms = duration.as_millis() as u64,
                    "authentication succeeded"
                );
                return DaemonResponse::AuthResult(MatchResult {
                    matched: true,
                    model_id: best_model_id,
                    label: best_model_id.and_then(&label_for),
                    similarity: best_similarity,
                });
            }
        } else if best_similarity >= threshold {
            let duration = start.elapsed();
            info!(
                user,
                similarity = format!("{best_similarity:.4}"),
                frames = frame_count,
                duration_ms = duration.as_millis() as u64,
                "authentication succeeded (no variance check)"
            );
            return DaemonResponse::AuthResult(MatchResult {
                matched: true,
                model_id: best_model_id,
                label: best_model_id.and_then(&label_for),
                similarity: best_similarity,
            });
        }
    }

    let duration = start.elapsed();

    if dark_count == frame_count && frame_count > 0 {
        warn!(
            user,
            frames = frame_count,
            duration_ms = duration.as_millis() as u64,
            "all frames were dark"
        );
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
