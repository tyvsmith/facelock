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
use nix::unistd::{Group, Uid, User};
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct CallerIdentity {
    uid: u32,
    username: Option<String>,
    in_facelock_group: bool,
}

impl CallerIdentity {
    fn display_name(&self) -> String {
        self.username
            .clone()
            .unwrap_or_else(|| format!("UID {}", self.uid))
    }
}

async fn resolve_caller_identity(
    hdr: &zbus::message::Header<'_>,
    connection: &zbus::Connection,
) -> fdo::Result<CallerIdentity> {
    let sender = hdr
        .sender()
        .ok_or_else(|| fdo::Error::Failed("no sender in D-Bus message".into()))?;

    let dbus_proxy = fdo::DBusProxy::new(connection)
        .await
        .map_err(|e| fdo::Error::Failed(format!("failed to create DBus proxy: {e}")))?;
    let uid = dbus_proxy
        .get_connection_unix_user(sender.as_ref().into())
        .await
        .map_err(|e| fdo::Error::Failed(format!("failed to get caller UID: {e}")))?;

    let username = uid_to_username(uid);
    Ok(CallerIdentity {
        uid,
        in_facelock_group: is_facelock_group_member(uid, username.as_deref()),
        username,
    })
}

fn require_root(caller: &CallerIdentity, operation: &str) -> fdo::Result<()> {
    if caller.uid == 0 {
        return Ok(());
    }

    let caller_name = caller.display_name();
    warn!(
        operation = operation,
        caller_uid = caller.uid,
        caller_name = %caller_name,
        "D-Bus caller not authorized for privileged operation"
    );
    Err(fdo::Error::AccessDenied(format!(
        "{operation} requires root (caller: '{caller_name}', UID {})",
        caller.uid
    )))
}

fn require_user_authorized(
    caller: &CallerIdentity,
    user: &str,
    operation: &str,
) -> fdo::Result<()> {
    if caller.uid == 0 {
        return Ok(());
    }

    let caller_name = caller.username.clone().ok_or_else(|| {
        fdo::Error::Failed(format!("failed to resolve UID {} to username", caller.uid))
    })?;

    if caller_name == user {
        return Ok(());
    }

    warn!(
        operation = operation,
        caller_uid = caller.uid,
        caller_name = %caller_name,
        requested_user = %user,
        "D-Bus caller not authorized to act on behalf of requested user"
    );
    Err(fdo::Error::AccessDenied(format!(
        "{operation} not authorized: caller '{caller_name}' (UID {}) cannot act as '{user}'",
        caller.uid
    )))
}

fn require_camera_owner_or_root(
    caller: &CallerIdentity,
    owner_uid: Option<u32>,
    operation: &str,
) -> fdo::Result<()> {
    if caller.uid == 0 {
        return Ok(());
    }

    if owner_uid == Some(caller.uid) {
        return Ok(());
    }

    let caller_name = caller.display_name();
    warn!(
        operation = operation,
        caller_uid = caller.uid,
        caller_name = %caller_name,
        owner_uid,
        "D-Bus caller does not own the active preview camera session"
    );
    Err(fdo::Error::AccessDenied(format!(
        "{operation} not authorized: caller '{caller_name}' (UID {}) does not own the active preview camera session",
        caller.uid
    )))
}

fn require_facelock_access(caller: &CallerIdentity, operation: &str) -> fdo::Result<()> {
    if caller.uid == 0 || caller.in_facelock_group {
        return Ok(());
    }

    let caller_name = caller.display_name();
    warn!(
        operation = operation,
        caller_uid = caller.uid,
        caller_name = %caller_name,
        "D-Bus caller is not in facelock group"
    );
    Err(fdo::Error::AccessDenied(format!(
        "{operation} requires root or facelock group membership (caller: '{caller_name}', UID {})",
        caller.uid
    )))
}

async fn verify_caller_is_root(
    hdr: &zbus::message::Header<'_>,
    connection: &zbus::Connection,
    operation: &str,
) -> fdo::Result<()> {
    let caller = resolve_caller_identity(hdr, connection).await?;
    require_root(&caller, operation)
}

async fn verify_caller_authorized(
    hdr: &zbus::message::Header<'_>,
    connection: &zbus::Connection,
    user: &str,
    operation: &str,
) -> fdo::Result<()> {
    let caller = resolve_caller_identity(hdr, connection).await?;
    require_user_authorized(&caller, user, operation)
}

fn uid_to_username(uid: u32) -> Option<String> {
    User::from_uid(Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|user| user.name)
}

fn is_facelock_group_member(uid: u32, username: Option<&str>) -> bool {
    let Some(group) = Group::from_name("facelock").ok().flatten() else {
        return false;
    };

    let in_primary_group = User::from_uid(Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|user| user.gid == group.gid)
        .unwrap_or(false);

    let listed_in_group = username
        .map(|name| group.mem.iter().any(|member| member == name))
        .unwrap_or(false);

    in_primary_group || listed_in_group
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
    /// Config file mtime when the handler was last built.
    /// Used to detect config changes and reload on next request.
    config_mtime: Arc<Mutex<Option<std::time::SystemTime>>>,
    /// UID of the caller that currently owns preview camera cleanup rights.
    camera_owner_uid: Arc<Mutex<Option<u32>>>,
}

