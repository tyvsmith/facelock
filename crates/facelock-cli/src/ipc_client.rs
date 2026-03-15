use anyhow::{Context, bail};
use nix::unistd::Uid;
use zbus::blocking::Connection;
use zbus::blocking::Proxy;

use facelock_core::dbus_interface::*;
use facelock_core::ipc::{DaemonRequest, DaemonResponse, IpcDeviceInfo, PreviewFace};
use facelock_core::types::{FaceModelInfo, MatchResult};

/// Check if running as root; if not, offer to re-exec via sudo.
///
/// `hint` is a human-readable description like "sudo facelock setup".
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
/// Returns true if config says "oneshot" OR if the D-Bus service isn't available.
/// When falling back from daemon mode, logs a warning.
pub fn should_use_direct(config: &facelock_core::Config) -> bool {
    if config.daemon.mode == facelock_core::DaemonMode::Oneshot {
        return true;
    }
    // Daemon mode -- check if D-Bus service is available
    match Connection::system() {
        Ok(conn) => {
            let proxy = zbus::blocking::fdo::DBusProxy::new(&conn);
            match proxy {
                Ok(p) => match BUS_NAME.try_into() {
                    Ok(name) => !p.name_has_owner(name).unwrap_or(false),
                    Err(_) => true,
                },
                Err(_) => true,
            }
        }
        Err(_) => true,
    }
}

/// Create a blocking D-Bus proxy to the daemon with a 15-second method timeout.
fn create_proxy() -> anyhow::Result<Proxy<'static>> {
    let connection = zbus::blocking::connection::Builder::system()
        .map_err(|e| anyhow::anyhow!("D-Bus connection failed: {e}"))?
        .method_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow::anyhow!("D-Bus connection failed: {e}"))?;

    let proxy = Proxy::new_owned(connection, BUS_NAME, OBJECT_PATH, INTERFACE_NAME)
        .map_err(|e| anyhow::anyhow!("D-Bus proxy failed: {e}"))?;

    Ok(proxy)
}

/// Send a request to the daemon via D-Bus, translating to/from the old
/// DaemonRequest/DaemonResponse types used by the command layer.
pub fn send_request(request: &DaemonRequest) -> anyhow::Result<DaemonResponse> {
    let proxy = create_proxy()?;

    match request {
        DaemonRequest::Authenticate { user } => {
            let result: AuthResult = proxy
                .call("Authenticate", &(user.as_str(),))
                .context("D-Bus Authenticate call failed")?;
            Ok(DaemonResponse::AuthResult(MatchResult {
                matched: result.matched,
                model_id: if result.model_id >= 0 {
                    Some(result.model_id as u32)
                } else {
                    None
                },
                label: if result.label.is_empty() {
                    None
                } else {
                    Some(result.label)
                },
                similarity: result.similarity as f32,
            }))
        }
        DaemonRequest::Enroll { user, label } => {
            let result: (u32, u32) = proxy
                .call("Enroll", &(user.as_str(), label.as_str()))
                .context("D-Bus Enroll call failed")?;
            Ok(DaemonResponse::Enrolled {
                model_id: result.0,
                embedding_count: result.1,
            })
        }
        DaemonRequest::ListModels { user } => {
            let models: Vec<ModelInfo> = proxy
                .call("ListModels", &(user.as_str(),))
                .context("D-Bus ListModels call failed")?;
            Ok(DaemonResponse::Models(
                models
                    .into_iter()
                    .map(|m| FaceModelInfo {
                        id: m.id,
                        user: m.user,
                        label: m.label,
                        created_at: m.created_at,
                    })
                    .collect(),
            ))
        }
        DaemonRequest::RemoveModel { user, model_id } => {
            let _: bool = proxy
                .call("RemoveModel", &(user.as_str(), *model_id))
                .context("D-Bus RemoveModel call failed")?;
            Ok(DaemonResponse::Removed)
        }
        DaemonRequest::ClearModels { user } => {
            let _: u32 = proxy
                .call("ClearModels", &(user.as_str(),))
                .context("D-Bus ClearModels call failed")?;
            Ok(DaemonResponse::Removed)
        }
        DaemonRequest::PreviewFrame => {
            let jpeg_data: Vec<u8> = proxy
                .call("PreviewFrame", &())
                .context("D-Bus PreviewFrame call failed")?;
            Ok(DaemonResponse::Frame { jpeg_data })
        }
        DaemonRequest::PreviewDetectFrame { user } => {
            let result: (Vec<u8>, Vec<PreviewFaceInfo>) = proxy
                .call("PreviewDetectFrame", &(user.as_str(),))
                .context("D-Bus PreviewDetectFrame call failed")?;
            let (jpeg_data, face_infos) = result;
            let faces = face_infos
                .into_iter()
                .map(|f| PreviewFace {
                    x: f.x as f32,
                    y: f.y as f32,
                    width: f.width as f32,
                    height: f.height as f32,
                    confidence: f.confidence as f32,
                    similarity: f.similarity as f32,
                    recognized: f.recognized,
                })
                .collect();
            Ok(DaemonResponse::DetectFrame { jpeg_data, faces })
        }
        DaemonRequest::ListDevices => {
            let devices: Vec<DeviceInfo> = proxy
                .call("ListDevices", &())
                .context("D-Bus ListDevices call failed")?;
            Ok(DaemonResponse::Devices(
                devices
                    .into_iter()
                    .map(|d| IpcDeviceInfo {
                        path: d.path,
                        name: d.name,
                        driver: d.driver,
                        is_ir: d.is_ir,
                        formats: vec![],
                    })
                    .collect(),
            ))
        }
        DaemonRequest::ReleaseCamera => {
            let _: () = proxy
                .call("ReleaseCamera", &())
                .context("D-Bus ReleaseCamera call failed")?;
            Ok(DaemonResponse::Ok)
        }
        DaemonRequest::Ping => {
            let _: String = proxy.call("Ping", &()).context("D-Bus Ping call failed")?;
            Ok(DaemonResponse::Ok)
        }
        DaemonRequest::Shutdown => {
            let _: () = proxy
                .call("Shutdown", &())
                .context("D-Bus Shutdown call failed")?;
            Ok(DaemonResponse::Ok)
        }
    }
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
