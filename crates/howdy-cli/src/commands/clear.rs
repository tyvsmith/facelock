use anyhow::Context;

use howdy_core::ipc::{DaemonRequest, DaemonResponse};
use howdy_core::Config;

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
