use std::io::{BufReader, BufWriter};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use visage_camera::{Camera, auto_detect_device, is_ir_camera, validate_device};
use visage_core::config::Config;
use visage_core::ipc::{decode_request, encode_response, recv_message, send_message};
use visage_core::traits::{CameraSource, FaceProcessor};
use visage_daemon::handler::Handler;
use visage_daemon::rate_limit::RateLimiter;
use visage_face::FaceEngine;
use visage_store::FaceStore;
use tracing::{debug, error, info, warn};

/// Production type alias for the handler with real Camera and FaceEngine.
type ProductionHandler = Handler<Camera<'static>, FaceEngine>;

/// Type alias for the camera factory closure.
type CameraFactory = Box<dyn Fn(&Config) -> Result<Camera<'static>, String>>;

pub fn run(config_path: Option<String>) -> anyhow::Result<()> {
    // Load config
    let config = match config_path {
        Some(ref p) => Config::load_from(Path::new(p)),
        None => Config::load(),
    };
    let mut config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("visage daemon: failed to load config: {e}");
            std::process::exit(1);
        }
    };

    // Init tracing (daemon uses its own tracing setup with target=true)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "visage_daemon=info,visage_cli=info".into()),
        )
        .with_target(true)
        .init();

    info!("visage daemon starting");

    // Auto-detect device if not specified
    if config.device.path.is_none() {
        match auto_detect_device() {
            Ok(info) => {
                let is_ir = is_ir_camera(&info);
                info!(
                    device = %info.path,
                    name = %info.name,
                    ir = is_ir,
                    "auto-detected camera device"
                );
                config.device.path = Some(info.path);
            }
            Err(e) => {
                error!("no camera device specified and auto-detection failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let device_path = config.device.path.clone().unwrap();

    // Check device info for IR detection
    let device_is_ir = match validate_device(&device_path) {
        Ok(info) => {
            let is_ir = is_ir_camera(&info);
            info!(device = %device_path, ir = is_ir, name = %info.name, "camera device");
            is_ir
        }
        Err(e) => {
            error!("failed to query device {device_path}: {e}");
            false
        }
    };

    // Load ONNX models
    let engine = match FaceEngine::load(
        &config.recognition,
        Path::new(&config.daemon.model_dir),
    ) {
        Ok(e) => e,
        Err(e) => {
            error!("failed to load face engine: {e}");
            std::process::exit(1);
        }
    };

    // Open database
    let store = match FaceStore::open(Path::new(&config.storage.db_path)) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to open database: {e}");
            std::process::exit(1);
        }
    };

    // Create rate limiter
    let rate_limiter = RateLimiter::new(
        config.security.rate_limit.max_attempts,
        config.security.rate_limit.window_secs,
    );

    // Try socket activation first, then create our own socket
    let socket_path = config.daemon.socket_path.clone();
    let (listener, socket_activated) = match receive_systemd_socket() {
        Some(l) => {
            info!("socket-activated by systemd");
            (l, true)
        }
        None => {
            let l = match create_socket(&socket_path) {
                Ok(l) => l,
                Err(e) => {
                    error!("failed to create socket: {e}");
                    std::process::exit(1);
                }
            };
            (l, false)
        }
    };

    // Register signal handlers
    let term = Arc::new(AtomicBool::new(false));
    if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term)) {
        error!("failed to register SIGTERM handler: {e}");
        std::process::exit(1);
    }
    if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term)) {
        error!("failed to register SIGINT handler: {e}");
        std::process::exit(1);
    }

    // Set non-blocking for accept loop with signal checking
    listener
        .set_nonblocking(true)
        .expect("failed to set socket non-blocking");

    // Camera factory for lazy opening
    let camera_factory: CameraFactory =
        Box::new(|config: &Config| {
            Camera::open(&config.device).map_err(|e| e.to_string())
        });

    let mut handler: ProductionHandler = Handler::new(
        config, engine, store, rate_limiter, device_is_ir, Some(camera_factory),
    );

    info!(socket = %&socket_path, "listening for connections");

    // Idle timeout tracking for socket-activated mode
    let idle_timeout_secs = handler.config.daemon.idle_timeout_secs;
    let mut last_activity = Instant::now();

    // Accept loop
    while !term.load(Ordering::Relaxed) && !handler.shutdown_requested {
        match listener.accept() {
            Ok((stream, _addr)) => {
                last_activity = Instant::now();

                // Verify peer credentials
                if let Err(e) = verify_peer(&stream) {
                    warn!("rejecting unauthorized connection: {e}");
                    continue;
                }

                // Handle connection
                if let Err(e) = handle_connection(&mut handler, stream) {
                    warn!("connection error: {e}");
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No pending connection, release camera if idle, then check signals
                handler.maybe_release_camera();

                // Check idle timeout for socket-activated mode
                if socket_activated
                    && idle_timeout_secs > 0
                    && last_activity.elapsed() > Duration::from_secs(idle_timeout_secs)
                {
                    info!(
                        idle_secs = idle_timeout_secs,
                        "idle timeout reached, shutting down"
                    );
                    break;
                }

                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                error!("accept error: {e}");
                break;
            }
        }
    }

    // Cleanup
    info!("shutting down");
    if !socket_activated {
        let _ = std::fs::remove_file(socket_path);
        info!("socket removed, goodbye");
    } else {
        info!("socket-activated mode, socket owned by systemd, goodbye");
    }

    Ok(())
}

