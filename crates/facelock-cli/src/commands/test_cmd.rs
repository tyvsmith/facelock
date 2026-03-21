use std::time::Instant;

use anyhow::Context;

use facelock_core::Config;
use facelock_core::ipc::{DaemonRequest, DaemonResponse};

use crate::ipc_client;
use crate::notifications::{NotifyEvent, notify_if_enabled};

pub fn run(user: Option<String>) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;

    // Check models exist — offer to run setup if missing
    let model_dir = std::path::Path::new(&config.daemon.model_dir);
    let detector = model_dir.join(&config.recognition.detector_model);
    let embedder = model_dir.join(&config.recognition.embedder_model);
    if !detector.exists() || !embedder.exists() {
        crate::ipc_client::require_root("sudo facelock setup")?;
        println!("Face recognition models not found.");
        if crate::ipc_client::confirm("Download models now?")? {
            crate::commands::setup::run(false)?;
            if !detector.exists() || !embedder.exists() {
                anyhow::bail!("Models still not found after setup.");
            }
        } else {
            anyhow::bail!("Models required. Run `facelock setup` to download them.");
        }
    }

    let user = ipc_client::resolve_user(user.as_deref());
    let notif_config = &config.notification;

    // Check if user has enrolled models before attempting auth
    let has_models = if ipc_client::should_use_direct(&config) {
        crate::direct::open_store(&config)
            .ok()
            .and_then(|s| s.has_models(&user).ok())
            .unwrap_or(false)
    } else {
        let request = DaemonRequest::ListModels { user: user.clone() };
        matches!(
            ipc_client::send_request(&request),
            Ok(DaemonResponse::Models(ref m)) if !m.is_empty()
        )
    };
    if !has_models {
        println!("No face models enrolled for user '{user}'.");
        println!("Run 'facelock enroll' to enroll a face first.");
        return Ok(());
    }

    // Warn if no enrolled models match the current embedder
    {
        let config_embedder = &config.recognition.embedder_model;
        let has_matching = if ipc_client::should_use_direct(&config) {
            crate::direct::open_store(&config)
                .ok()
                .and_then(|s| s.has_models_for_embedder(&user, config_embedder).ok())
                .unwrap_or(false)
        } else {
            let request = DaemonRequest::ListModels { user: user.clone() };
            match ipc_client::send_request(&request) {
                Ok(DaemonResponse::Models(ref m)) => m
                    .iter()
                    .any(|model| model.embedder_model == *config_embedder),
                _ => true, // can't check, proceed anyway
            }
        };
        if !has_matching {
            println!(
                "Warning: no enrolled models use the configured embedder '{config_embedder}'."
            );
            println!("Re-enroll with 'facelock enroll' to use the current model.");
            return Ok(());
        }
    }

    println!("Testing face recognition for user '{user}'...");
    println!("Look at the camera.");

    notify_if_enabled(notif_config, &NotifyEvent::Scanning);

    if ipc_client::should_use_direct(&config) {
        ipc_client::require_root("sudo facelock test")?;
        let start = Instant::now();
        match crate::direct::authenticate(&config, &user) {
            Ok(true) => {
                let elapsed = start.elapsed();
                println!("Matched in {:.2}s", elapsed.as_secs_f64());
                notify_if_enabled(
                    notif_config,
                    &NotifyEvent::Success {
                        label: None,
                        similarity: 0.0,
                    },
                );
            }
            Ok(false) => {
                let elapsed = start.elapsed();
                println!("No match after {:.1}s", elapsed.as_secs_f64());
                notify_if_enabled(
                    notif_config,
                    &NotifyEvent::Failure {
                        reason: "no match".to_string(),
                    },
                );
            }
            Err(e) => {
                notify_if_enabled(
                    notif_config,
                    &NotifyEvent::Failure {
                        reason: e.to_string(),
                    },
                );
                return Err(e);
            }
        }
        return Ok(());
    }

    let request = DaemonRequest::Authenticate { user: user.clone() };

    let start = Instant::now();
    let response = ipc_client::send_request(&request)?;
    let elapsed = start.elapsed();

    match response {
        DaemonResponse::AuthResult(result) => {
            if result.matched {
                let model_id = result.model_id.unwrap_or(0);
                let label = result.label.as_deref().unwrap_or("unknown");
                println!(
                    "Matched model #{model_id} '{label}' (similarity: {:.2}) in {:.2}s",
                    result.similarity,
                    elapsed.as_secs_f64()
                );
                notify_if_enabled(
                    notif_config,
                    &NotifyEvent::Success {
                        label: result.label.clone(),
                        similarity: result.similarity,
                    },
                );
            } else {
                println!(
                    "No match (best: {:.2}) after {:.1}s",
                    result.similarity,
                    elapsed.as_secs_f64()
                );
                notify_if_enabled(
                    notif_config,
                    &NotifyEvent::Failure {
                        reason: format!("no match (best similarity: {:.2})", result.similarity),
                    },
                );
            }
        }
        other => {
            notify_if_enabled(
                notif_config,
                &NotifyEvent::Failure {
                    reason: "unexpected daemon response".to_string(),
                },
            );
            anyhow::bail!("unexpected response from daemon: {other:?}");
        }
    }

    Ok(())
}