impl FacelockService {
    fn clear_camera_owner(&self) {
        if let Ok(mut owner) = self.camera_owner_uid.lock() {
            *owner = None;
        }
    }

    fn set_camera_owner(&self, uid: u32) {
        if let Ok(mut owner) = self.camera_owner_uid.lock() {
            *owner = Some(uid);
        }
    }

    fn camera_owner_uid(&self) -> Option<u32> {
        self.camera_owner_uid.lock().ok().and_then(|owner| *owner)
    }

    /// Check if the config file has been modified since the handler was built.
    /// If so, reload config, rebuild the engine/store/handler, and swap it in.
    /// Called at the start of authenticate and enroll — the two methods that
    /// depend on cached ONNX models and config values.
    fn maybe_reload_handler(&self) {
        let config_path = facelock_core::paths::config_path();
        let current_mtime = std::fs::metadata(&config_path)
            .and_then(|m| m.modified())
            .ok();

        let needs_reload = {
            let stored = self.config_mtime.lock().unwrap();
            match (*stored, current_mtime) {
                (Some(old), Some(new)) => new > old,
                _ => false,
            }
        };

        if !needs_reload {
            return;
        }

        info!("config file changed, reloading");

        let new_handler = match build_handler(None) {
            Ok((handler, _idle)) => handler,
            Err(e) => {
                warn!("failed to reload config: {e} — continuing with old config");
                return;
            }
        };

        // Swap in the new handler
        if let Ok(mut guard) = self.handler.lock() {
            *guard = new_handler;
        }
        self.clear_camera_owner();

        // Update stored mtime
        if let Ok(mut stored) = self.config_mtime.lock() {
            *stored = current_mtime;
        }

        info!("handler reloaded with new config");
    }
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
        self.maybe_reload_handler();
        verify_caller_authorized(&hdr, connection, user, "Authenticate").await?;
        self.clear_camera_owner();
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
        self.maybe_reload_handler();
        verify_caller_is_root(&hdr, connection, "Enroll").await?;
        self.clear_camera_owner();
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
        verify_caller_authorized(&hdr, connection, user, "ListModels").await?;
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
                        embedder_model: m.embedder_model,
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
        verify_caller_is_root(&hdr, connection, "RemoveModel").await?;
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
        verify_caller_is_root(&hdr, connection, "ClearModels").await?;
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

