use anyhow::Context;
use chrono::Local;

use facelock_core::Config;
use facelock_core::ipc::{DaemonRequest, DaemonResponse};

use crate::ipc_client;

pub fn run(
    user: Option<String>,
    label: Option<String>,
    skip_setup_check: bool,
) -> anyhow::Result<()> {
    // Setup gate: prompt user if setup hasn't been run.
    // Setup includes model downloads, encryption, and face enrollment,
    // so if setup runs successfully we're done — no need to enroll again.
    if !skip_setup_check {
        let marker = std::path::Path::new(super::setup::SETUP_COMPLETE_MARKER);
        if !marker.exists() {
            ipc_client::require_root("sudo facelock setup")?;
            println!("Setup has not been completed.");
            if ipc_client::confirm("Run setup now?")? {
                super::setup::run(false)?;
                if !marker.exists() {
                    anyhow::bail!("Setup did not complete successfully.");
                }
                // Setup includes face enrollment (Step 4), so we're done
                return Ok(());
            } else {
                println!("Run 'sudo facelock setup' when ready.");
                return Ok(());
            }
        }
    }

    ipc_client::require_root("sudo facelock enroll")?;

    let config = Config::load().context("failed to load config")?;

    // Check models exist
    let model_dir = std::path::Path::new(&config.daemon.model_dir);
    let detector = model_dir.join(&config.recognition.detector_model);
    let embedder = model_dir.join(&config.recognition.embedder_model);
    if !detector.exists() || !embedder.exists() {
        anyhow::bail!(
            "Face recognition models not found in {}.\nRun `sudo facelock setup` to download them.",
            config.daemon.model_dir
        );
    }

    let user = ipc_client::resolve_user(user.as_deref());

    let label = label.unwrap_or_else(|| {
        let date = Local::now().format("%Y-%m-%d").to_string();
        next_label(&date, &user)
    });

    // Warn if existing models use a different embedder than currently configured
    {
        let config_embedder = &config.recognition.embedder_model;
        let has_stale = if ipc_client::should_use_direct(&config) {
            crate::direct::open_store(&config).ok().map(|s| {
                let has_any = s.has_models(&user).ok().unwrap_or(false);
                let has_matching = s
                    .has_models_for_embedder(&user, config_embedder)
                    .ok()
                    .unwrap_or(false);
                has_any && !has_matching
            })
        } else {
            let request = DaemonRequest::ListModels { user: user.clone() };
            match ipc_client::send_request(&request) {
                Ok(DaemonResponse::Models(ref m)) if !m.is_empty() => Some(
                    !m.iter()
                        .any(|model| model.embedder_model == *config_embedder),
                ),
                _ => None,
            }
        };
        if has_stale == Some(true) {
            println!(
                "Note: existing models don't use the configured embedder '{config_embedder}'."
            );
            println!(
                "Old enrollments will not work with the new embedder. Consider removing them with 'facelock remove'.\n"
            );
        }
    }

    println!("Enrolling face for user '{user}' with label '{label}'...");
    println!("Look at the camera. Slowly turn your head left and right.");

    if ipc_client::should_use_direct(&config) {
        ipc_client::require_root("sudo facelock enroll")?;
        let (model_id, embedding_count) = crate::direct::enroll(&config, &user, &label)?;
        println!(
            "\nFace enrolled successfully!\n  Model ID: {model_id}\n  Embeddings: {embedding_count}\n  Label: {label}"
        );
        return Ok(());
    }

    let request = DaemonRequest::Enroll {
        user: user.clone(),
        label: label.clone(),
    };

    let response = ipc_client::send_request(&request)?;

    match response {
        DaemonResponse::Enrolled {
            model_id,
            embedding_count,
        } => {
            println!(
                "\nFace enrolled successfully!\n  Model ID: {model_id}\n  Embeddings: {embedding_count}\n  Label: {label}"
            );
            check_model_count(&user)?;
        }
        other => {
            anyhow::bail!("unexpected response from daemon: {other:?}");
        }
    }

    Ok(())
}

/// Generate the next available label like "2026-03-15-1", "2026-03-15-2", etc.
fn next_label(date_prefix: &str, user: &str) -> String {
    let existing = ipc_client::send_request(&DaemonRequest::ListModels {
        user: user.to_string(),
    });

    let max_suffix = match existing {
        Ok(DaemonResponse::Models(models)) => models
            .iter()
            .filter_map(|m| {
                m.label
                    .strip_prefix(date_prefix)
                    .and_then(|rest| rest.strip_prefix('-'))
                    .and_then(|n| n.parse::<u32>().ok())
            })
            .max()
            .unwrap_or(0),
        _ => 0,
    };

    format!("{date_prefix}-{}", max_suffix + 1)
}

fn check_model_count(user: &str) -> anyhow::Result<()> {
    let request = DaemonRequest::ListModels {
        user: user.to_string(),
    };

    if let Ok(DaemonResponse::Models(models)) = ipc_client::send_request(&request) {
        if models.len() > 5 {
            println!(
                "\nWarning: user '{user}' has {} face models. Consider removing old ones with 'facelock remove'.",
                models.len()
            );
        }
    }

    Ok(())
}
