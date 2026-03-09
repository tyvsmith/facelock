use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context, bail};
use howdy_core::ipc::{
    DaemonRequest, DaemonResponse, decode_response, encode_request, recv_message, send_message,
};

/// Default connection timeout for IPC.
const CONNECT_TIMEOUT_SECS: u64 = 5;
/// Default read/write timeout for IPC.
const IO_TIMEOUT_SECS: u64 = 30;

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
             Is the daemon running? Try: howdy status"
        )
    })?;

    stream
        .set_read_timeout(Some(Duration::from_secs(IO_TIMEOUT_SECS)))
        .context("failed to set read timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(IO_TIMEOUT_SECS)))
        .context("failed to set write timeout")?;

    // Use connect timeout indirectly: the connect call above will fail
    // if the daemon isn't listening. For explicit timeout we'd need
    // nonblocking connect, but for a CLI tool the default is sufficient.
    let _ = CONNECT_TIMEOUT_SECS;

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
            std::env::var("USER").unwrap_or_else(|_| "unknown".into())
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
