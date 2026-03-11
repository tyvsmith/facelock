use anyhow::Context;

use facelock_core::ipc::{DaemonRequest, DaemonResponse};
use facelock_core::Config;

use crate::ipc_client;

pub fn run(user: Option<String>, yes: bool) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    let user = ipc_client::resolve_user(user.as_deref());

    if !yes {
        let confirmed =
            ipc_client::confirm(&format!("Remove ALL face models for user '{user}'?"))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    if ipc_client::should_use_direct(&config) {
        ipc_client::require_root("sudo facelock clear")?;
        let store = crate::direct::open_store(&config)?;
        let count = store.clear_user(&user).map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("Removed {count} face model(s) for user '{user}'.");
        return Ok(());
    }

    let request = DaemonRequest::ClearModels {
        user: user.clone(),
    };

    let response = ipc_client::send_request(&config.daemon.socket_path, &request)?;

    match response {
        DaemonResponse::Removed => {
            println!("All face models removed for user '{user}'.");
        }
        other => {
            anyhow::bail!("unexpected response from daemon: {other:?}");
        }
    }

    Ok(())
}