    async fn preview_frame(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<Vec<u8>> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let caller = resolve_caller_identity(&hdr, connection).await?;
        require_root(&caller, "PreviewFrame")?;
        let handler = self.handler.clone();
        let result = tokio::task::spawn_blocking(move || {
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
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?;
        if result.is_ok() {
            self.set_camera_owner(caller.uid);
        }
        result
    }

    async fn preview_detect_frame(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
    ) -> fdo::Result<(Vec<u8>, Vec<PreviewFaceInfo>)> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let caller = resolve_caller_identity(&hdr, connection).await?;
        require_user_authorized(&caller, user, "PreviewDetectFrame")?;
        let handler = self.handler.clone();
        let user = user.to_string();
        let result = tokio::task::spawn_blocking(move || {
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
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?;
        if result.is_ok() {
            self.set_camera_owner(caller.uid);
        }
        result
    }

    async fn list_devices(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<Vec<DeviceInfo>> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let caller = resolve_caller_identity(&hdr, connection).await?;
        require_facelock_access(&caller, "ListDevices")?;
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

    async fn release_camera(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let caller = resolve_caller_identity(&hdr, connection).await?;
        require_camera_owner_or_root(&caller, self.camera_owner_uid(), "ReleaseCamera")?;
        let handler = self.handler.clone();
        let result = tokio::task::spawn_blocking(move || {
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
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?;
        if result.is_ok() {
            self.clear_camera_owner();
        }
        result
    }

    async fn ping(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<String> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        let _ = resolve_caller_identity(&hdr, connection).await?;
        Ok("pong".to_string())
    }

    async fn shutdown(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        verify_caller_is_root(&hdr, connection, "Shutdown").await?;
        self.clear_camera_owner();
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            match handler.handle(DaemonRequest::Shutdown) {
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

/// Build a new handler from config. Used at startup and for live config reload.
/// Returns the handler and idle_timeout_secs from the loaded config.
fn build_handler(config_path: Option<&str>) -> Result<(ProductionHandler, u64), String> {
    let config = match config_path {
        Some(p) => Config::load_from(Path::new(p)),
        None => Config::load(),
    };
    let mut config = config.map_err(|e| format!("failed to load config: {e}"))?;

    let quirks = QuirksDb::load();

    if config.device.path.is_none() {
        let info = auto_detect_device()
            .map_err(|e| format!("no camera device specified and auto-detection failed: {e}"))?;
        let is_ir = is_ir_camera_with_quirks(&info, Some(&quirks));
        info!(device = %info.path, name = %info.name, ir = is_ir, "auto-detected camera device");
        config.device.path = Some(info.path);
    }

    let device_path = config.device.path.clone().unwrap();

    let device_is_ir = match validate_device(&device_path) {
        Ok(info) => {
            let is_ir = is_ir_camera_with_quirks(&info, Some(&quirks));
            info!(device = %device_path, ir = is_ir, name = %info.name, "camera device");
            is_ir
        }
        Err(e) => {
            warn!("failed to query device {device_path}: {e}");
            false
        }
    };

    let engine = FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir))
        .map_err(|e| format!("failed to load face engine: {e}"))?;

    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .map_err(|e| format!("failed to open database: {e}"))?;

    let rate_limiter = RateLimiter::new(
        config.security.rate_limit.max_attempts,
        config.security.rate_limit.window_secs,
    );

    let device_quirk = validate_device(&device_path)
        .ok()
        .and_then(|info| quirks.find_match(&info).cloned());

    let quirk_for_factory = device_quirk.clone();
    let camera_factory: CameraFactory = Box::new(move |config: &Config| {
        Camera::open(&config.device, quirk_for_factory.as_ref()).map_err(|e| e.to_string())
    });

    let idle_timeout_secs = config.daemon.idle_timeout_secs;
    let warmup_override = device_quirk.and_then(|q| q.warmup_frames);
    let handler = Handler::new(
        config,
        engine,
        store,
        rate_limiter,
        device_is_ir,
        Some(camera_factory),
        warmup_override,
    )?;

    Ok((handler, idle_timeout_secs))
}

pub fn run(config_path: Option<String>) -> anyhow::Result<()> {
    crate::ipc_client::require_root("sudo facelock daemon")?;

    // Init tracing (daemon uses its own tracing setup with target=true)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "facelock_daemon=info,facelock=info".into()),
        )
        .with_target(true)
        .init();

    info!("facelock daemon starting");

    let (handler, idle_timeout_secs) = match build_handler(config_path.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            error!("{e}");
            std::process::exit(1);
        }
    };

    let config_mtime = std::fs::metadata(facelock_core::paths::config_path())
        .and_then(|m| m.modified())
        .ok();

    let handler = Arc::new(Mutex::new(handler));

    // Build and run the tokio runtime for the D-Bus server
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(run_dbus_server(handler, idle_timeout_secs, config_mtime))
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
    startup_config_mtime: Option<std::time::SystemTime>,
) -> anyhow::Result<()> {
    let last_activity = Arc::new(AtomicU64::new(now_secs()));
    let service = FacelockService {
        handler: handler.clone(),
        last_activity: last_activity.clone(),
        config_mtime: Arc::new(Mutex::new(startup_config_mtime)),
        camera_owner_uid: Arc::new(Mutex::new(None)),
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

    fn caller(uid: u32, username: Option<&str>) -> CallerIdentity {
        CallerIdentity {
            uid,
            in_facelock_group: false,
            username: username.map(str::to_string),
        }
    }

    fn facelock_caller(uid: u32, username: Option<&str>) -> CallerIdentity {
        CallerIdentity {
            uid,
            in_facelock_group: true,
            username: username.map(str::to_string),
        }
    }

    #[test]
    fn bus_name_constants() {
        assert_eq!(BUS_NAME, "org.facelock.Daemon");
        assert_eq!(OBJECT_PATH, "/org/facelock/Daemon");
    }

    #[test]
    fn root_is_allowed_for_privileged_operations() {
        assert!(require_root(&caller(0, Some("root")), "Shutdown").is_ok());
    }

    #[test]
    fn same_user_is_allowed_for_user_scoped_methods() {
        assert!(
            require_user_authorized(&caller(1000, Some("alice")), "alice", "Authenticate").is_ok()
        );
    }

    #[test]
    fn different_user_is_denied_for_user_scoped_methods() {
        let err = require_user_authorized(&caller(1000, Some("alice")), "bob", "Authenticate")
            .unwrap_err();
        assert!(matches!(err, fdo::Error::AccessDenied(_)));
    }

    #[test]
    fn facelock_group_member_can_access_group_scoped_methods() {
        assert!(
            require_facelock_access(&facelock_caller(1000, Some("alice")), "ListDevices").is_ok()
        );
    }

    #[test]
    fn non_member_cannot_access_group_scoped_methods() {
        let err = require_facelock_access(&caller(1000, Some("alice")), "ListDevices").unwrap_err();
        assert!(matches!(err, fdo::Error::AccessDenied(_)));
    }

    #[test]
    fn preview_owner_can_release_camera() {
        assert!(
            require_camera_owner_or_root(&caller(1000, Some("alice")), Some(1000), "ReleaseCamera")
                .is_ok()
        );
    }

    #[test]
    fn root_can_release_camera() {
        assert!(
            require_camera_owner_or_root(&caller(0, Some("root")), Some(1000), "ReleaseCamera")
                .is_ok()
        );
    }

    #[test]
    fn non_owner_cannot_release_camera() {
        let err =
            require_camera_owner_or_root(&caller(1001, Some("bob")), Some(1000), "ReleaseCamera")
                .unwrap_err();
        assert!(matches!(err, fdo::Error::AccessDenied(_)));
    }
}
