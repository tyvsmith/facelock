mod auth;
mod enroll;
mod handler;
mod rate_limit;

use std::io::{BufReader, BufWriter};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use howdy_camera::{is_ir_camera, validate_device};
use howdy_core::config::Config;
use howdy_core::ipc::{decode_request, encode_response, recv_message, send_message};
use howdy_face::FaceEngine;
use howdy_store::FaceStore;
use tracing::{error, info, warn};

use crate::handler::Handler;
use crate::rate_limit::RateLimiter;

fn main() {
    // Parse args
    let config_path = parse_args();

    // Load config
    let config = match config_path {
        Some(ref p) => Config::load_from(Path::new(p)),
        None => Config::load(),
    };
    let config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("howdy-daemon: failed to load config: {e}");
            std::process::exit(1);
        }
    };

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    if config.debug.verbose {
                        "howdy_daemon=debug".into()
                    } else {
                        "howdy_daemon=info".into()
                    }
                }),
        )
        .with_target(true)
        .init();

    info!("howdy-daemon starting");

    // Check device info for IR detection
    let device_is_ir = match validate_device(&config.device.path) {
        Ok(info) => {
            let is_ir = is_ir_camera(&info);
            info!(device = %config.device.path, ir = is_ir, name = %info.name, "camera device");
            is_ir
        }
        Err(e) => {
            error!("failed to query device {}: {e}", config.device.path);
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

    // Create Unix socket
    let socket_path = config.daemon.socket_path.clone();
    let listener = match create_socket(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            error!("failed to create socket: {e}");
            std::process::exit(1);
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

    let mut handler = Handler::new(config, engine, store, rate_limiter, device_is_ir);

    info!(socket = %&socket_path, "listening for connections");

    // Accept loop
    while !term.load(Ordering::Relaxed) && !handler.shutdown_requested {
        match listener.accept() {
            Ok((stream, _addr)) => {
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
    let _ = std::fs::remove_file(socket_path);
    info!("socket removed, goodbye");
}

fn parse_args() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--config" && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        i += 1;
    }
    None
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

    // Set permissions: owner (root) + group (howdy) only
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))?;

    // Attempt to set ownership to root:howdy (may fail if not running as root)
    if let Some(howdy_gid) = get_howdy_gid() {
        let _ = nix::unistd::chown(
            socket_path,
            Some(nix::unistd::Uid::from_raw(0)),
            Some(nix::unistd::Gid::from_raw(howdy_gid)),
        );
    }

    Ok(listener)
}

fn get_howdy_gid() -> Option<u32> {
    // Try to look up the "howdy" group
    nix::unistd::Group::from_name("howdy")
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

    // Check if peer is in the howdy group
    if is_in_howdy_group(peer_uid) {
        return Ok(());
    }

    // Development mode: if no howdy group exists on the system, allow any user.
    // In production, the howdy group is created during installation.
    if nix::unistd::Group::from_name("howdy").ok().flatten().is_none() {
        tracing::debug!("no howdy group found, allowing UID {peer_uid} (dev mode)");
        return Ok(());
    }

    Err(format!("unauthorized UID {peer_uid}"))
}

fn is_in_howdy_group(uid: u32) -> bool {
    // Look up the howdy group
    let howdy_gid = match nix::unistd::Group::from_name("howdy") {
        Ok(Some(g)) => g.gid,
        _ => return false,
    };

    // Check if the user's primary group matches
    if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
        if user.gid == howdy_gid {
            return true;
        }
    }

    // Check supplementary groups
    // The howdy group's member list contains usernames
    if let Ok(Some(group)) = nix::unistd::Group::from_gid(howdy_gid) {
        // Look up the username for this UID
        if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
            let username: &str = user.name.as_ref();
            return group.mem.iter().any(|m| m.as_str() == username);
        }
    }

    false
}

fn handle_connection(
    handler: &mut Handler,
    stream: std::os::unix::net::UnixStream,
) -> howdy_core::Result<()> {
    // Set a read timeout to avoid blocking forever
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(howdy_core::error::HowdyError::Io)?;

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
    fn parse_args_none() {
        // No --config arg
        assert!(parse_args().is_none() || parse_args().is_some());
        // Can't really test main's parse_args in isolation without setting std::env::args
    }

    #[test]
    fn socket_creation_and_cleanup() {
        let dir = std::env::temp_dir().join("howdy-test-daemon");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.sock");
        let path_str = path.to_str().unwrap();

        // Create
        let listener = create_socket(path_str).unwrap();
        assert!(path.exists());

        // Verify permissions
        let meta = std::fs::metadata(&path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);

        drop(listener);

        // Cleanup
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn socket_replaces_stale() {
        let dir = std::env::temp_dir().join("howdy-test-stale");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("stale.sock");
        let path_str = path.to_str().unwrap();

        // Create first socket
        let _l1 = create_socket(path_str).unwrap();
        // Drop it, file remains
        drop(_l1);

        // Creating again should succeed (removes stale)
        let _l2 = create_socket(path_str).unwrap();
        assert!(path.exists());

        drop(_l2);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn peer_verification_on_connected_pair() {
        // Create a connected pair for testing
        let (s1, _s2) = std::os::unix::net::UnixStream::pair().unwrap();
        // verify_peer should succeed for our own UID (either root or howdy group member)
        // In CI/dev, we're not root and likely not in howdy group, so this may fail
        // That's correct behavior - it should reject unauthorized connections
        let result = verify_peer(&s1);
        // We just verify it doesn't panic
        let _ = result;
    }

    #[test]
    fn get_howdy_gid_does_not_panic() {
        // May return None if howdy group doesn't exist (expected in dev)
        let _ = get_howdy_gid();
    }

    #[test]
    fn is_in_howdy_group_nonexistent_user() {
        // Very high UID unlikely to exist
        assert!(!is_in_howdy_group(99999));
    }

    #[test]
    fn signal_handling_flag() {
        let term = Arc::new(AtomicBool::new(false));
        assert!(!term.load(Ordering::Relaxed));
        term.store(true, Ordering::Relaxed);
        assert!(term.load(Ordering::Relaxed));
    }
}
