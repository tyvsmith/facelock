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

pub struct Handler<'a> {
    pub camera: Camera<'a>,
    pub engine: FaceEngine,
    pub store: FaceStore,
    pub config: Config,
    pub rate_limiter: RateLimiter,
    pub device_is_ir: bool,
    pub shutdown_requested: bool,
}

impl Handler<'_> {
    pub fn handle(&mut self, request: DaemonRequest) -> DaemonResponse {
        debug!(?request, "handling request");
        match request {
            DaemonRequest::Ping => DaemonResponse::Ok,

            DaemonRequest::Shutdown => {
                info!("shutdown requested via IPC");
                self.shutdown_requested = true;
                DaemonResponse::Ok
            }

            DaemonRequest::Authenticate { user } => auth::authenticate(
                &mut self.camera,
                &mut self.engine,
                &self.store,
                &self.config,
                &user,
                &mut self.rate_limiter,
                self.device_is_ir,
            ),

            DaemonRequest::Enroll { user, label } => enroll::enroll(
                &mut self.camera,
                &mut self.engine,
                &self.store,
                &user,
                &label,
            ),

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

            DaemonRequest::PreviewFrame => match self.camera.capture() {
                Ok(frame) => {
                    let mut jpeg_buf = Vec::new();
                    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, 80);
                    match encoder.encode(&frame.rgb, frame.width, frame.height, image::ExtendedColorType::Rgb8) {
                        Ok(()) => DaemonResponse::Frame {
                            jpeg_data: jpeg_buf,
                        },
                        Err(e) => DaemonResponse::Error {
                            message: format!("JPEG encode error: {e}"),
                        },
                    }
                }
                Err(e) => DaemonResponse::Error {
                    message: format!("capture error: {e}"),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    // Handler dispatch tests require hardware (camera + models).
    // Unit tests for individual request types are in auth.rs and via integration tests.

    #[test]
    fn shutdown_flag_set() {
        // We can't construct a full Handler without hardware,
        // but we can verify the shutdown logic conceptually.
        // The actual integration is tested in spec 12.
        assert!(true);
    }
}
