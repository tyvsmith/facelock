use anyhow::Context;

use facelock_core::Config;
use facelock_core::ipc::{DaemonRequest, DaemonResponse};

use crate::ipc_client;

pub fn run(model_id: u32, user: Option<String>, yes: bool) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    let user = ipc_client::resolve_user(user.as_deref());

    if !yes {
        let confirmed =
            ipc_client::confirm(&format!("Remove face model #{model_id} for user '{user}'?"))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    if ipc_client::should_use_direct(&config) {
        ipc_client::require_root(&format!("sudo facelock remove {model_id}"))?;
        let store = crate::direct::open_store(&config)?;
        let removed = store
            .remove_model(&user, model_id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if removed {
            println!("Removed face model #{model_id} for user '{user}'.");
        } else {
            println!("Model #{model_id} not found for user '{user}'.");
        }
        return Ok(());
    }

    let request = DaemonRequest::RemoveModel {
        user: user.clone(),
        model_id,
    };

    let response = ipc_client::send_request(&request)?;

    match response {
        DaemonResponse::Removed => {
            println!("Removed face model #{model_id} for user '{user}'.");
        }
        other => {
            anyhow::bail!("unexpected response from daemon: {other:?}");
        }
    }

    Ok(())
}
