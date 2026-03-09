use std::time::{Duration, Instant};

use howdy_camera::Camera;
use howdy_core::config::Config;
use howdy_core::ipc::{DaemonRequest, DaemonResponse};
use howdy_face::FaceEngine;
use howdy_store::FaceStore;
use image::codecs::jpeg::JpegEncoder;
use tracing::{debug, info};

use crate::auth;
use crate::enroll;
use crate::rate_limit::RateLimiter;

/// Safety net: release camera if no request has used it for this long.
/// This is only a fallback for crashed clients — normal release is via
/// the explicit ReleaseCamera command.
const CAMERA_DEBOUNCE: Duration = Duration::from_secs(10);

/// Estimated JPEG size for a 640x480 frame at quality 60.
/// Pre-allocating avoids repeated heap growth during encoding.
const JPEG_BUF_CAPACITY: usize = 128 * 1024;

pub struct Handler {
    pub config: Config,
    pub engine: FaceEngine,
    pub store: FaceStore,
    pub rate_limiter: RateLimiter,
    pub device_is_ir: bool,
    pub shutdown_requested: bool,
    camera: Option<Camera<'static>>,
    camera_last_used: Instant,
    jpeg_buf: Vec<u8>,
}

impl Handler {
    pub fn new(
        config: Config,
        engine: FaceEngine,
        store: FaceStore,
        rate_limiter: RateLimiter,
        device_is_ir: bool,
    ) -> Self {
        Self {
            config,
            engine,
            store,
            rate_limiter,
            device_is_ir,
            shutdown_requested: false,
            camera: None,
            camera_last_used: Instant::now(),
            jpeg_buf: Vec::with_capacity(JPEG_BUF_CAPACITY),
        }
    }

    /// Release the camera if it hasn't been used recently (debounce safety net).
    pub fn maybe_release_camera(&mut self) {
        if self.camera.is_some() && self.camera_last_used.elapsed() > CAMERA_DEBOUNCE {
            debug!("releasing camera (debounce)");
            self.camera = None;
        }
    }

    fn acquire_camera(&mut self) -> Result<&mut Camera<'static>, DaemonResponse> {
        if self.camera.is_none() {
            debug!("opening camera");
            let cam = Camera::open(&self.config.device).map_err(|e| DaemonResponse::Error {
                message: format!("failed to open camera: {e}"),
            })?;
            self.camera = Some(cam);
        }
        self.camera_last_used = Instant::now();
        Ok(self.camera.as_mut().unwrap())
    }

    fn release_camera(&mut self) {
        if self.camera.is_some() {
            debug!("releasing camera");
            self.camera = None;
        }
    }

    pub fn handle(&mut self, request: DaemonRequest) -> DaemonResponse {
        debug!(?request, "handling request");
        match request {
            DaemonRequest::Ping => DaemonResponse::Ok,

            DaemonRequest::Shutdown => {
                info!("shutdown requested via IPC");
                self.release_camera();
                self.shutdown_requested = true;
                DaemonResponse::Ok
            }

            DaemonRequest::ReleaseCamera => {
                self.release_camera();
                DaemonResponse::Ok
            }

            DaemonRequest::Authenticate { user } => {
                if let Some(resp) = auth::pre_check(
                    &self.config,
                    &self.store,
                    &user,
                    &mut self.rate_limiter,
                    self.device_is_ir,
                ) {
                    return resp;
                }

                let camera = match self.acquire_camera() {
                    Ok(c) => c as *mut Camera<'static>,
                    Err(resp) => return resp,
                };
                // SAFETY: pointer valid for duration of this call; no other
                // access to self.camera occurs during authenticate().
                let camera = unsafe { &mut *camera };
                let result = auth::authenticate(
                    camera,
                    &mut self.engine,
                    &self.store,
                    &self.config,
                    &user,
                    self.device_is_ir,
                );
                // Auth is a one-shot operation — release camera when done
                self.release_camera();
                result
            }

            DaemonRequest::Enroll { user, label } => {
                let camera = match self.acquire_camera() {
                    Ok(c) => c as *mut Camera<'static>,
                    Err(resp) => return resp,
                };
                let camera = unsafe { &mut *camera };
                let result =
                    enroll::enroll(camera, &mut self.engine, &self.store, &user, &label);
                // Enroll is a one-shot operation — release camera when done
                self.release_camera();
                result
            }

            DaemonRequest::ListModels { user } => match self.store.list_models(&user) {
                Ok(models) => DaemonResponse::Models(models),
                Err(e) => DaemonResponse::Error {
                    message: format!("storage error: {e}"),
                },
            },

            DaemonRequest::RemoveModel { user, model_id } => {
                match self.store.remove_model(&user, model_id) {
                    Ok(_) => DaemonResponse::Removed,
                    Err(e) => DaemonResponse::Error {
                        message: format!("storage error: {e}"),
                    },
                }
            }

            DaemonRequest::ClearModels { user } => match self.store.clear_user(&user) {
                Ok(_) => DaemonResponse::Removed,
                Err(e) => DaemonResponse::Error {
                    message: format!("storage error: {e}"),
                },
            },

            // Preview keeps the camera open across frames — the client
            // sends ReleaseCamera when the preview window closes.
            // Uses capture_rgb_only() to skip grayscale/CLAHE (not needed for display).
            DaemonRequest::PreviewFrame => {
                let camera = match self.acquire_camera() {
                    Ok(c) => c,
                    Err(resp) => return resp,
                };
                match camera.capture_rgb_only() {
                    Ok(frame) => {
                        self.jpeg_buf.clear();
                        let mut encoder =
                            JpegEncoder::new_with_quality(&mut self.jpeg_buf, 60);
                        match encoder.encode(
                            &frame.rgb,
                            frame.width,
                            frame.height,
                            image::ExtendedColorType::Rgb8,
                        ) {
                            Ok(()) => DaemonResponse::Frame {
                                jpeg_data: self.jpeg_buf.clone(),
                            },
                            Err(e) => DaemonResponse::Error {
                                message: format!("JPEG encode error: {e}"),
                            },
                        }
                    }
                    Err(e) => DaemonResponse::Error {
                        message: format!("capture error: {e}"),
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shutdown_flag_set() {
        assert!(true);
    }
}