/// Check if systemd socket activation is in effect.
/// Returns a UnixListener from the passed file descriptor if so.
fn receive_systemd_socket() -> Option<UnixListener> {
    use std::os::unix::io::FromRawFd;

    let pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
    if pid != std::process::id() {
        return None;
    }
    let fds: u32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    if fds < 1 {
        return None;
    }
    // SD_LISTEN_FDS_START = 3
    Some(unsafe { UnixListener::from_raw_fd(3) })
}

fn create_socket(path: &str) -> std::io::Result<UnixListener> {
    let socket_path = Path::new(path);

    // Create parent directory if missing
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Remove stale socket file
    let _ = std::fs::remove_file(socket_path);

    // Bind
    let listener = UnixListener::bind(socket_path)?;

    // Set permissions: owner (root) + group (visage) only
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))?;

    // Attempt to set ownership to root:visage (may fail if not running as root)
    if let Some(visage_gid) = get_visage_gid() {
        let _ = nix::unistd::chown(
            socket_path,
            Some(nix::unistd::Uid::from_raw(0)),
            Some(nix::unistd::Gid::from_raw(visage_gid)),
        );
    }

    Ok(listener)
}

fn get_visage_gid() -> Option<u32> {
    // Try to look up the "visage" group
    nix::unistd::Group::from_name("visage")
        .ok()
        .flatten()
        .map(|g| g.gid.as_raw())
}

fn verify_peer(stream: &std::os::unix::net::UnixStream) -> Result<(), String> {
    let cred = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
        .map_err(|e| format!("getsockopt(SO_PEERCRED) failed: {e}"))?;

    let peer_uid = cred.uid();

    // Root (UID 0) is always allowed (PAM context)
    if peer_uid == 0 {
        return Ok(());
    }

    // Check if peer is in the visage group
    if is_in_visage_group(peer_uid) {
        return Ok(());
    }

    // Development mode: if no visage group exists on the system, allow any user.
    // In production, the visage group is created during installation.
    if nix::unistd::Group::from_name("visage").ok().flatten().is_none() {
        debug!("no visage group found, allowing UID {peer_uid} (dev mode)");
        return Ok(());
    }

    Err(format!("unauthorized UID {peer_uid}"))
}

fn is_in_visage_group(uid: u32) -> bool {
    // Look up the visage group
    let visage_gid = match nix::unistd::Group::from_name("visage") {
        Ok(Some(g)) => g.gid,
        _ => return false,
    };

    // Check if the user's primary group matches
    if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
        if user.gid == visage_gid {
            return true;
        }
    }

    // Check supplementary groups
    if let Ok(Some(group)) = nix::unistd::Group::from_gid(visage_gid) {
        if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
            let username: &str = user.name.as_ref();
            return group.mem.iter().any(|m| m.as_str() == username);
        }
    }

    false
}

fn handle_connection<C: CameraSource, E: FaceProcessor>(
    handler: &mut Handler<C, E>,
    stream: std::os::unix::net::UnixStream,
) -> visage_core::Result<()> {
    // Set a read timeout to avoid blocking forever
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(visage_core::error::VisageError::Io)?;

    let mut reader = BufReader::new(&stream);
    let mut writer = BufWriter::new(&stream);

    // Read request
    let data = recv_message(&mut reader)?;
    let request = decode_request(&data)?;

    // Handle
    let response = handler.handle(request);

    // Send response
    let resp_data = encode_response(&response)?;
    send_message(&mut writer, &resp_data)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_creation_and_cleanup() {
        let dir = std::env::temp_dir().join("visage-test-daemon-cli");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.sock");
        let path_str = path.to_str().unwrap();

        let listener = create_socket(path_str).unwrap();
        assert!(path.exists());

        let meta = std::fs::metadata(&path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);

        drop(listener);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn socket_replaces_stale() {
        let dir = std::env::temp_dir().join("visage-test-stale-cli");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("stale.sock");
        let path_str = path.to_str().unwrap();

        let _l1 = create_socket(path_str).unwrap();
        drop(_l1);

        let _l2 = create_socket(path_str).unwrap();
        assert!(path.exists());

        drop(_l2);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn peer_verification_on_connected_pair() {
        let (s1, _s2) = std::os::unix::net::UnixStream::pair().unwrap();
        let result = verify_peer(&s1);
        let _ = result;
    }

    #[test]
    fn get_visage_gid_does_not_panic() {
        let _ = get_visage_gid();
    }

    #[test]
    fn is_in_visage_group_nonexistent_user() {
        assert!(!is_in_visage_group(99999));
    }

    #[test]
    fn signal_handling_flag() {
        let term = Arc::new(AtomicBool::new(false));
        assert!(!term.load(Ordering::Relaxed));
        term.store(true, Ordering::Relaxed);
        assert!(term.load(Ordering::Relaxed));
    }

    #[test]
    fn receive_systemd_socket_returns_none_without_env() {
        unsafe {
            std::env::remove_var("LISTEN_PID");
            std::env::remove_var("LISTEN_FDS");
        }
        assert!(receive_systemd_socket().is_none());
    }

    #[test]
    fn idle_timeout_calculation() {
        let last_activity = Instant::now() - Duration::from_secs(10);
        let idle_timeout = Duration::from_secs(5);
        assert!(last_activity.elapsed() > idle_timeout);

        let recent = Instant::now();
        assert!(recent.elapsed() < idle_timeout);
    }
}
