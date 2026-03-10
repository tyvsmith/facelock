//! One-shot authentication binary.
//!
//! Loads config, opens camera, loads ONNX models, runs a single auth cycle,
//! and exits with a status code:
//!   0 = authenticated (face matched)
//!   1 = not authenticated (no match, timeout, all dark)
//!   2 = error (no camera, no models, no user models, config error)

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use visage_camera::{Camera, auto_detect_device, is_ir_camera, validate_device};
use visage_core::config::Config;
use visage_core::types::{cosine_similarity, FaceEmbedding};
use visage_face::FaceEngine;
use visage_store::FaceStore;
use tracing::{debug, error, info};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    let mut user: Option<String> = None;
    let mut config_path: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--user" if i + 1 < args.len() => {
                user = Some(args[i + 1].clone());
                i += 2;
            }
            "--config" if i + 1 < args.len() => {
                config_path = Some(args[i + 1].clone());
                i += 2;
            }
            _ => i += 1,
        }
    }

    let user = match user {
        Some(u) => u,
        None => {
            eprintln!("visage-auth: --user <username> required");
            return ExitCode::from(2);
        }
    };

    // Load config
    let config = match config_path {
        Some(ref p) => Config::load_from(Path::new(p)),
        None => Config::load(),
    };
    let mut config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("visage-auth: config error: {e}");
            return ExitCode::from(2);
        }
    };

    // Init tracing (stderr, brief)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "visage_auth=info".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    // Auto-detect device if needed
    if config.device.path.is_none() {
        match auto_detect_device() {
            Ok(dev) => {
                info!(device = %dev.path, name = %dev.name, "auto-detected camera");
                config.device.path = Some(dev.path);
            }
            Err(e) => {
                error!("no camera: {e}");
                return ExitCode::from(2);
            }
        }
    }

    let device_path = config.device.path.clone().unwrap();

    // Check IR status
    let device_is_ir = match validate_device(&device_path) {
        Ok(dev) => is_ir_camera(&dev),
        Err(_) => false,
    };

    if config.security.require_ir && !device_is_ir {
        error!("IR camera required but device is not IR");
        return ExitCode::from(2);
    }

    // Open camera
    let mut camera = match Camera::open(&config.device) {
        Ok(c) => c,
        Err(e) => {
            error!("camera: {e}");
            return ExitCode::from(2);
        }
    };

    // Load models
    let mut engine = match FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir)) {
        Ok(e) => e,
        Err(e) => {
            error!("models: {e}");
            return ExitCode::from(2);
        }
    };

    // Open database
    let store = match FaceStore::open(Path::new(&config.storage.db_path)) {
        Ok(s) => s,
        Err(e) => {
            error!("database: {e}");
            return ExitCode::from(2);
        }
    };

    // Check user has enrolled models
    match store.has_models(&user) {
        Ok(true) => {}
        Ok(false) => {
            info!(user = %user, "no enrolled models");
            return ExitCode::from(1);
        }
        Err(e) => {
            error!("storage: {e}");
            return ExitCode::from(2);
        }
    }

    // Run authentication inline (simplified version of auth::authenticate)
    let result = run_auth(&mut camera, &mut engine, &store, &config, &user);
    match result {
        AuthResult::Matched(sim) => {
            info!(user = %user, similarity = format!("{sim:.4}"), "authenticated");
            ExitCode::from(0)
        }
        AuthResult::NoMatch(sim) => {
            info!(user = %user, similarity = format!("{sim:.4}"), "no match");
            ExitCode::from(1)
        }
        AuthResult::AllDark => {
            info!(user = %user, "all frames dark");
            ExitCode::from(1)
        }
        AuthResult::Error(msg) => {
            error!(user = %user, "error: {msg}");
            ExitCode::from(2)
        }
    }
}

enum AuthResult {
    Matched(f32),
    NoMatch(f32),
    AllDark,
    Error(String),
}

fn run_auth(
    camera: &mut Camera<'_>,
    engine: &mut FaceEngine,
    store: &FaceStore,
    config: &Config,
    user: &str,
) -> AuthResult {
    let stored = match store.get_user_embeddings(user) {
        Ok(v) => v,
        Err(e) => return AuthResult::Error(format!("storage: {e}")),
    };
    if stored.is_empty() {
        return AuthResult::NoMatch(0.0);
    }

    let deadline =
        Instant::now() + std::time::Duration::from_secs(config.recognition.timeout_secs as u64);
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

        // Frame variance check
        if config.security.require_frame_variance {
            if matched_frame_embeddings.len() >= config.security.min_auth_frames as usize
                && check_frame_variance(&matched_frame_embeddings)
            {
                return AuthResult::Matched(best_similarity);
            }
        } else if best_similarity >= threshold {
            return AuthResult::Matched(best_similarity);
        }
    }

    if dark_count == frame_count && frame_count > 0 {
        return AuthResult::AllDark;
    }

    AuthResult::NoMatch(best_similarity)
}

fn check_frame_variance(embeddings: &[FaceEmbedding]) -> bool {
    if embeddings.len() < 2 {
        return false;
    }
    let first = &embeddings[0];
    let last = &embeddings[embeddings.len() - 1];
    let sim = cosine_similarity(first, last);
    sim < 0.998
}
