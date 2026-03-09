use std::time::Instant;

use howdy_camera::{Camera, check_ir_texture};
use howdy_core::config::Config;
use howdy_core::ipc::DaemonResponse;
use howdy_core::types::{cosine_similarity, FaceEmbedding, MatchResult};
use howdy_face::FaceEngine;
use howdy_store::FaceStore;
use tracing::{debug, info, warn};

use crate::rate_limit::RateLimiter;

pub fn authenticate(
    camera: &mut Camera<'_>,
    engine: &mut FaceEngine,
    store: &FaceStore,
    config: &Config,
    user: &str,
    rate_limiter: &mut RateLimiter,
    device_is_ir: bool,
) -> DaemonResponse {
    let start = Instant::now();

    // Pre-condition checks
    if config.security.disabled {
        warn!(user, "howdy is disabled");
        return DaemonResponse::Error {
            message: "howdy is disabled".into(),
        };
    }

    if config.security.abort_if_ssh && is_ssh_session() {
        info!(user, "SSH session detected, aborting");
        return DaemonResponse::Error {
            message: "SSH session detected".into(),
        };
    }

    if config.security.abort_if_lid_closed && is_lid_closed() {
        info!(user, "lid closed, aborting");
        return DaemonResponse::Error {
            message: "lid closed".into(),
        };
    }

    // Check if user has any enrolled models
    let has_models = match store.has_models(user) {
        Ok(v) => v,
        Err(e) => {
            return DaemonResponse::Error {
                message: format!("storage error: {e}"),
            };
        }
    };
    if !has_models {
        return DaemonResponse::AuthResult(MatchResult {
            matched: false,
            model_id: None,
            label: None,
            similarity: 0.0,
        });
    }

    // Rate limit check
    if !rate_limiter.check_and_record(user) {
        warn!(user, "rate limited");
        return DaemonResponse::Error {
            message: "rate limited".into(),
        };
    }

    // IR camera enforcement
    if config.security.require_ir && !device_is_ir {
        warn!(user, "IR camera required but device is not IR");
        return DaemonResponse::Error {
            message: "IR camera required for authentication. Set security.require_ir = false to override (NOT RECOMMENDED).".into(),
        };
    }

    // Load user embeddings
    let stored = match store.get_user_embeddings(user) {
        Ok(v) => v,
        Err(e) => {
            return DaemonResponse::Error {
                message: format!("storage error: {e}"),
            };
        }
    };

    let deadline =
        Instant::now() + std::time::Duration::from_secs(config.recognition.timeout_secs as u64);
    let threshold = config.recognition.threshold;
    let mut best_similarity: f32 = 0.0;
    let mut matched_embeddings: Vec<FaceEmbedding> = Vec::new();
    let mut dark_count: u32 = 0;
    let mut frame_count: u32 = 0;
    let mut best_model_id: Option<u32> = None;
    let mut best_label: Option<String> = None;

    while Instant::now() < deadline {
        // Capture frame
        let frame = match camera.capture() {
            Ok(f) => f,
            Err(e) => {
                debug!("capture error: {e}");
                continue;
            }
        };
        frame_count += 1;

        // Skip dark frames
        if Camera::is_dark(&frame) {
            dark_count += 1;
            continue;
        }

        // Run face engine
        let faces = match engine.process(&frame) {
            Ok(f) => f,
            Err(e) => {
                debug!("face engine error: {e}");
                continue;
            }
        };

        if faces.is_empty() {
            continue;
        }

        // IR texture check
        for (det, embedding) in &faces {
            if device_is_ir && !check_ir_texture(&frame.gray, &det.bbox, frame.width) {
                debug!("IR texture check failed, possible spoof");
                continue;
            }

            // Compare against all stored embeddings
            for (model_id, stored_emb) in &stored {
                let sim = cosine_similarity(embedding, stored_emb);
                if sim > best_similarity {
                    best_similarity = sim;
                    best_model_id = Some(*model_id);
                    // Look up label from the model list
                    best_label = store
                        .list_models(user)
                        .ok()
                        .and_then(|models| {
                            models
                                .into_iter()
                                .find(|m| m.id == *model_id)
                                .map(|m| m.label)
                        });
                }
                if sim >= threshold {
                    matched_embeddings.push(*embedding);
                }
            }
        }

        // Frame variance check
        if config.security.require_frame_variance {
            if matched_embeddings.len() >= config.security.min_auth_frames as usize {
                if check_frame_variance(&matched_embeddings) {
                    let duration = start.elapsed();
                    info!(
                        user,
                        matched = true,
                        similarity = best_similarity,
                        frames = frame_count,
                        duration_ms = duration.as_millis() as u64,
                        "authentication succeeded"
                    );
                    return DaemonResponse::AuthResult(MatchResult {
                        matched: true,
                        model_id: best_model_id,
                        label: best_label,
                        similarity: best_similarity,
                    });
                }
                // Frames too similar (possible static image), keep trying
                debug!("frame variance check failed, continuing");
            }
        } else if best_similarity >= threshold {
            // No variance check required, accept on first match
            let duration = start.elapsed();
            info!(
                user,
                matched = true,
                similarity = best_similarity,
                frames = frame_count,
                duration_ms = duration.as_millis() as u64,
                "authentication succeeded (no variance check)"
            );
            return DaemonResponse::AuthResult(MatchResult {
                matched: true,
                model_id: best_model_id,
                label: best_label,
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
        matched = false,
        similarity = best_similarity,
        frames = frame_count,
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

/// Check that consecutive matched embeddings have sufficient variance.
/// Real faces produce micro-movements causing slight embedding variation.
/// A static photo produces near-identical embeddings (similarity > 0.995).
fn check_frame_variance(embeddings: &[FaceEmbedding]) -> bool {
    if embeddings.len() < 2 {
        return false;
    }
    for window in embeddings.windows(2) {
        let sim = cosine_similarity(&window[0], &window[1]);
        if sim >= 0.995 {
            return false;
        }
    }
    true
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
        // Save and clear
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

        // Restore
        if let Some(v) = old_conn {
            unsafe { std::env::set_var("SSH_CONNECTION", v) };
        }
        if let Some(v) = old_tty {
            unsafe { std::env::set_var("SSH_TTY", v) };
        }
    }

    #[test]
    fn frame_variance_identical_embeddings_rejected() {
        let emb = [0.5f32; 512];
        let embeddings = vec![emb, emb, emb];
        assert!(!check_frame_variance(&embeddings));
    }

    #[test]
    fn frame_variance_different_embeddings_accepted() {
        // Create distinct L2-normalized vectors by putting weight in different regions
        let mut emb1 = [0.0f32; 512];
        let mut emb2 = [0.0f32; 512];
        let mut emb3 = [0.0f32; 512];
        // emb1: weight in first third
        for i in 0..170 {
            emb1[i] = 1.0 / (170.0f32).sqrt();
        }
        // emb2: weight in second third
        for i in 170..340 {
            emb2[i] = 1.0 / (170.0f32).sqrt();
        }
        // emb3: weight in last third
        for i in 340..510 {
            emb3[i] = 1.0 / (170.0f32).sqrt();
        }
        assert!(check_frame_variance(&[emb1, emb2, emb3]));
    }

    #[test]
    fn frame_variance_needs_at_least_two() {
        let emb = [0.5f32; 512];
        assert!(!check_frame_variance(&[emb]));
        assert!(!check_frame_variance(&[]));
    }

    #[test]
    fn lid_closed_returns_false_on_missing_file() {
        // /proc/acpi/button/lid/LID0/state likely doesn't exist in CI
        // so this should return false (not panic)
        let _result = is_lid_closed();
    }
}
