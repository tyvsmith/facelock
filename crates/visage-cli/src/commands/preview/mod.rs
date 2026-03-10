mod font;
mod render;
mod text_only;
#[cfg(feature = "wayland")]
mod wayland_preview;

use anyhow::Context;
use visage_core::Config;

fn resolve_user(user: Option<String>) -> anyhow::Result<String> {
    match user {
        Some(u) => Ok(u),
        None => {
            let uid = unsafe { libc::getuid() };
            let pw = unsafe { libc::getpwuid(uid) };
            if pw.is_null() {
                anyhow::bail!("could not determine current user");
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) };
            Ok(name.to_string_lossy().into_owned())
        }
    }
}

pub fn run(text_only: bool, user: Option<String>) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    let socket_path = config.daemon.socket_path.clone();
    let user = resolve_user(user)?;

    if text_only {
        return text_only::run(&socket_path, &user);
    }

    run_graphical(&socket_path, &user)
}

#[cfg(feature = "wayland")]
fn run_graphical(socket_path: &str, user: &str) -> anyhow::Result<()> {
    match wayland_preview::run(socket_path, user) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("Wayland preview failed: {e}");
            eprintln!(
                "Wayland preview unavailable: {e}\n\
                 Falling back to text-only mode.\n"
            );
            text_only::run(socket_path, user)
        }
    }
}

#[cfg(not(feature = "wayland"))]
fn run_graphical(socket_path: &str, user: &str) -> anyhow::Result<()> {
    eprintln!(
        "Graphical preview not available (compiled without wayland feature).\n\
         Using text-only mode.\n"
    );
    text_only::run(socket_path, user)
}
