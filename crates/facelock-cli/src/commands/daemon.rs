use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use facelock_camera::{Camera, auto_detect_device, is_ir_camera, validate_device};
use facelock_core::config::Config;
use facelock_core::dbus_interface::{
    AuthResult, DeviceInfo, ModelInfo, PreviewFaceInfo, BUS_NAME, OBJECT_PATH,
};
use facelock_core::ipc::{DaemonRequest, DaemonResponse};
use futures_util::StreamExt;
use facelock_daemon::handler::Handler;
use facelock_daemon::rate_limit::RateLimiter;
use facelock_face::FaceEngine;
use facelock_store::FaceStore;
use tracing::{error, info, warn};
use zbus::{interface, fdo, object_server::SignalEmitter};

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
                error!("handler mutex poisoned (previous operation panicked): {e}");
                return Err(fdo::Error::Failed(format!("handler panicked: {e}")));
            }
            Err(TryLockError::WouldBlock) => {
                if !waited {
                    warn!("handler lock contention — waiting for previous operation");
                    waited = true;
                }
                if Instant::now() >= deadline {
                    error!("handler lock timeout after {HANDLER_LOCK_TIMEOUT:?} — previous operation is stuck");
                    return Err(fdo::Error::Failed(
                        "daemon busy: previous operation timed out".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

struct FacelockService {
    handler: Arc<Mutex<ProductionHandler>>,
}

#[interface(name = "org.facelock.Daemon")]
impl FacelockService {
    async fn authenticate(&self, user: &str) -> fdo::Result<AuthResult> {
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::Authenticate { user };
            let response = handler.handle(request);
            drop(handler);
            match response {
                DaemonResponse::AuthResult(result) => Ok(AuthResult {
                    matched: result.matched,
                    model_id: result.model_id.map(|id| id as i32).unwrap_or(-1),
                    label: result.label.unwrap_or_default(),
                    similarity: result.similarity as f64,
                }),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn enroll(&self, user: &str, label: &str) -> fdo::Result<(u32, u32)> {
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
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn list_models(&self, user: &str) -> fdo::Result<Vec<ModelInfo>> {
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
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn remove_model(&self, user: &str, model_id: u32) -> fdo::Result<()> {
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::RemoveModel { user, model_id };
            let response = handler.handle(request);
            match response {
                DaemonResponse::Removed => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn clear_models(&self, user: &str) -> fdo::Result<()> {
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ClearModels { user };
            let response = handler.handle(request);
            match response {
                DaemonResponse::Removed => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn preview_frame(&self) -> fdo::Result<Vec<u8>> {
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::PreviewFrame;
            let response = handler.handle(request);
            match response {
                DaemonResponse::Frame { jpeg_data } => Ok(jpeg_data),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn preview_detect_frame(
        &self,
        user: &str,
    ) -> fdo::Result<(Vec<u8>, Vec<PreviewFaceInfo>)> {
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
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn list_devices(&self) -> fdo::Result<Vec<DeviceInfo>> {
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
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn release_camera(&self) -> fdo::Result<()> {
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ReleaseCamera;
            let response = handler.handle(request);
            match response {
                DaemonResponse::Ok => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!("unexpected response: {other:?}"))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    async fn ping(&self) -> fdo::Result<String> {
        Ok("pong".to_string())
    }

    async fn shutdown(&self) -> fdo::Result<()> {
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
                .unwrap_or_else(|_| "facelock_daemon=info,facelock_cli=info".into()),
        )
        .with_target(true)
        .init();

    info!("facelock daemon starting");

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

    // Camera factory for lazy opening
    let camera_factory: CameraFactory =
        Box::new(|config: &Config| Camera::open(&config.device).map_err(|e| e.to_string()));

    let handler: ProductionHandler = Handler::new(
        config, engine, store, rate_limiter, device_is_ir, Some(camera_factory),
    );

    let handler = Arc::new(Mutex::new(handler));

    // Build and run the tokio runtime for the D-Bus server
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(run_dbus_server(handler))
}

async fn run_dbus_server(handler: Arc<Mutex<ProductionHandler>>) -> anyhow::Result<()> {
    let service = FacelockService {
        handler: handler.clone(),
    };

    let _connection = zbus::connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service)?
        .build()
        .await?;

    info!("facelock daemon running on D-Bus system bus as {BUS_NAME}");

    // Spawn a background task to release the camera on system suspend.
    // Best-effort: if logind is unavailable, log a warning and continue.
    let handler_for_sleep = handler.clone();
    tokio::spawn(async move {
        if let Err(e) = watch_sleep_signals(handler_for_sleep).await {
            tracing::warn!("failed to watch logind sleep signals: {e}");
        }
    });

    // Wait for shutdown signal (SIGTERM or SIGINT)
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received SIGINT, shutting down");
        }
        _ = sigterm.recv() => {
            info!("received SIGTERM, shutting down");
        }
        _ = poll_shutdown(handler) => {
            info!("shutdown requested via D-Bus, shutting down");
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
async fn watch_sleep_signals(
    handler: Arc<Mutex<ProductionHandler>>,
) -> anyhow::Result<()> {
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
            let _ = tokio::task::spawn_blocking(move || {
                match handler.try_lock() {
                    Ok(mut h) => {
                        h.handle(DaemonRequest::ReleaseCamera);
                        info!("released camera for suspend");
                    }
                    Err(_) => {
                        warn!("could not release camera for suspend: handler busy");
                    }
                }
            })
            .await;
        } else {
            info!("resumed from suspend, camera will reacquire on demand");
        }
    }
    Ok(())
}

/// Poll the handler's shutdown_requested flag and idle camera release.
/// All mutex access goes through spawn_blocking to avoid blocking the
/// tokio runtime (which would deadlock D-Bus method dispatch).
async fn poll_shutdown(handler: Arc<Mutex<ProductionHandler>>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
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
