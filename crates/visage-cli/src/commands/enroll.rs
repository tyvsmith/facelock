use anyhow::Context;
use chrono::Local;

use visage_core::ipc::{DaemonRequest, DaemonResponse};
use visage_core::Config;

use crate::ipc_client;

pub fn run(user: Option<String>, label: Option<String>) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;

    // Check models exist before doing anything else
    let model_dir = std::path::Path::new(&config.daemon.model_dir);
    let detector = model_dir.join(&config.recognition.detector_model);
    let embedder = model_dir.join(&config.recognition.embedder_model);
    if !detector.exists() || !embedder.exists() {
        anyhow::bail!(
            "Face recognition models not found in {}.\n\
             Run `sudo visage setup` to download them.",
            config.daemon.model_dir
        );
    }

    let user = ipc_client::resolve_user(user.as_deref());

    let label = label.unwrap_or_else(|| {
        let date = Local::now().format("%Y-%m-%d").to_string();
        format!("{date}-1")
    });

    println!("Enrolling face for user '{user}' with label '{label}'...");
    println!("Look at the camera. Slowly turn your head left and right.");

    if ipc_client::should_use_direct(&config) {
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

    let response = ipc_client::send_request(&config.daemon.socket_path, &request)?;

    match response {
        DaemonResponse::Enrolled {
            model_id,
            embedding_count,
        } => {
            println!(
                "\nFace enrolled successfully!\n  Model ID: {model_id}\n  Embeddings: {embedding_count}\n  Label: {label}"
            );
            check_model_count(&config, &user)?;
        }
        other => {
            anyhow::bail!("unexpected response from daemon: {other:?}");
        }
    }

    Ok(())
}

fn check_model_count(config: &Config, user: &str) -> anyhow::Result<()> {
    let request = DaemonRequest::ListModels {
        user: user.to_string(),
    };

    if let Ok(DaemonResponse::Models(models)) =
        ipc_client::send_request(&config.daemon.socket_path, &request)
    {
        if models.len() > 5 {
            println!(
                "\nWarning: user '{user}' has {} face models. Consider removing old ones with 'visage remove'.",
                models.len()
            );
        }
    }

    Ok(())
}
