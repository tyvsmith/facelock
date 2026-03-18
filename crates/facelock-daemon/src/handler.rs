use std::time::{Duration, Instant};

use facelock_core::config::{Config, EncryptionMethod};
use facelock_core::ipc::{DaemonRequest, DaemonResponse, PreviewFace};
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_core::types::best_match;
use facelock_store::FaceStore;
use image::codecs::jpeg::JpegEncoder;
use tracing::{debug, info, warn};

use crate::audit::{self, AuditEntry};
use crate::auth;
use crate::enroll;
use crate::rate_limit::RateLimiter;

/// Type alias for the camera factory closure.
type CameraFactory<C> = Box<dyn Fn(&Config) -> Result<C, String> + Send + Sync>;

/// Fallback camera release delay when config value is 0 (shouldn't happen with default).
const CAMERA_DEBOUNCE_FALLBACK: Duration = Duration::from_secs(5);
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
    /// Quirk-overridden warmup frames (takes precedence over config if `Some`).
    warmup_frames_override: Option<u32>,
    #[cfg(feature = "tpm")]
    tpm_sealer: Option<facelock_tpm::TpmSealer>,
    software_sealer: Option<facelock_tpm::SoftwareSealer>,
}

impl<C: CameraSource, E: FaceProcessor> Handler<C, E> {
    pub fn new(
        config: Config,
        engine: E,
        store: FaceStore,
        rate_limiter: RateLimiter,
        device_is_ir: bool,
        camera_factory: Option<CameraFactory<C>>,
        warmup_frames_override: Option<u32>,
    ) -> Result<Self, String> {
        #[cfg(feature = "tpm")]
        let tpm_sealer = if config.tpm.seal_database {
            match facelock_tpm::TpmSealer::new(&config.tpm.tcti) {
                Ok(sealer) => {
                    info!("TPM sealer initialized for seal_database");
                    Some(sealer)
                }
                Err(e) => {
                    warn!("failed to initialize TPM sealer: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Initialize software sealer based on encryption method
        let software_sealer = match config.encryption.method {
            EncryptionMethod::Keyfile => {
                let key_path = std::path::Path::new(&config.encryption.key_path);
                match facelock_tpm::SoftwareSealer::from_key_file(key_path) {
                    Ok(sealer) => {
                        info!(
                            "software encryption sealer initialized from {}",
                            key_path.display()
                        );
                        Some(sealer)
                    }
                    Err(e) => {
                        warn!("failed to initialize software encryption sealer: {e}");
                        None
                    }
                }
            }
            EncryptionMethod::Tpm => {
                #[cfg(feature = "tpm")]
                {
                    let sealed_path = std::path::Path::new(&config.encryption.sealed_key_path);
                    let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                        .map_err(|e| format!("TPM initialization failed: {e}"))?;
                    let key = tpm.unseal_key_from_file(sealed_path).map_err(|e| {
                        format!(
                            "failed to unseal AES key from {}: {e}",
                            sealed_path.display()
                        )
                    })?;
                    info!("AES key unsealed from TPM ({})", sealed_path.display());
                    Some(facelock_tpm::SoftwareSealer::from_key(key))
                }
                #[cfg(not(feature = "tpm"))]
                {
                    return Err(
                        "encryption method is 'tpm' but TPM support is not compiled in \
                         (rebuild with --features tpm)"
                            .into(),
                    );
                }
            }
            EncryptionMethod::None => None,
        };

        Ok(Self {
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
            warmup_frames_override,
            #[cfg(feature = "tpm")]
            tpm_sealer,
            software_sealer,
        })
    }

    pub fn maybe_release_camera(&mut self) {
        let debounce = if self.config.device.camera_release_secs > 0 {
            Duration::from_secs(self.config.device.camera_release_secs as u64)
        } else {
            CAMERA_DEBOUNCE_FALLBACK
        };
        if self.camera.is_some() && self.camera_last_used.elapsed() > debounce {
            debug!("releasing camera (debounce)");
            self.camera = None;
        }
    }

    fn acquire_camera(&mut self) -> Result<(), DaemonResponse> {
        if self.camera.is_none() {
            debug!("opening camera");
            if let Some(ref factory) = self.camera_factory {
                let mut cam = factory(&self.config).map_err(|e| DaemonResponse::Error {
                    message: format!("failed to open camera: {e}"),
                })?;
                // Discard warmup frames for AGC/AE stabilization.
                // Quirk override takes precedence over config value.
                let warmup = self
                    .warmup_frames_override
                    .unwrap_or(self.config.device.warmup_frames);
                if warmup > 0 {
                    debug!(warmup, "discarding warmup frames");
                    for _ in 0..warmup {
                        let _ = cam.capture();
                    }
                }
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

    /// Load user embeddings, decrypting TPM-sealed or software-encrypted blobs.
    /// Falls back to the standard `get_user_embeddings` path when no encryption is active.
    fn load_user_embeddings(
        &mut self,
        user: &str,
    ) -> Result<Vec<(u32, facelock_core::types::FaceEmbedding)>, DaemonResponse> {
        // Check if any encryption is configured that requires raw blob handling
        let needs_raw = self.software_sealer.is_some();
        #[cfg(feature = "tpm")]
        let needs_raw = needs_raw || self.tpm_sealer.is_some();

        if !needs_raw {
            // Fast path: no encryption, use standard method (no overhead)
            return self
                .store
                .get_user_embeddings(user)
                .map_err(|e| DaemonResponse::Error {
                    message: format!("storage error: {e}"),
                });
        }

        // Slow path: load raw blobs and decrypt as needed
        let raw_rows =
            self.store
                .get_user_embeddings_raw(user)
                .map_err(|e| DaemonResponse::Error {
                    message: format!("storage error: {e}"),
                })?;

        let mut results = Vec::with_capacity(raw_rows.len());
        for (id, blob, sealed) in &raw_rows {
            let embedding = if *sealed && facelock_tpm::is_software_encrypted(blob) {
                // Software-encrypted (version byte 0x02)
                let sealer =
                    self.software_sealer
                        .as_ref()
                        .ok_or_else(|| DaemonResponse::Error {
                            message: format!(
                                "embedding {id} is software-encrypted but no key is configured"
                            ),
                        })?;
                sealer
                    .unseal_embedding(blob)
                    .map_err(|e| DaemonResponse::Error {
                        message: format!("software decryption failed for embedding {id}: {e}"),
                    })?
            } else if *sealed {
                // TPM-sealed (version byte 0x01)
                #[cfg(feature = "tpm")]
                {
                    let sealer = self
                        .tpm_sealer
                        .as_mut()
                        .ok_or_else(|| DaemonResponse::Error {
                            message: "TPM-sealed embeddings exist but TPM is not available".into(),
                        })?;
                    sealer
                        .unseal_embedding(blob)
                        .map_err(|e| DaemonResponse::Error {
                            message: format!("TPM unseal failed for embedding {id}: {e}"),
                        })?
                }
                #[cfg(not(feature = "tpm"))]
                {
                    return Err(DaemonResponse::Error {
                        message: format!(
                            "embedding {id} is TPM-sealed but TPM support is not compiled in"
                        ),
                    });
                }
            } else {
                // Plaintext raw embedding
                if blob.len() != 512 * 4 {
                    return Err(DaemonResponse::Error {
                        message: format!(
                            "invalid raw embedding size for id {id}: expected {} bytes, got {}",
                            512 * 4,
                            blob.len()
                        ),
                    });
                }
                let floats: &[f32] = bytemuck::cast_slice(blob);
                let mut emb = [0f32; 512];
                emb.copy_from_slice(floats);
                emb
            };

            results.push((*id, embedding));
        }
        Ok(results)
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
                    let (result, error) = match &resp {
                        DaemonResponse::Error { message } if message.contains("rate limited") => {
                            ("rate_limited".to_string(), Some(message.clone()))
                        }
                        DaemonResponse::Error { message } => {
                            ("error".to_string(), Some(message.clone()))
                        }
                        DaemonResponse::AuthResult(mr) if !mr.matched => {
                            ("failure".to_string(), None)
                        }
                        DaemonResponse::Suppressed => ("suppressed".to_string(), None),
                        _ => ("error".to_string(), None),
                    };
                    audit::write_audit_entry(
                        &self.config.audit,
                        &AuditEntry {
                            timestamp: audit::now_iso8601(),
                            user: user.clone(),
                            result,
                            similarity: None,
                            frame_count: None,
                            duration_ms: None,
                            device: self.config.device.path.clone(),
                            model_label: None,
                            error,
                        },
                    );
                    return resp;
                }

                if let Err(resp) = self.acquire_camera() {
                    return resp;
                }

                // Pre-load and decrypt embeddings (handles TPM + software encryption)
                let stored = match self.load_user_embeddings(&user) {
                    Ok(s) => s,
                    Err(resp) => return resp,
                };

                // Split borrows: take camera out, run auth, put it back
                let mut camera = self.camera.take().unwrap();
                let models = self.store.list_models(&user).unwrap_or_default();
                let result = auth::authenticate_with_embeddings(
                    &mut camera,
                    &mut self.engine,
                    &stored,
                    &models,
                    &self.config,
                    &user,
                    self.device_is_ir,
                );
                self.camera = Some(camera);
                self.camera_last_used = Instant::now();
                // Only failed auths count against the rate limit
                if let DaemonResponse::AuthResult(ref mr) = result {
                    if !mr.matched {
                        self.rate_limiter.record_failure(&user);
                    }
                }
                result
            }

            DaemonRequest::Enroll { user, label } => {
                if let Err(resp) = self.acquire_camera() {
                    return resp;
                }

                let mut camera = self.camera.take().unwrap();
                let result = enroll::enroll(
                    &mut camera,
                    &mut self.engine,
                    &self.store,
                    &self.config,
                    &user,
                    &label,
                    self.software_sealer.as_ref(),
                );
                self.camera = Some(camera);
                self.camera_last_used = Instant::now();
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

            DaemonRequest::ListDevices => {
                use facelock_camera::{is_ir_camera, list_devices};
                match list_devices() {
                    Ok(devices) => DaemonResponse::Devices(
                        devices
                            .iter()
                            .map(|d| facelock_core::ipc::IpcDeviceInfo {
                                path: d.path.clone(),
                                name: d.name.clone(),
                                driver: d.driver.clone(),
                                is_ir: is_ir_camera(d),
                                formats: d
                                    .formats
                                    .iter()
                                    .map(|f| facelock_core::ipc::IpcFormatInfo {
                                        fourcc: f.fourcc.clone(),
                                        description: f.description.clone(),
                                        sizes: f.sizes.clone(),
                                    })
                                    .collect(),
                            })
                            .collect(),
                    ),
                    Err(e) => DaemonResponse::Error {
                        message: format!("device enumeration failed: {e}"),
                    },
                }
            }

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
                        let mut encoder = JpegEncoder::new_with_quality(&mut self.jpeg_buf, 60);
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
        frame: &facelock_core::types::Frame,
        user: &str,
    ) -> Vec<PreviewFace> {
        let detections = match self.engine.process(frame) {
            Ok(d) => d,
            Err(e) => {
                debug!("face engine error during preview: {e}");
                return Vec::new();
            }
        };

        let stored = self.load_user_embeddings(user).unwrap_or_default();
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
