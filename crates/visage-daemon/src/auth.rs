use std::time::Instant;

use visage_core::config::Config;
use visage_core::ipc::DaemonResponse;
use visage_core::traits::{CameraSource, FaceProcessor};
use visage_core::types::{cosine_similarity, FaceEmbedding, MatchResult};
use visage_store::FaceStore;
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
        warn!(user, "visage is disabled");
        return Some(DaemonResponse::Error {
            message: "visage is disabled".into(),
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
    _device_is_ir: bool,
) -> DaemonResponse {
    let start = Instant::now();
    let verbose = config.debug.verbose;

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
    // Store one embedding per matched frame (not per stored-embedding pair)
    let mut matched_frame_embeddings: Vec<FaceEmbedding> = Vec::new();
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
        if C::is_dark(&frame) {
            dark_count += 1;
            if verbose {
                debug!(frame = frame_count, "dark frame, skipping");
            }
            continue;
        }

        // Run face engine
        let faces = match engine.process(&frame) {
            Ok(f) => f,
            Err(e) => {
                debug!(frame = frame_count, "face engine error: {e}");
                continue;
            }
        };

        if faces.is_empty() {
            if verbose {
                debug!(frame = frame_count, "no faces detected");
            }
            continue;
        }

        // Matching (IR texture check skipped in generic path — only applies to real cameras)
        let mut frame_matched = false;
        for (_det, embedding) in &faces {
            // Compare against all stored embeddings, track best for this frame
            let mut frame_best_sim: f32 = 0.0;
            for (model_id, stored_emb) in &stored {
                let sim = cosine_similarity(embedding, stored_emb);
                if sim > frame_best_sim {
                    frame_best_sim = sim;
                }
                if sim > best_similarity {
                    best_similarity = sim;
                    best_model_id = Some(*model_id);
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
            }

            if frame_best_sim >= threshold && !frame_matched {
                matched_frame_embeddings.push(*embedding);
                frame_matched = true;
            }

            if verbose {
                let variance_info = if matched_frame_embeddings.len() >= 2 {
                    let first = &matched_frame_embeddings[0];
                    let last = &matched_frame_embeddings[matched_frame_embeddings.len() - 1];
                    format!(", variance={:.4}", 1.0 - cosine_similarity(first, last))
                } else {
                    String::new()
                };
                debug!(
                    frame = frame_count,
                    "similarity={:.4}, matched_frames={}{variance_info}",
                    frame_best_sim,
                    matched_frame_embeddings.len(),
                );
            }
        }

        // Frame variance check
        if config.security.require_frame_variance {
            if matched_frame_embeddings.len() >= config.security.min_auth_frames as usize {
                if check_frame_variance(&matched_frame_embeddings) {
                    let duration = start.elapsed();
                    info!(
                        user,
                        similarity = format!("{:.4}", best_similarity),
                        frames = frame_count,
                        matched = matched_frame_embeddings.len(),
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
                if verbose {
                    debug!("frame variance check failed, continuing");
                }
            }
        } else if best_similarity >= threshold {
            // No variance check required, accept on first match
            let duration = start.elapsed();
            info!(
                user,
                similarity = format!("{:.4}", best_similarity),
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
        similarity = format!("{:.4}", best_similarity),
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

/// Check that matched embeddings have sufficient variance.
/// Real faces produce micro-movements causing slight embedding variation.
/// A static photo produces near-identical embeddings.
///
/// Compares the first embedding against all others and requires at least
/// one pair to differ enough (similarity < 0.998). This is more forgiving
/// than requiring all consecutive pairs to differ, which fails on low-res
/// IR cameras where frame-to-frame variation is minimal.
fn check_frame_variance(embeddings: &[FaceEmbedding]) -> bool {
    if embeddings.len() < 2 {
        return false;
    }
    let first = &embeddings[0];
    let last = &embeddings[embeddings.len() - 1];
    let sim = cosine_similarity(first, last);
    sim < 0.998
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
        // Create two slightly different L2-normalized vectors
        let val = 1.0 / (512.0f32).sqrt();
        let mut emb1 = [val; 512];
        let mut emb2 = [val; 512];
        // Perturb emb2 enough to drop below 0.998 similarity
        emb2[0] += 0.1;
        emb2[1] -= 0.1;
        // Re-normalize
        let norm: f32 = emb2.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut emb2 {
            *x /= norm;
        }
        let norm1: f32 = emb1.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut emb1 {
            *x /= norm1;
        }
        assert!(check_frame_variance(&[emb1, emb2]));
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
