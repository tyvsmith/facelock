//! Direct (daemonless) implementations of CLI operations.
//!
//! Used when daemon socket is unavailable or `daemon.mode = oneshot`.
//! Opens camera, loads models, and accesses the database directly.

use std::path::Path;

use anyhow::{Context, bail};
use facelock_camera::{Camera, is_ir_camera, list_devices};
use facelock_core::config::Config;
use facelock_core::ipc::DaemonResponse;
use facelock_core::types::MatchResult;
use facelock_face::FaceEngine;
use facelock_store::FaceStore;

pub fn open_store(config: &Config) -> anyhow::Result<FaceStore> {
    FaceStore::open(Path::new(&config.storage.db_path)).context("failed to open database")
}

pub fn open_camera(config: &Config) -> anyhow::Result<Camera<'static>> {
    Camera::open(&config.device).context("failed to open camera")
}

pub fn load_engine(config: &Config) -> anyhow::Result<FaceEngine> {
    FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir))
        .context("failed to load face engine")
}

/// Direct authentication — returns true if matched.
pub fn authenticate(config: &Config, user: &str) -> anyhow::Result<bool> {
    let store = open_store(config)?;

    if !store.has_models(user).context("storage error")? {
        return Ok(false);
    }

    let mut camera = open_camera(config)?;
    let mut engine = load_engine(config)?;

    // Reuse the shared authenticate function from facelock-daemon's auth module.
    // Since it returns DaemonResponse (which is an IPC type), we map it here.
    // This avoids duplicating the auth loop.
    let response = facelock_daemon_auth::authenticate(
        &mut camera,
        &mut engine,
        &store,
        config,
        user,
    );

    match response {
        DaemonResponse::AuthResult(MatchResult { matched, .. }) => Ok(matched),
        DaemonResponse::Error { message } => bail!("{message}"),
        _ => bail!("unexpected auth response"),
    }
}

/// Direct enrollment — returns (model_id, embedding_count).
pub fn enroll(
    config: &Config,
    user: &str,
    label: &str,
) -> anyhow::Result<(u32, u32)> {
    let store = open_store(config)?;
    let mut camera = open_camera(config)?;
    let mut engine = load_engine(config)?;

    let response = facelock_daemon_enroll::enroll(
        &mut camera,
        &mut engine,
        &store,
        config,
        user,
        label,
    );

    match response {
        DaemonResponse::Enrolled { model_id, embedding_count } => Ok((model_id, embedding_count)),
        DaemonResponse::Error { message } => bail!("{message}"),
        _ => bail!("unexpected enroll response"),
    }
}

/// Direct device listing (no daemon needed).
pub fn list_devices_direct() -> anyhow::Result<()> {
    let devices = list_devices().context("failed to enumerate devices")?;

    if devices.is_empty() {
        println!("No video devices found.");
        return Ok(());
    }

    println!("Available video devices:\n");
    for dev in &devices {
        let ir_tag = if is_ir_camera(dev) { " [IR]" } else { "" };
        println!("  {}{ir_tag}", dev.path);
        println!("    Name:    {}", dev.name);
        println!("    Driver:  {}", dev.driver);

        if !dev.formats.is_empty() {
            println!("    Formats:");
            for fmt in &dev.formats {
                let sizes: Vec<String> = fmt
                    .sizes
                    .iter()
                    .map(|(w, h)| format!("{w}x{h}"))
                    .collect();
                println!(
                    "      {} ({}) — {}",
                    fmt.fourcc.trim(),
                    fmt.description,
                    if sizes.is_empty() {
                        "no sizes reported".to_string()
                    } else {
                        sizes.join(", ")
                    }
                );
            }
        }
        println!();
    }

    Ok(())
}

// Bridge modules — the daemon's auth and enroll functions are generic over traits,
// and Camera/FaceEngine implement those traits. We reference them via extern crate
// since the daemon is a separate binary crate. Instead, we inline the module paths.
//
// Actually, since facelock-daemon is a binary crate, we can't import from it.
// The auth and enroll modules use types from facelock-core's traits, and the concrete
// Camera/FaceEngine implement those traits. We need to either:
// 1. Move the shared auth/enroll logic to a library crate
// 2. Keep local implementations
//
// For now, we keep lightweight wrappers that call the same underlying logic.
// The auth loop is implemented in terms of core types (CameraSource + FaceProcessor).

