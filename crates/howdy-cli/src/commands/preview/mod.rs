mod font;
mod render;
mod text_only;
#[cfg(feature = "wayland")]
mod wayland_preview;

use anyhow::Context;
use howdy_core::Config;

pub fn run(text_only: bool) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    let socket_path = config.daemon.socket_path.clone();

    if text_only {
        return text_only::run(&socket_path);
    }

    run_graphical(&socket_path)
}

#[cfg(feature = "wayland")]
fn run_graphical(socket_path: &str) -> anyhow::Result<()> {
    match wayland_preview::run(socket_path) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("Wayland preview failed: {e}");
            eprintln!(
                "Wayland preview unavailable: {e}\n\
                 Falling back to text-only mode.\n"
            );
            text_only::run(socket_path)
        }
    }
}

#[cfg(not(feature = "wayland"))]
fn run_graphical(socket_path: &str) -> anyhow::Result<()> {
    eprintln!(
        "Graphical preview not available (compiled without wayland feature).\n\
         Using text-only mode.\n"
    );
    text_only::run(socket_path)
}
