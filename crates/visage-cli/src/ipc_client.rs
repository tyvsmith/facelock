use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context, bail};
use nix::unistd::Uid;
use visage_core::ipc::{
    DaemonRequest, DaemonResponse, decode_response, encode_request, recv_message, send_message,
};

/// Default read/write timeout for IPC.
const IO_TIMEOUT_SECS: u64 = 30;

/// Check if running as root; if not, offer to re-exec via sudo.
///
/// `hint` is a human-readable description like "sudo visage setup".
/// If stdin is a TTY, prompts the user and re-execs. Otherwise bails
/// with an actionable error message.
pub fn require_root(hint: &str) -> anyhow::Result<()> {
    if Uid::current().is_root() {
        return Ok(());
    }

    let is_tty = unsafe { libc::isatty(0) } != 0;

    // Non-interactive: just bail with instructions
    if !is_tty {
        bail!("Root required.\n  Run: {hint}");
    }

    // Interactive: offer to re-exec with sudo
    eprint!("Root required. Re-run with sudo? [Y/n] ");
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("failed to read input")?;
    let answer = input.trim().to_lowercase();
    if answer == "n" || answer == "no" {
        bail!("Root required.\n  Run: {hint}");
    }

    // Re-exec with sudo, preserving all arguments
    let args: Vec<String> = std::env::args().collect();
    let status = std::process::Command::new("sudo")
        .args(&args)
        .status()
        .context("failed to execute sudo")?;

    std::process::exit(status.code().unwrap_or(1));
}

/// Check whether we should use direct (daemonless) mode.
/// Returns true if config says "oneshot" OR if the daemon socket isn't reachable.
/// When falling back from daemon mode, logs a warning.
pub fn should_use_direct(config: &visage_core::Config) -> bool {
    if config.daemon.mode == visage_core::DaemonMode::Oneshot {
        return true;
    }
    // Daemon mode — check if socket exists, fall back silently to direct mode
    !std::path::Path::new(&config.daemon.socket_path).exists()
}

/// Connect to the daemon socket and send a request, returning the response.
pub fn send_request(socket_path: &str, request: &DaemonRequest) -> anyhow::Result<DaemonResponse> {
    let mut stream = connect(socket_path)?;

    let encoded = encode_request(request).context("failed to encode IPC request")?;
    send_message(&mut stream, &encoded).context("failed to send IPC message")?;

    let response_data = recv_message(&mut stream).context("failed to receive IPC response")?;
    let response = decode_response(&response_data).context("failed to decode IPC response")?;

    // Check for error responses
    if let DaemonResponse::Error { ref message } = response {
        bail!("daemon error: {message}");
    }

    Ok(response)
}

/// Connect to the daemon Unix socket with timeouts.
fn connect(socket_path: &str) -> anyhow::Result<UnixStream> {
    let stream = UnixStream::connect(socket_path).with_context(|| {
        format!(
            "failed to connect to daemon at {socket_path}\n\
             Is the daemon running? Try: visage status"
        )
    })?;

    stream
        .set_read_timeout(Some(Duration::from_secs(IO_TIMEOUT_SECS)))
        .context("failed to set read timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(IO_TIMEOUT_SECS)))
        .context("failed to set write timeout")?;

    Ok(stream)
}

/// Resolve the target user for commands.
///
/// Priority: explicit --user flag > SUDO_USER > DOAS_USER > current user.
pub fn resolve_user(flag: Option<&str>) -> String {
    flag.map(String::from)
        .or_else(|| std::env::var("SUDO_USER").ok())
        .or_else(|| std::env::var("DOAS_USER").ok())
        .unwrap_or_else(|| {
            std::env::var("USER").ok().unwrap_or_else(|| {
                // Fall back to getpwuid if $USER is not set (e.g. in containers)
                let uid = unsafe { libc::getuid() };
                let pw = unsafe { libc::getpwuid(uid) };
                if pw.is_null() {
                    "unknown".into()
                } else {
                    let cstr = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) };
                    cstr.to_str().unwrap_or("unknown").to_string()
                }
            })
        })
}

/// Read a yes/no confirmation from stdin. Returns true if user confirms.
pub fn confirm(prompt: &str) -> anyhow::Result<bool> {
    eprint!("{prompt} [y/N] ");
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("failed to read input")?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_user_with_flag() {
        let user = resolve_user(Some("alice"));
        assert_eq!(user, "alice");
    }

    #[test]
    fn resolve_user_no_flag_falls_through() {
        // When no flag is set, it checks env vars then current user.
        // We can't control env vars reliably in tests, but at minimum
        // it should return a non-empty string.
        let user = resolve_user(None);
        assert!(!user.is_empty());
    }
}
