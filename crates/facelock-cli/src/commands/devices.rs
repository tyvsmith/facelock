use anyhow::Context;
use facelock_core::ipc::{DaemonRequest, DaemonResponse};
use facelock_core::Config;

use crate::ipc_client;

pub fn run() -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;

    if ipc_client::should_use_direct(&config) {
        return crate::direct::list_devices_direct();
    }

    let response = ipc_client::send_request(&config.daemon.socket_path, &DaemonRequest::ListDevices)
        .context("failed to query daemon — is facelock-daemon running?")?;

    match response {
        DaemonResponse::Devices(devices) => {
            if devices.is_empty() {
                println!("No video devices found.");
                println!("Check that your camera is connected and the v4l2 module is loaded.");
                return Ok(());
            }

            println!("Available video devices:\n");
            for dev in &devices {
                let ir_tag = if dev.is_ir { " [IR]" } else { "" };
                println!("  {}{ir_tag}", dev.path);
                println!("    Name:    {}", dev.name);
                println!("    Driver:  {}", dev.driver);

                if !dev.formats.is_empty() {
                    println!("    Formats:");
                    for fmt in &dev.formats {
                        let sizes: Vec<String> = fmt
                            .sizes
                            .iter()
                            .map(|(w, h)| format!("{w}x{h}"))
                            .collect();
                        println!(
                            "      {} ({}) — {}",
                            fmt.fourcc.trim(),
                            fmt.description,
                            if sizes.is_empty() {
                                "no sizes reported".to_string()
                            } else {
                                sizes.join(", ")
                            }
                        );
                    }
                }
                println!();
            }
        }
        DaemonResponse::Error { message } => {
            anyhow::bail!("daemon error: {message}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    // Device listing requires hardware or a running daemon.
}
