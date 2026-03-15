use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use facelock_camera::quirks::QuirksDb;
use facelock_camera::{Camera, auto_detect_device, is_ir_camera_with_quirks, validate_device};
use facelock_core::config::Config;
use facelock_core::dbus_interface::{
    AuthResult, BUS_NAME, DeviceInfo, ModelInfo, OBJECT_PATH, PreviewFaceInfo,
};
use facelock_core::ipc::{DaemonRequest, DaemonResponse};
use facelock_daemon::handler::Handler;
use facelock_daemon::rate_limit::RateLimiter;
use facelock_face::FaceEngine;
use facelock_store::FaceStore;
use futures_util::StreamExt;
use tracing::{error, info, warn};
use zbus::{fdo, interface, object_server::SignalEmitter};

/// Production type alias for the handler with real Camera and FaceEngine.
type ProductionHandler = Handler<Camera<'static>, FaceEngine>;

/// Maximum time to wait for the handler mutex before returning a "busy" error.
/// This prevents D-Bus clients from hanging indefinitely if a previous auth
/// call is stuck (e.g., camera blocking on DQBUF).
const HANDLER_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Try to acquire the handler mutex with a timeout.
/// Uses try_lock in a polling loop to avoid blocking the thread indefinitely.
fn lock_handler_with_timeout(
    handler: &Mutex<ProductionHandler>,
) -> std::result::Result<MutexGuard<'_, ProductionHandler>, fdo::Error> {
    let deadline = Instant::now() + HANDLER_LOCK_TIMEOUT;
    let mut waited = false;
    loop {
        match handler.try_lock() {
            Ok(guard) => {
                if waited {
                    warn!("handler lock acquired after waiting");
                }
                return Ok(guard);
            }
            Err(TryLockError::Poisoned(e)) => {
                error!("handler mutex poisoned (previous operation panicked), recovering");
                return Ok(e.into_inner());
            }
            Err(TryLockError::WouldBlock) => {
                if !waited {
                    warn!("handler lock contention — waiting for previous operation");
                    waited = true;
                }
                if Instant::now() >= deadline {
                    error!(
                        "handler lock timeout after {HANDLER_LOCK_TIMEOUT:?} — previous operation is stuck"
                    );
                    return Err(fdo::Error::Failed(
                        "daemon busy: previous operation timed out".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Verify the D-Bus caller is authorized to act on behalf of `user`.
/// Root (UID 0) can act on any user. Non-root callers must match `user`.
async fn verify_caller_authorized(
    hdr: &zbus::message::Header<'_>,
    connection: &zbus::Connection,
    user: &str,
) -> fdo::Result<()> {
    let sender = hdr
        .sender()
        .ok_or_else(|| fdo::Error::Failed("no sender in D-Bus message".into()))?;

    // Ask the bus daemon for the sender's Unix UID
    let dbus_proxy = fdo::DBusProxy::new(connection)
        .await
        .map_err(|e| fdo::Error::Failed(format!("failed to create DBus proxy: {e}")))?;
    let uid = dbus_proxy
        .get_connection_unix_user(sender.as_ref().into())
        .await
        .map_err(|e| fdo::Error::Failed(format!("failed to get caller UID: {e}")))?;

    // Root can operate on any user
    if uid == 0 {
        return Ok(());
    }

    // Resolve UID to username
    let caller_name = uid_to_username(uid)
        .ok_or_else(|| fdo::Error::Failed(format!("failed to resolve UID {uid} to username")))?;

    if caller_name != user {
        warn!(
            caller_uid = uid,
            caller_name = %caller_name,
            requested_user = %user,
            "D-Bus caller not authorized to act on behalf of requested user"
        );
        return Err(fdo::Error::AccessDenied(format!(
            "caller '{caller_name}' (UID {uid}) not authorized to act as '{user}'"
        )));
    }

    Ok(())
}

/// Resolve a Unix UID to a username via getpwuid_r.
fn uid_to_username(uid: u32) -> Option<String> {
    use std::ffi::CStr;
    let mut buf = vec![0u8; 4096];
    let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let ret = unsafe {
        libc::getpwuid_r(
            uid,
            passwd.as_mut_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        )
    };
    if ret != 0 || result.is_null() {
        return None;
    }
    let passwd = unsafe { passwd.assume_init() };
    let name = unsafe { CStr::from_ptr(passwd.pw_name) };
    name.to_str().ok().map(|s| s.to_string())
}

/// Current time as seconds since an arbitrary epoch (Instant-based).
/// Used for idle timeout tracking without wall-clock dependency.
fn now_secs() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_secs()
}

struct FacelockService {
    handler: Arc<Mutex<ProductionHandler>>,
    /// Timestamp of last D-Bus method call (seconds since daemon start).
    last_activity: Arc<AtomicU64>,
}

#[interface(name = "org.facelock.Daemon")]
impl FacelockService {
    async fn authenticate(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        user: &str,
    ) -> fdo::Result<AuthResult> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        verify_caller_authorized(&hdr, connection, user).await?;
        let handler = self.handler.clone();
        let user = user.to_string();
        let signal_user = user.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::Authenticate { user: user.clone() };
            let response = handler.handle(request);
            drop(handler);
            match response {
                DaemonResponse::AuthResult(result) => {
                    // Send desktop notification (fire-and-forget, runs as root → setpriv)
                    let event = if result.matched {
                        crate::notifications::NotifyEvent::Success {
                            label: result.label.clone(),
                            similarity: result.similarity,
                        }
                    } else {
                        crate::notifications::NotifyEvent::Failure {
                            reason: "no match".into(),
                        }
                    };
                    // Re-read notification config from disk so changes
                    // take effect without daemon restart
                    let notify_config = Config::load().map(|c| c.notification).unwrap_or_default();
                    crate::notifications::notify_if_enabled_for_user(&notify_config, &event, &user);

                    Ok(AuthResult {
                        matched: result.matched,
                        model_id: result.model_id.map(|id| id as i32).unwrap_or(-1),
                        label: result.label.unwrap_or_default(),
                        similarity: result.similarity as f64,
                    })
                }
                DaemonResponse::Suppressed => {
                    // No enrolled models + suppress_unknown enabled.
                    // Return model_id=-3 as a marker so the PAM module
                    // can map this to PAM_AUTHINFO_UNAVAIL.
                    Ok(AuthResult {
                        matched: false,
                        model_id: -3,
                        label: String::new(),
                        similarity: 0.0,
                    })
                }
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?;

        // Emit auth_attempted signal (best-effort, don't fail auth if signal fails)
        if let Ok(ref auth_result) = result {
            let _ = Self::auth_attempted(
                &ctxt,
                &signal_user,
                auth_result.matched,
                auth_result.similarity,
            )
            .await;
        }

        result
    }

    async fn enroll(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
        label: &str,
    ) -> fdo::Result<(u32, u32)> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        verify_caller_authorized(&hdr, connection, user).await?;
        let handler = self.handler.clone();
        let user = user.to_string();
        let label = label.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::Enroll { user, label };
            let response = handler.handle(request);
            match response {
                DaemonResponse::Enrolled {
                    model_id,
                    embedding_count,
                } => Ok((model_id, embedding_count)),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn list_models(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
    ) -> fdo::Result<Vec<ModelInfo>> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        verify_caller_authorized(&hdr, connection, user).await?;
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ListModels { user };
            let response = handler.handle(request);
            match response {
                DaemonResponse::Models(models) => Ok(models
                    .into_iter()
                    .map(|m| ModelInfo {
                        id: m.id,
                        user: m.user,
                        label: m.label,
                        created_at: m.created_at,
                    })
                    .collect()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn remove_model(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
        model_id: u32,
    ) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        verify_caller_authorized(&hdr, connection, user).await?;
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::RemoveModel { user, model_id };
            let response = handler.handle(request);
            match response {
                DaemonResponse::Removed => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn clear_models(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
    ) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        verify_caller_authorized(&hdr, connection, user).await?;
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ClearModels { user };
            let response = handler.handle(request);
            match response {
                DaemonResponse::Removed => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn preview_frame(&self) -> fdo::Result<Vec<u8>> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::PreviewFrame;
            let response = handler.handle(request);
            match response {
                DaemonResponse::Frame { jpeg_data } => Ok(jpeg_data),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn preview_detect_frame(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
    ) -> fdo::Result<(Vec<u8>, Vec<PreviewFaceInfo>)> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        verify_caller_authorized(&hdr, connection, user).await?;
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::PreviewDetectFrame { user };
            let response = handler.handle(request);
            match response {
                DaemonResponse::DetectFrame { jpeg_data, faces } => {
                    let face_infos: Vec<PreviewFaceInfo> = faces
                        .into_iter()
                        .map(|f| PreviewFaceInfo {
                            x: f.x as f64,
                            y: f.y as f64,
                            width: f.width as f64,
                            height: f.height as f64,
                            confidence: f.confidence as f64,
                            similarity: f.similarity as f64,
                            recognized: f.recognized,
                        })
                        .collect();
                    Ok((jpeg_data, face_infos))
                }
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn list_devices(&self) -> fdo::Result<Vec<DeviceInfo>> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ListDevices;
            let response = handler.handle(request);
            match response {
                DaemonResponse::Devices(devices) => Ok(devices
                    .into_iter()
                    .map(|d| DeviceInfo {
                        path: d.path,
                        name: d.name,
                        driver: d.driver,
                        is_ir: d.is_ir,
                    })
                    .collect()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn release_camera(&self) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ReleaseCamera;
            let response = handler.handle(request);
            match response {
                DaemonResponse::Ok => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn ping(&self) -> fdo::Result<String> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        Ok("pong".to_string())
    }

    async fn shutdown(&self) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            handler.shutdown_requested = true;
            Ok::<(), fdo::Error>(())
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    /// Signal emitted after each authentication attempt.
    #[zbus(signal)]
    async fn auth_attempted(
        emitter: &SignalEmitter<'_>,
        user: &str,
        matched: bool,
        similarity: f64,
    ) -> zbus::Result<()>;
}

/// Type alias for the camera factory closure.
type CameraFactory = Box<dyn Fn(&Config) -> Result<Camera<'static>, String> + Send + Sync>;

pub fn run(config_path: Option<String>) -> anyhow::Result<()> {
    crate::ipc_client::require_root("sudo facelock daemon")?;

    // Load config
    let config = match config_path {
        Some(ref p) => Config::load_from(Path::new(p)),
        None => Config::load(),
    };
    let mut config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("facelock daemon: failed to load config: {e}");
            std::process::exit(1);
        }
    };

    // Init tracing (daemon uses its own tracing setup with target=true)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "facelock_daemon=info,facelock=info".into()),
        )
        .with_target(true)
        .init();

    info!("facelock daemon starting");

    // Load hardware quirks database
    let quirks = QuirksDb::load();

    // Auto-detect device if not specified
    if config.device.path.is_none() {
        match auto_detect_device() {
            Ok(info) => {
                let is_ir = is_ir_camera_with_quirks(&info, Some(&quirks));
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
            let is_ir = is_ir_camera_with_quirks(&info, Some(&quirks));
            info!(device = %device_path, ir = is_ir, name = %info.name, "camera device");
            is_ir
        }
        Err(e) => {
            error!("failed to query device {device_path}: {e}");
            false
        }
    };

    // Load ONNX models
    let engine = match FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir)) {
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

    // Look up hardware quirk for this specific device
    let device_quirk = validate_device(&device_path)
        .ok()
        .and_then(|info| quirks.find_match(&info).cloned());

    // Camera factory for lazy opening — passes the matched quirk (if any)
    let quirk_for_factory = device_quirk.clone();
    let camera_factory: CameraFactory = Box::new(move |config: &Config| {
        Camera::open(&config.device, quirk_for_factory.as_ref()).map_err(|e| e.to_string())
    });

    let idle_timeout_secs = config.daemon.idle_timeout_secs;
    let warmup_override = device_quirk.and_then(|q| q.warmup_frames);
    let handler: ProductionHandler = match Handler::new(
        config,
        engine,
        store,
        rate_limiter,
        device_is_ir,
        Some(camera_factory),
        warmup_override,
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("failed to initialize handler: {e}");
            std::process::exit(1);
        }
    };

    let handler = Arc::new(Mutex::new(handler));

    // Build and run the tokio runtime for the D-Bus server
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(run_dbus_server(handler, idle_timeout_secs))
}

/// Drop all Linux capabilities and set PR_SET_NO_NEW_PRIVS.
///
/// After initialization the daemon has already opened the camera fd, loaded
/// models, connected to D-Bus, and opened the database. It no longer needs
/// any elevated capabilities, so we clear them all.
///
/// Returns `Ok(())` on success. Errors are non-fatal — the caller should
/// warn and continue.
fn drop_capabilities() -> std::result::Result<(), String> {
    // capget/capset use syscall numbers directly since libc doesn't expose
    // the cap structs on all platforms.
    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }

    #[repr(C)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }

    // _LINUX_CAPABILITY_VERSION_3 = 0x20080522
    const LINUX_CAP_V3: u32 = 0x2008_0522;

    unsafe {
        // Prevent the process (and children) from ever gaining new privileges
        let ret = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        if ret != 0 {
            return Err(format!(
                "prctl(PR_SET_NO_NEW_PRIVS) failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        // Clear all capability sets (effective, permitted, inheritable).
        // V3 uses two CapData structs (for caps 0-31 and 32-63).
        let mut header = CapHeader {
            version: LINUX_CAP_V3,
            pid: 0,
        };
        let mut data = [
            CapData {
                effective: 0,
                permitted: 0,
                inheritable: 0,
            },
            CapData {
                effective: 0,
                permitted: 0,
                inheritable: 0,
            },
        ];
        let ret = libc::syscall(
            libc::SYS_capset,
            &mut header as *mut CapHeader,
            data.as_mut_ptr(),
        );
        if ret != 0 {
            return Err(format!(
                "capset syscall failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

async fn run_dbus_server(
    handler: Arc<Mutex<ProductionHandler>>,
    idle_timeout_secs: u64,
) -> anyhow::Result<()> {
    let last_activity = Arc::new(AtomicU64::new(now_secs()));
    let service = FacelockService {
        handler: handler.clone(),
        last_activity: last_activity.clone(),
    };

    let _connection = zbus::connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service)?
        .build()
        .await?;

    info!("facelock daemon running on D-Bus system bus as {BUS_NAME}");

    // Drop capabilities now that initialization is complete — camera fd is
    // open, models are loaded, D-Bus is connected, database is open.
    match drop_capabilities() {
        Ok(()) => info!("dropped all capabilities and set no-new-privs"),
        Err(e) => warn!("failed to drop capabilities (continuing): {e}"),
    }

    // Spawn a background task to release the camera on system suspend.
    // Best-effort: if logind is unavailable, log a warning and continue.
    let handler_for_sleep = handler.clone();
    tokio::spawn(async move {
        if let Err(e) = watch_sleep_signals(handler_for_sleep).await {
            tracing::warn!("failed to watch logind sleep signals: {e}");
        }
    });

    // Wait for shutdown signal (SIGTERM or SIGINT)
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received SIGINT, shutting down");
        }
        _ = sigterm.recv() => {
            info!("received SIGTERM, shutting down");
        }
        _ = poll_shutdown(handler, last_activity, idle_timeout_secs) => {
            info!("shutdown requested via D-Bus or idle timeout, shutting down");
        }
    }

    info!("goodbye");
    Ok(())
}

/// Watch for logind `PrepareForSleep` signals.
///
/// On suspend (arg=true), release the camera so V4L2 handles don't go stale.
/// On resume (arg=false), just log — the camera will be re-acquired on demand.
///
/// Manual testing:
/// ```bash
/// # Start daemon, then:
/// sudo systemctl suspend
/// # After resume, check: journalctl -u facelock-daemon --since "5 min ago"
/// ```
async fn watch_sleep_signals(handler: Arc<Mutex<ProductionHandler>>) -> anyhow::Result<()> {
    let connection = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;

    let mut stream = proxy.receive_signal("PrepareForSleep").await?;
    info!("watching logind PrepareForSleep signals for camera suspend/resume");

    while let Some(signal) = stream.next().await {
        let suspending: bool = signal.body().deserialize().unwrap_or(false);
        if suspending {
            let handler = handler.clone();
            let _ = tokio::task::spawn_blocking(move || match handler.try_lock() {
                Ok(mut h) => {
                    h.handle(DaemonRequest::ReleaseCamera);
                    info!("released camera for suspend");
                }
                Err(_) => {
                    warn!("could not release camera for suspend: handler busy");
                }
            })
            .await;
        } else {
            info!("resumed from suspend, camera will reacquire on demand");
        }
    }
    Ok(())
}

/// Poll the handler's shutdown_requested flag, idle camera release, and idle timeout.
/// All mutex access goes through spawn_blocking to avoid blocking the
/// tokio runtime (which would deadlock D-Bus method dispatch).
async fn poll_shutdown(
    handler: Arc<Mutex<ProductionHandler>>,
    last_activity: Arc<AtomicU64>,
    idle_timeout_secs: u64,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Check idle timeout (0 = disabled)
        if idle_timeout_secs > 0 {
            let last = last_activity.load(Ordering::Relaxed);
            let now = now_secs();
            if now.saturating_sub(last) >= idle_timeout_secs {
                info!(
                    idle_secs = now.saturating_sub(last),
                    timeout = idle_timeout_secs,
                    "idle timeout reached, initiating shutdown"
                );
                return;
            }
        }

        let handler = handler.clone();
        let should_shutdown = tokio::task::spawn_blocking(move || {
            if let Ok(mut h) = handler.try_lock() {
                if h.shutdown_requested {
                    return true;
                }
                h.maybe_release_camera();
            }
            false
        })
        .await
        .unwrap_or(false);

        if should_shutdown {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_name_constants() {
        assert_eq!(BUS_NAME, "org.facelock.Daemon");
        assert_eq!(OBJECT_PATH, "/org/facelock/Daemon");
    }
}
