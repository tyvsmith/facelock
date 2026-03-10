//! One-shot authentication binary.
//!
//! Exit codes: 0 = matched, 1 = no match/timeout, 2 = error.

// These module declarations pull in the daemon's shared code.
// Only `auth` is actually used; the others exist so the mod tree compiles.
#[allow(dead_code)]
mod enroll;
#[allow(dead_code)]
mod handler;
#[allow(dead_code)]
mod rate_limit;
mod auth;

use std::path::Path;
use std::process::ExitCode;

use visage_camera::{Camera, auto_detect_device, is_ir_camera, validate_device};
use visage_core::config::Config;
use visage_core::ipc::DaemonResponse;
use visage_core::types::MatchResult;
use visage_face::FaceEngine;
use visage_store::FaceStore;
use tracing::{error, info};

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

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "visage_auth=info".into()),
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
                return ExitCode::from(2);
            }
        }
    }

    let device_path = config.device.path.clone().unwrap();
    let device_is_ir = validate_device(&device_path)
        .map(|dev| is_ir_camera(&dev))
        .unwrap_or(false);

    if config.security.require_ir && !device_is_ir {
        error!("IR camera required but device is not IR");
        return ExitCode::from(2);
    }

    let mut camera = match Camera::open(&config.device) {
        Ok(c) => c,
        Err(e) => {
            error!("camera: {e}");
            return ExitCode::from(2);
        }
    };

    let mut engine = match FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir)) {
        Ok(e) => e,
        Err(e) => {
            error!("models: {e}");
            return ExitCode::from(2);
        }
    };

    let store = match FaceStore::open(Path::new(&config.storage.db_path)) {
        Ok(s) => s,
        Err(e) => {
            error!("database: {e}");
            return ExitCode::from(2);
        }
    };

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
            ExitCode::from(0)
        }
        DaemonResponse::AuthResult(MatchResult { matched: false, similarity, .. }) => {
            info!(user = %user, similarity = format!("{similarity:.4}"), "no match");
            ExitCode::from(1)
        }
        DaemonResponse::Error { message } if message.contains("all frames dark") => {
            info!(user = %user, "all frames dark");
            ExitCode::from(1)
        }
        DaemonResponse::Error { message } => {
            error!(user = %user, "auth error: {message}");
            ExitCode::from(2)
        }
        _ => {
            error!("unexpected response");
            ExitCode::from(2)
        }
    }
}
