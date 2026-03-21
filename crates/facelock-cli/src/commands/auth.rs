//! One-shot authentication subcommand.
//!
//! Exit codes: 0 = matched, 1 = no match/timeout, 2 = error.

use std::path::Path;

use facelock_camera::quirks::QuirksDb;
use facelock_camera::{Camera, auto_detect_device, is_ir_camera_with_quirks, validate_device};
use facelock_core::config::Config;
use facelock_core::ipc::DaemonResponse;
use facelock_core::types::MatchResult;
use facelock_daemon::audit::{self, AuditEntry};
use facelock_daemon::auth;
use facelock_face::FaceEngine;
use facelock_store::FaceStore;
use tracing::{error, info};

pub fn run(user: String, config_path: Option<String>) -> i32 {
    let config = match config_path {
        Some(ref p) => Config::load_from(Path::new(p)),
        None => Config::load(),
    };
    let mut config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("facelock auth: config error: {e}");
            return 2;
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "facelock_cli=info".into()),
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

    // --- Pre-flight security checks (mirrors daemon pre_check) ---

    if config.security.disabled {
        error!("facelock is disabled");
        return 2;
    }

    if config.security.abort_if_ssh
        && (std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok())
    {
        error!("SSH session detected, aborting");
        return 2;
    }

    if config.security.abort_if_lid_closed {
        let lid_closed = std::fs::read_to_string("/proc/acpi/button/lid/LID0/state")
            .map(|s| s.contains("closed"))
            .unwrap_or(false);
        if lid_closed {
            error!("lid closed, aborting");
            return 2;
        }
    }

    // Open a writable store for rate limiting (the oneshot path runs as root
    // or the facelock group, so write access is available).
    let store = match FaceStore::open(Path::new(&config.storage.db_path)) {
        Ok(s) => s,
        Err(e) => {
            error!("database: {e}");
            return 2;
        }
    };

    // SQLite-based rate limiting: survives across oneshot process invocations.
    let rl = &config.security.rate_limit;
    match store.check_rate_limit(&user, rl.max_attempts, rl.window_secs) {
        Ok(true) => {}
        Ok(false) => {
            error!(user = %user, "rate limited");
            return 2;
        }
        Err(e) => {
            error!("rate limit check: {e}");
            return 2;
        }
    }

    // Record attempt *before* auth so the window tracks even failed attempts.
    if let Err(e) = store.record_auth_attempt(&user) {
        error!("rate limit record: {e}");
        return 2;
    }

    // Opportunistically clean up stale rate-limit rows.
    let _ = store.cleanup_rate_limit(rl.window_secs);

    match store.has_models(&user) {
        Ok(true) => {}
        Ok(false) => {
            info!(user = %user, "no enrolled models");
            return 2;
        }
        Err(e) => {
            error!("storage: {e}");
            return 2;
        }
    }

    // --- End pre-flight checks ---

    let device_path = config.device.path.clone().unwrap();
    let quirks = QuirksDb::load();
    let device_info = validate_device(&device_path);
    let device_is_ir = device_info
        .as_ref()
        .map(|dev| is_ir_camera_with_quirks(dev, Some(&quirks)))
        .unwrap_or(false);

    if config.security.require_ir && !device_is_ir {
        error!("IR camera required but device is not IR");
        return 2;
    }

    let device_quirk = device_info
        .ok()
        .and_then(|info| quirks.find_match(&info).cloned());

    let mut camera = match Camera::open(&config.device, device_quirk.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            error!("camera: {e}");
            return 2;
        }
    };

    // Discard warmup frames for AGC/AE stabilization.
    let warmup = device_quirk
        .and_then(|q| q.warmup_frames)
        .unwrap_or(config.device.warmup_frames);
    for _ in 0..warmup {
        let _ = camera.capture();
    }

    let mut engine =
        match FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir)) {
            Ok(e) => e,
            Err(e) => {
                error!("models: {e}");
                return 2;
            }
        };

    let start = std::time::Instant::now();
    let response = auth::authenticate(
        &mut camera,
        &mut engine,
        &store,
        &config,
        &user,
        device_is_ir,
    );
    let duration_ms = start.elapsed().as_millis() as u64;

    // Note: authenticate_inner already writes audit entries for the camera-based
    // auth loop. The oneshot path relies on those entries, so no additional audit
    // logging is needed here for the auth result itself.

    match response {
        DaemonResponse::AuthResult(MatchResult {
            matched: true,
            similarity,
            ..
        }) => {
            info!(user = %user, similarity = format!("{similarity:.4}"), "authenticated");
            0
        }
        DaemonResponse::AuthResult(MatchResult {
            matched: false,
            similarity,
            ..
        }) => {
            info!(user = %user, similarity = format!("{similarity:.4}"), "no match");
            1
        }
        DaemonResponse::Error { message } if message.contains("all frames dark") => {
            info!(user = %user, "all frames dark");
            1
        }
        DaemonResponse::Error { message } => {
            // Errors from authenticate() that aren't "all frames dark" are storage errors
            // which happen before the auth loop — audit those here.
            audit::write_audit_entry(
                &config.audit,
                &AuditEntry {
                    timestamp: audit::now_iso8601(),
                    user: user.clone(),
                    result: "error".into(),
                    similarity: None,
                    frame_count: None,
                    duration_ms: Some(duration_ms),
                    device: config.device.path.clone(),
                    model_label: None,
                    error: Some(message.clone()),
                },
            );
            error!(user = %user, "auth error: {message}");
            2
        }
        _ => {
            error!("unexpected response");
            2
        }
    }
}
