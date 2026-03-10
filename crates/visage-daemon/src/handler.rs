use std::time::{Duration, Instant};

use visage_core::config::Config;
use visage_core::ipc::{DaemonRequest, DaemonResponse, PreviewFace};
use visage_core::traits::{CameraSource, FaceProcessor};
use visage_core::types::best_match;
use visage_store::FaceStore;
use image::codecs::jpeg::JpegEncoder;
use tracing::{debug, info};

use crate::auth;
use crate::enroll;
use crate::rate_limit::RateLimiter;

/// Type alias for the camera factory closure.
type CameraFactory<C> = Box<dyn Fn(&Config) -> Result<C, String>>;

const CAMERA_DEBOUNCE: Duration = Duration::from_secs(10);
const JPEG_BUF_CAPACITY: usize = 128 * 1024;

pub struct Handler<C: CameraSource, E: FaceProcessor> {
    pub config: Config,
    pub engine: E,
    pub store: FaceStore,
    pub rate_limiter: RateLimiter,
    pub device_is_ir: bool,
    pub shutdown_requested: bool,
    camera: Option<C>,
    camera_factory: Option<CameraFactory<C>>,
    camera_last_used: Instant,
    jpeg_buf: Vec<u8>,
}

impl<C: CameraSource, E: FaceProcessor> Handler<C, E> {
    pub fn new(
        config: Config,
        engine: E,
        store: FaceStore,
        rate_limiter: RateLimiter,
        device_is_ir: bool,
        camera_factory: Option<CameraFactory<C>>,
    ) -> Self {
        Self {
            config,
            engine,
            store,
            rate_limiter,
            device_is_ir,
            shutdown_requested: false,
            camera: None,
            camera_factory,
            camera_last_used: Instant::now(),
            jpeg_buf: Vec::with_capacity(JPEG_BUF_CAPACITY),
        }
    }

    pub fn maybe_release_camera(&mut self) {
        if self.camera.is_some() && self.camera_last_used.elapsed() > CAMERA_DEBOUNCE {
            debug!("releasing camera (debounce)");
            self.camera = None;
        }
    }

    fn acquire_camera(&mut self) -> Result<(), DaemonResponse> {
        if self.camera.is_none() {
            debug!("opening camera");
            if let Some(ref factory) = self.camera_factory {
                let cam = factory(&self.config).map_err(|e| DaemonResponse::Error {
                    message: format!("failed to open camera: {e}"),
                })?;
                self.camera = Some(cam);
            } else {
                return Err(DaemonResponse::Error {
                    message: "no camera available".into(),
                });
            }
        }
        self.camera_last_used = Instant::now();
        Ok(())
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

                if let Err(resp) = self.acquire_camera() {
                    return resp;
                }

                // Split borrows: take camera out, run auth, put it back
                let mut camera = self.camera.take().unwrap();
                let result = auth::authenticate(
                    &mut camera,
                    &mut self.engine,
                    &self.store,
                    &self.config,
                    &user,
                );
                // Release camera after auth (one-shot operation)
                drop(camera);
                result
            }

            DaemonRequest::Enroll { user, label } => {
                if let Err(resp) = self.acquire_camera() {
                    return resp;
                }

                let mut camera = self.camera.take().unwrap();
                let result =
                    enroll::enroll(&mut camera, &mut self.engine, &self.store, &self.config, &user, &label);
                drop(camera);
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

            DaemonRequest::ListDevices => DaemonResponse::Devices(Vec::new()),

            DaemonRequest::PreviewFrame => {
                if let Err(resp) = self.acquire_camera() {
                    return resp;
                }
                let camera = self.camera.as_mut().unwrap();
                match camera.capture_rgb_only() {
                    Ok(frame) => self.encode_frame_response(&frame.rgb, frame.width, frame.height),
                    Err(e) => DaemonResponse::Error {
                        message: format!("capture error: {e}"),
                    },
                }
            }

            DaemonRequest::PreviewDetectFrame { user } => {
                if let Err(resp) = self.acquire_camera() {
                    return resp;
                }
                // Take camera for split borrow, put back after
                let mut camera = self.camera.take().unwrap();
                let result = match camera.capture() {
                    Ok(frame) => {
                        let faces = self.detect_and_match(&frame, &user);
                        self.jpeg_buf.clear();
                        let mut encoder =
                            JpegEncoder::new_with_quality(&mut self.jpeg_buf, 60);
                        match encoder.encode(
                            &frame.rgb,
                            frame.width,
                            frame.height,
                            image::ExtendedColorType::Rgb8,
                        ) {
                            Ok(()) => DaemonResponse::DetectFrame {
                                jpeg_data: std::mem::take(&mut self.jpeg_buf),
                                faces,
                            },
                            Err(e) => DaemonResponse::Error {
                                message: format!("JPEG encode error: {e}"),
                            },
                        }
                    }
                    Err(e) => DaemonResponse::Error {
                        message: format!("capture error: {e}"),
                    },
                };
                self.camera = Some(camera);
                result
            }
        }
    }

    fn encode_frame_response(&mut self, rgb: &[u8], width: u32, height: u32) -> DaemonResponse {
        self.jpeg_buf.clear();
        let mut encoder = JpegEncoder::new_with_quality(&mut self.jpeg_buf, 60);
        match encoder.encode(rgb, width, height, image::ExtendedColorType::Rgb8) {
            Ok(()) => DaemonResponse::Frame {
                jpeg_data: std::mem::take(&mut self.jpeg_buf),
            },
            Err(e) => DaemonResponse::Error {
                message: format!("JPEG encode error: {e}"),
            },
        }
    }

    fn detect_and_match(
        &mut self,
        frame: &visage_core::types::Frame,
        user: &str,
    ) -> Vec<PreviewFace> {
        let detections = match self.engine.process(frame) {
            Ok(d) => d,
            Err(e) => {
                debug!("face engine error during preview: {e}");
                return Vec::new();
            }
        };

        let stored = self.store.get_user_embeddings(user).unwrap_or_default();
        let threshold = self.config.recognition.threshold;

        detections
            .into_iter()
            .map(|(det, embedding)| {
                let (best_sim, _) = best_match(&embedding, &stored);
                PreviewFace {
                    x: det.bbox.x,
                    y: det.bbox.y,
                    width: det.bbox.width,
                    height: det.bbox.height,
                    confidence: det.confidence,
                    similarity: best_sim,
                    recognized: best_sim >= threshold,
                }
            })
            .collect()
    }
}
