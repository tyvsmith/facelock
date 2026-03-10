//! Direct (daemonless) implementations of CLI operations.
//!
//! Used when `daemon.mode = "oneshot"`. Opens camera, loads models, and
//! accesses the database directly instead of going through IPC.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use visage_camera::{Camera, is_ir_camera, list_devices};
use visage_core::config::Config;
use visage_core::types::{FaceEmbedding, cosine_similarity};
use visage_face::FaceEngine;
use visage_store::FaceStore;
use tracing::{debug, info, warn};

/// Open the face store from config.
pub fn open_store(config: &Config) -> anyhow::Result<FaceStore> {
    FaceStore::open(Path::new(&config.storage.db_path)).context("failed to open database")
}

/// Open camera from config (with auto-detect).
pub fn open_camera(config: &Config) -> anyhow::Result<Camera<'static>> {
    Camera::open(&config.device).context("failed to open camera")
}

/// Load face engine from config.
pub fn load_engine(config: &Config) -> anyhow::Result<FaceEngine> {
    FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir))
        .context("failed to load face engine")
}

/// Direct authentication (same logic as visage-auth).
pub fn authenticate(config: &Config, user: &str) -> anyhow::Result<bool> {
    let store = open_store(config)?;

    if !store.has_models(user).context("storage error")? {
        println!("No face models enrolled for user '{user}'.");
        return Ok(false);
    }

    let stored = store.get_user_embeddings(user).context("storage error")?;
    let mut camera = open_camera(config)?;
    let mut engine = load_engine(config)?;

    let deadline =
        Instant::now() + Duration::from_secs(config.recognition.timeout_secs as u64);
    let threshold = config.recognition.threshold;
    let mut best_similarity: f32 = 0.0;
    let mut matched_frame_embeddings: Vec<FaceEmbedding> = Vec::new();
    let mut dark_count: u32 = 0;
    let mut frame_count: u32 = 0;

    while Instant::now() < deadline {
        let frame = match camera.capture() {
            Ok(f) => f,
            Err(e) => {
                debug!("capture error: {e}");
                continue;
            }
        };
        frame_count += 1;

        if Camera::is_dark(&frame) {
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
            let mut frame_best_sim: f32 = 0.0;
            for (_model_id, stored_emb) in &stored {
                let sim = cosine_similarity(embedding, stored_emb);
                if sim > frame_best_sim {
                    frame_best_sim = sim;
                }
                if sim > best_similarity {
                    best_similarity = sim;
                }
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
                return Ok(true);
            }
        } else if best_similarity >= threshold {
            return Ok(true);
        }
    }

    if dark_count == frame_count && frame_count > 0 {
        bail!("all frames were dark — is the camera covered?");
    }

    Ok(false)
}

/// Direct enrollment.
pub fn enroll(
    config: &Config,
    user: &str,
    label: &str,
) -> anyhow::Result<(u32, u32)> {
    let store = open_store(config)?;

    // Remove existing model with same label (re-enrollment)
    match store.remove_model_by_label(user, label) {
        Ok(true) => info!("removed existing model '{label}' for re-enrollment"),
        Ok(false) => {}
        Err(e) => bail!("storage error clearing old model: {e}"),
    }

    let mut camera = open_camera(config)?;
    let mut engine = load_engine(config)?;

    let enroll_secs = (config.recognition.timeout_secs as u64).max(5) * 3;
    let deadline = Instant::now() + Duration::from_secs(enroll_secs);
    let inter_frame_delay = Duration::from_millis(200);
    let min_captures: u32 = 3;
    let max_captures: u32 = 10;

    let mut stored_count: u32 = 0;
    let mut model_id: Option<u32> = None;
    let mut last_capture = Instant::now() - inter_frame_delay;

    while Instant::now() < deadline && stored_count < max_captures {
        let since_last = Instant::now().duration_since(last_capture);
        if since_last < inter_frame_delay {
            std::thread::sleep(inter_frame_delay - since_last);
        }

        let frame = match camera.capture() {
            Ok(f) => f,
            Err(e) => {
                debug!("capture error: {e}");
                continue;
            }
        };

        if Camera::is_dark(&frame) {
            warn!("skipping dark frame");
            continue;
        }

        let faces = match engine.process(&frame) {
            Ok(f) => f,
            Err(e) => {
                warn!("face engine error: {e}");
                continue;
            }
        };

        if faces.is_empty() {
            continue;
        }
        if faces.len() > 1 {
            warn!("multiple faces detected, skipping frame");
            continue;
        }

        let (_det, embedding) = &faces[0];

        match model_id {
            None => {
                let id = store.add_model(user, label, embedding)
                    .context("failed to create model")?;
                model_id = Some(id);
                stored_count += 1;
                info!(model_id = id, "created model with first embedding");
            }
            Some(id) => {
                if let Err(e) = store.add_embedding(id, embedding) {
                    warn!("failed to store embedding: {e}");
                } else {
                    stored_count += 1;
                }
            }
        }

        last_capture = Instant::now();
    }

    if stored_count < min_captures {
        bail!(
            "only captured {stored_count} frames, need at least {min_captures}"
        );
    }

    Ok((model_id.unwrap_or(0), stored_count))
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

fn check_frame_variance(embeddings: &[FaceEmbedding]) -> bool {
    if embeddings.len() < 2 {
        return false;
    }
    let first = &embeddings[0];
    let last = &embeddings[embeddings.len() - 1];
    cosine_similarity(first, last) < 0.998
}
