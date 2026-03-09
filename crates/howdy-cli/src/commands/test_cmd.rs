use std::time::Instant;

use anyhow::Context;

use howdy_core::ipc::{DaemonRequest, DaemonResponse};
use howdy_core::Config;

use crate::ipc_client;

pub fn run(user: Option<String>) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    let user = ipc_client::resolve_user(user.as_deref());

    println!("Testing face recognition for user '{user}'...");
    println!("Look at the camera.");

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
            } else {
                println!(
                    "No match (best: {:.2}) after {:.1}s",
                    result.similarity,
                    elapsed.as_secs_f64()
                );
            }
        }
        other => {
            anyhow::bail!("unexpected response from daemon: {other:?}");
        }
    }

    Ok(())
}
