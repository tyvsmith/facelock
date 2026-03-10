use anyhow::Context;

use visage_core::ipc::{DaemonRequest, DaemonResponse};
use visage_core::Config;

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

    let request = DaemonRequest::RemoveModel {
        user: user.clone(),
        model_id,
    };

    let response = ipc_client::send_request(&config.daemon.socket_path, &request)?;

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