mod facelock_daemon_auth {
    use facelock_core::config::Config;
    use facelock_core::ipc::DaemonResponse;
    use facelock_core::traits::{CameraSource, FaceProcessor};
    use facelock_core::types::{best_match, check_frame_variance, FaceEmbedding, MatchResult};
    use facelock_store::FaceStore;
    use tracing::{debug, info, warn};
    use std::time::Instant;

    pub fn authenticate<C: CameraSource, E: FaceProcessor>(
        camera: &mut C,
        engine: &mut E,
        store: &FaceStore,
        config: &Config,
        user: &str,
    ) -> DaemonResponse {
        let start = Instant::now();

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
                continue;
            }

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
            }

            if config.security.require_frame_variance {
                if matched_frame_embeddings.len() >= config.security.min_auth_frames as usize
                    && check_frame_variance(&matched_frame_embeddings)
                {
                    let duration = start.elapsed();
                    info!(user, similarity = format!("{best_similarity:.4}"), frames = frame_count, duration_ms = duration.as_millis() as u64, "authentication succeeded");
                    return DaemonResponse::AuthResult(MatchResult {
                        matched: true,
                        model_id: best_model_id,
                        label: best_model_id.and_then(&label_for),
                        similarity: best_similarity,
                    });
                }
            } else if best_similarity >= threshold {
                let duration = start.elapsed();
                info!(user, similarity = format!("{best_similarity:.4}"), frames = frame_count, duration_ms = duration.as_millis() as u64, "authentication succeeded");
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
            warn!(user, frames = frame_count, "all frames were dark");
            return DaemonResponse::Error { message: "all frames dark".into() };
        }

        info!(user, similarity = format!("{best_similarity:.4}"), frames = frame_count, duration_ms = duration.as_millis() as u64, "authentication failed");
        DaemonResponse::AuthResult(MatchResult {
            matched: false,
            model_id: None,
            label: None,
            similarity: best_similarity,
        })
    }
}

mod facelock_daemon_enroll {
    use facelock_core::config::Config;
    use facelock_core::ipc::DaemonResponse;
    use facelock_core::traits::{CameraSource, FaceProcessor};
    use facelock_store::FaceStore;
    use tracing::{debug, info, warn};
    use std::time::{Duration, Instant};

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
        match store.remove_model_by_label(user, label) {
            Ok(true) => info!(user, label, "removed existing model for re-enrollment"),
            Ok(false) => {}
            Err(e) => {
                return DaemonResponse::Error {
                    message: format!("storage error clearing old model: {e}"),
                };
            }
        }

        let enroll_secs = (config.recognition.timeout_secs as u64).max(5) * 3;
        let deadline = Instant::now() + Duration::from_secs(enroll_secs);
        let mut stored_count: u32 = 0;
        let mut model_id: Option<u32> = None;
        let mut last_capture = Instant::now() - INTER_FRAME_DELAY;

        while Instant::now() < deadline && (stored_count as usize) < MAX_CAPTURES {
            let since_last = Instant::now().duration_since(last_capture);
            if since_last < INTER_FRAME_DELAY {
                std::thread::sleep(INTER_FRAME_DELAY - since_last);
            }

            let frame = match camera.capture() {
                Ok(f) => f,
                Err(e) => { debug!("capture error: {e}"); continue; }
            };

            if C::is_dark(&frame) { continue; }

            let faces = match engine.process(&frame) {
                Ok(f) => f,
                Err(e) => { warn!("face engine error: {e}"); continue; }
            };

            if faces.is_empty() || faces.len() > 1 { continue; }

            let (_det, embedding) = &faces[0];
            match model_id {
                None => match store.add_model(user, label, embedding) {
                    Ok(id) => { model_id = Some(id); stored_count += 1; info!(model_id = id, "created model"); }
                    Err(e) => { return DaemonResponse::Error { message: format!("storage error: {e}") }; }
                },
                Some(id) => match store.add_embedding(id, embedding) {
                    Ok(()) => { stored_count += 1; }
                    Err(e) => { warn!("failed to store embedding: {e}"); }
                },
            }
            last_capture = Instant::now();
        }

        if stored_count < MIN_CAPTURES as u32 {
            return DaemonResponse::Error {
                message: format!("only captured {stored_count} frames, need at least {MIN_CAPTURES}"),
            };
        }

        info!(user, label, model_id = model_id.unwrap_or(0), embedding_count = stored_count, "enrollment complete");
        DaemonResponse::Enrolled { model_id: model_id.unwrap_or(0), embedding_count: stored_count }
    }
}
