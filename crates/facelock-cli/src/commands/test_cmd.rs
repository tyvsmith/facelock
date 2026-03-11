use std::time::Instant;

use anyhow::Context;

use facelock_core::ipc::{DaemonRequest, DaemonResponse};
use facelock_core::Config;

use crate::ipc_client;
use crate::notifications::{notify_if_enabled, NotifyEvent};

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

    let request = DaemonRequest::Authenticate {
        user: user.clone(),
    };

    let start = Instant::now();
    let response = ipc_client::send_request(&config.daemon.socket_path, &request)?;
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
                        reason: format!(
                            "no match (best similarity: {:.2})",
                            result.similarity
                        ),
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
