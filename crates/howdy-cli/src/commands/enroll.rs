use anyhow::Context;
use chrono::Local;

use howdy_core::ipc::{DaemonRequest, DaemonResponse};
use howdy_core::Config;

use crate::ipc_client;

pub fn run(user: Option<String>, label: Option<String>) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    let user = ipc_client::resolve_user(user.as_deref());

    // Generate default label: YYYY-MM-DD-N
    let label = label.unwrap_or_else(|| {
        let date = Local::now().format("%Y-%m-%d").to_string();
        format!("{date}-1")
    });

    println!("Enrolling face for user '{user}' with label '{label}'...");
    println!("Look at the camera. Slowly turn your head left and right.");

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

            // Check if user has many models and warn
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
                "\nWarning: user '{user}' has {} face models. Consider removing old ones with 'howdy remove'.",
                models.len()
            );
        }
    }

    Ok(())
}
