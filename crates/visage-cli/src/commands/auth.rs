//! One-shot authentication subcommand.
//!
//! Exit codes: 0 = matched, 1 = no match/timeout, 2 = error.

use std::path::Path;

use visage_camera::{Camera, auto_detect_device, is_ir_camera, validate_device};
use visage_core::config::Config;
use visage_core::ipc::DaemonResponse;
use visage_core::types::MatchResult;
use visage_daemon::auth;
use visage_face::FaceEngine;
use visage_store::FaceStore;
use tracing::{error, info};

pub fn run(user: String, config_path: Option<String>) -> i32 {
    let config = match config_path {
        Some(ref p) => Config::load_from(Path::new(p)),
        None => Config::load(),
    };
    let mut config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("visage auth: config error: {e}");
            return 2;
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "visage_cli=info".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    if config.device.path.is_none() {
        match auto_detect_device() {
            Ok(dev) => {
                info!(device = %dev.path, name = %dev.name, "auto-detected camera");
                config.device.path = Some(dev.path);
            }
            Err(e) => {
                error!("no camera: {e}");
                return 2;
            }
        }
    }

    let device_path = config.device.path.clone().unwrap();
    let device_is_ir = validate_device(&device_path)
        .map(|dev| is_ir_camera(&dev))
        .unwrap_or(false);

    if config.security.require_ir && !device_is_ir {
        error!("IR camera required but device is not IR");
        return 2;
    }

    let mut camera = match Camera::open(&config.device) {
        Ok(c) => c,
        Err(e) => {
            error!("camera: {e}");
            return 2;
        }
    };

    let mut engine = match FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir)) {
        Ok(e) => e,
        Err(e) => {
            error!("models: {e}");
            return 2;
        }
    };

    let store = match FaceStore::open(Path::new(&config.storage.db_path)) {
        Ok(s) => s,
        Err(e) => {
            error!("database: {e}");
            return 2;
        }
    };

    match store.has_models(&user) {
        Ok(true) => {}
        Ok(false) => {
            info!(user = %user, "no enrolled models");
            return 1;
        }
        Err(e) => {
            error!("storage: {e}");
            return 2;
        }
    }

    let response = auth::authenticate(
        &mut camera,
        &mut engine,
        &store,
        &config,
        &user,
    );

    match response {
        DaemonResponse::AuthResult(MatchResult { matched: true, similarity, .. }) => {
            info!(user = %user, similarity = format!("{similarity:.4}"), "authenticated");
            0
        }
        DaemonResponse::AuthResult(MatchResult { matched: false, similarity, .. }) => {
            info!(user = %user, similarity = format!("{similarity:.4}"), "no match");
            1
        }
        DaemonResponse::Error { message } if message.contains("all frames dark") => {
            info!(user = %user, "all frames dark");
            1
        }
        DaemonResponse::Error { message } => {
            error!(user = %user, "auth error: {message}");
            2
        }
        _ => {
            error!("unexpected response");
            2
        }
    }
}
