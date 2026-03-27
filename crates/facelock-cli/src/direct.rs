//! Direct (daemonless) implementations of CLI operations.
//!
//! Used when daemon is unavailable on D-Bus or `daemon.mode = oneshot`.
//! Opens camera, loads models, and accesses the database directly.

use std::path::Path;

use anyhow::{Context, bail};
use facelock_camera::quirks::QuirksDb;
use facelock_camera::{
    Camera, DeviceInfo, auto_detect_device, is_ir_camera, is_ir_camera_with_quirks,
    list_devices, validate_device,
};
use facelock_core::config::DeviceConfig;
use facelock_core::config::{Config, EncryptionMethod};
use facelock_core::ipc::DaemonResponse;
use facelock_core::types::MatchResult;
use facelock_face::FaceEngine;
use facelock_store::FaceStore;
use tracing::debug;

pub fn open_store(config: &Config) -> anyhow::Result<FaceStore> {
    FaceStore::open(Path::new(&config.storage.db_path)).context("failed to open database")
}

#[derive(Clone)]
struct ResolvedCameraDevice {
    device: DeviceConfig,
    device_quirk: Option<facelock_camera::quirks::Quirk>,
    device_is_ir: bool,
}

struct OpenedCamera {
    camera: Camera<'static>,
    device_is_ir: bool,
}

fn build_resolved_camera_device(
    config: &Config,
    device_info: DeviceInfo,
    quirks: &QuirksDb,
) -> ResolvedCameraDevice {
    let mut device = config.device.clone();
    device.path = Some(device_info.path.clone());

    ResolvedCameraDevice {
        device,
        device_quirk: quirks.find_match(&device_info).cloned(),
        device_is_ir: is_ir_camera_with_quirks(&device_info, Some(quirks)),
    }
}

fn resolve_camera_device(config: &Config) -> anyhow::Result<ResolvedCameraDevice> {
    let quirks = QuirksDb::load();
    let device_info = match config.device.path.as_deref() {
        Some(path) => validate_device(path)
            .with_context(|| format!("failed to query configured camera {path}"))?,
        None => auto_detect_device().context("no camera device specified and auto-detection failed")?,
    };

    Ok(build_resolved_camera_device(config, device_info, &quirks))
}

fn open_camera_context(config: &Config) -> anyhow::Result<OpenedCamera> {
    let resolved = resolve_camera_device(config)?;
    let mut camera = Camera::open(&resolved.device, resolved.device_quirk.as_ref())
        .context("failed to open camera")?;

    // Discard warmup frames for AGC/AE stabilization.
    // Quirk override takes precedence over config value.
    let warmup = resolved
        .device_quirk
        .and_then(|q| q.warmup_frames)
        .unwrap_or(resolved.device.warmup_frames);
    if warmup > 0 {
        debug!(warmup, "discarding warmup frames");
        for _ in 0..warmup {
            let _ = camera.capture();
        }
    }

    Ok(OpenedCamera {
        camera,
        device_is_ir: resolved.device_is_ir,
    })
}

/// Open camera with quirks support and warmup frame discarding.
pub fn open_camera(config: &Config) -> anyhow::Result<Camera<'static>> {
    Ok(open_camera_context(config)?.camera)
}

pub fn load_engine(config: &Config) -> anyhow::Result<FaceEngine> {
    FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir))
        .context("failed to load face engine")
}

/// Direct authentication — returns true if matched.
pub fn authenticate(config: &Config, user: &str) -> anyhow::Result<bool> {
    let store = open_store(config)?;

    if !store.has_models(user).context("storage error")? {
        return Ok(false);
    }

    let OpenedCamera {
        mut camera,
        device_is_ir,
    } = open_camera_context(config)?;
    let mut engine = load_engine(config)?;

    // Load embeddings with encryption support, matching the daemon handler path.
    let stored = load_user_embeddings(&store, config, user)?;
    let models = store.list_models(user).unwrap_or_default();

    let response = facelock_daemon_auth::authenticate(
        &mut camera,
        &mut engine,
        &stored,
        &models,
        config,
        user,
        device_is_ir,
    );

    match response {
        DaemonResponse::AuthResult(MatchResult { matched, .. }) => Ok(matched),
        DaemonResponse::Error { message } => bail!("{message}"),
        _ => bail!("unexpected auth response"),
    }
}

/// Initialize a software sealer based on encryption config.
/// Returns `None` if encryption is disabled.
fn init_software_sealer(config: &Config) -> anyhow::Result<Option<facelock_tpm::SoftwareSealer>> {
    match config.encryption.method {
        EncryptionMethod::Keyfile => {
            let key_path = Path::new(&config.encryption.key_path);
            Ok(Some(
                facelock_tpm::SoftwareSealer::from_key_file(key_path)
                    .context("failed to initialize software encryption sealer")?,
            ))
        }
        EncryptionMethod::Tpm => {
            #[cfg(feature = "tpm")]
            {
                let sealed_path = Path::new(&config.encryption.sealed_key_path);
                let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                    .context("TPM initialization failed")?;
                let key = tpm.unseal_key_from_file(sealed_path).with_context(|| {
                    format!("failed to unseal AES key from {}", sealed_path.display())
                })?;
                Ok(Some(facelock_tpm::SoftwareSealer::from_key(key)))
            }
            #[cfg(not(feature = "tpm"))]
            {
                bail!(
                    "encryption method is 'tpm' but TPM support is not compiled in \
                     (rebuild with --features tpm)"
                );
            }
        }
        EncryptionMethod::None => Ok(None),
    }
}

/// Load user embeddings, decrypting software-encrypted or TPM-sealed blobs as needed.
/// Mirrors `Handler::load_user_embeddings` from the daemon path.
pub fn load_user_embeddings(
    store: &FaceStore,
    config: &Config,
    user: &str,
) -> anyhow::Result<Vec<(u32, facelock_core::types::FaceEmbedding)>> {
    let software_sealer = init_software_sealer(config)?;

    // Fast path: no encryption configured
    if software_sealer.is_none() {
        return store
            .get_user_embeddings(user)
            .context("storage error loading embeddings");
    }

    // Slow path: load raw blobs and decrypt
    let sealer = software_sealer.unwrap();
    let raw_rows = store
        .get_user_embeddings_raw(user)
        .context("storage error loading raw embeddings")?;

    let mut results = Vec::with_capacity(raw_rows.len());
    for (id, blob, sealed) in &raw_rows {
        let embedding = if *sealed && facelock_tpm::is_software_encrypted(blob) {
            sealer
                .unseal_embedding(blob)
                .with_context(|| format!("software decryption failed for embedding {id}"))?
        } else if *sealed {
            #[cfg(feature = "tpm")]
            {
                bail!(
                    "embedding {id} is TPM-sealed but direct path only supports software encryption — use the daemon"
                );
            }
            #[cfg(not(feature = "tpm"))]
            {
                bail!("embedding {id} is TPM-sealed but TPM support is not compiled in");
            }
        } else {
            // Plaintext raw embedding
            anyhow::ensure!(
                blob.len() == 512 * 4,
                "invalid raw embedding size for id {id}: expected {} bytes, got {}",
                512 * 4,
                blob.len()
            );
            let floats: &[f32] = bytemuck::cast_slice(blob);
            let mut emb = [0f32; 512];
            emb.copy_from_slice(floats);
            emb
        };
        results.push((*id, embedding));
    }
    Ok(results)
}

/// Direct enrollment — returns (model_id, embedding_count).
pub fn enroll(config: &Config, user: &str, label: &str) -> anyhow::Result<(u32, u32)> {
    let store = open_store(config)?;
    let mut camera = open_camera_context(config)?.camera;
    let mut engine = load_engine(config)?;

    // Initialize sealer if encryption is configured
    let software_sealer = init_software_sealer(config)?;

    let response = facelock_daemon_enroll::enroll(
        &mut camera,
        &mut engine,
        &store,
        config,
        user,
        label,
        software_sealer.as_ref(),
    );

    match response {
        DaemonResponse::Enrolled {
            model_id,
            embedding_count,
        } => Ok((model_id, embedding_count)),
        DaemonResponse::Error { message } => bail!("{message}"),
        _ => bail!("unexpected enroll response"),
    }
}

/// Direct device listing (no daemon needed).
pub fn list_devices_direct() -> anyhow::Result<()> {
    let devices = list_devices().context("failed to enumerate devices")?;

    if devices.is_empty() {
        println!("No video devices found.");
        return Ok(());
    }

    println!("Available video devices:\n");
    for dev in &devices {
        let ir_tag = if is_ir_camera(dev) { " [IR]" } else { "" };
        println!("  {}{ir_tag}", dev.path);
        println!("    Name:    {}", dev.name);
        println!("    Driver:  {}", dev.driver);

        if !dev.formats.is_empty() {
            println!("    Formats:");
            for fmt in &dev.formats {
                let sizes: Vec<String> =
                    fmt.sizes.iter().map(|(w, h)| format!("{w}x{h}")).collect();
                println!(
                    "      {} ({}) — {}",
                    fmt.fourcc.trim(),
                    fmt.description,
                    if sizes.is_empty() {
                        "no sizes reported".to_string()
                    } else {
                        sizes.join(", ")
                    }
                );
            }
        }
        println!();
    }

    Ok(())
}

// Bridge modules — the daemon's auth and enroll functions are generic over traits,
// and Camera/FaceEngine implement those traits. We reference them via extern crate
// since the daemon is a separate binary crate. Instead, we inline the module paths.
//
// Actually, since facelock-daemon is a binary crate, we can't import from it.
// The auth and enroll modules use types from facelock-core's traits, and the concrete
// Camera/FaceEngine implement those traits. We need to either:
// 1. Move the shared auth/enroll logic to a library crate
// 2. Keep local implementations
//
// For now, we keep lightweight wrappers that call the same underlying logic.
// The auth loop is implemented in terms of core types (CameraSource + FaceProcessor).

mod facelock_daemon_auth {
    use facelock_camera::is_dark_with_config;
    use facelock_camera::preprocess::check_ir_texture;
    use facelock_core::config::Config;
    use facelock_core::ipc::DaemonResponse;
    use facelock_core::traits::{CameraSource, FaceProcessor};
    use facelock_core::types::{
        FaceEmbedding, FaceModelInfo, MatchResult, best_match, check_frame_variance,
        zeroize_embedding, zeroize_stored_embeddings,
    };
    use facelock_daemon::liveness::LandmarkTracker;
    use std::time::Instant;
    use tracing::{debug, info, warn};

    pub fn authenticate<C: CameraSource, E: FaceProcessor>(
        camera: &mut C,
        engine: &mut E,
        stored: &[(u32, FaceEmbedding)],
        models: &[FaceModelInfo],
        config: &Config,
        user: &str,
        device_is_ir: bool,
    ) -> DaemonResponse {
        let start = Instant::now();

        // Make a mutable copy so we can zeroize on all exit paths
        let mut stored = stored.to_vec();

        let label_for = |id: u32| -> Option<String> {
            models.iter().find(|m| m.id == id).map(|m| m.label.clone())
        };

        let deadline =
            Instant::now() + std::time::Duration::from_secs(config.recognition.timeout_secs as u64);
        let threshold = config.recognition.threshold;
        let mut best_similarity: f32 = 0.0;
        let mut matched_frame_embeddings: Vec<FaceEmbedding> =
            Vec::with_capacity(config.security.min_auth_frames as usize);
        let mut dark_count: u32 = 0;
        let mut frame_count: u32 = 0;
        let mut best_model_id: Option<u32> = None;
        let mut landmark_tracker = LandmarkTracker::new(
            10,
            config.security.landmark_displacement_px,
            config.security.landmark_min_moving as usize,
        );

        while Instant::now() < deadline {
            let frame = match camera.capture() {
                Ok(f) => f,
                Err(e) => {
                    debug!("capture error: {e}");
                    continue;
                }
            };
            frame_count += 1;

            if is_dark_with_config(
                &frame,
                config.device.dark_threshold,
                config.device.dark_pixel_value,
            ) {
                dark_count += 1;
                continue;
            }

            let faces = match engine.process(&frame) {
                Ok(f) => f,
                Err(e) => {
                    debug!("face engine error: {e}");
                    continue;
                }
            };

            if faces.is_empty() {
                continue;
            }

            // Push landmarks from the first detected face for liveness tracking
            if let Some((det, _)) = faces.first() {
                landmark_tracker.push(det.landmarks);
            }

            // IR texture check: skip frames where all faces have flat texture
            if device_is_ir {
                let all_flat = faces
                    .iter()
                    .all(|(det, _)| !check_ir_texture(&frame.gray, &det.bbox, frame.width));
                if all_flat {
                    debug!(
                        frame = frame_count,
                        "IR texture check failed on all faces, skipping frame"
                    );
                    continue;
                }
            }

            let mut frame_matched = false;
            for (det, embedding) in &faces {
                // Skip individual faces that fail IR texture check
                if device_is_ir && !check_ir_texture(&frame.gray, &det.bbox, frame.width) {
                    debug!(
                        frame = frame_count,
                        "IR texture check failed for face, skipping"
                    );
                    continue;
                }
                let (frame_best_sim, frame_best_id) = best_match(embedding, &stored);
                if frame_best_sim > best_similarity {
                    best_similarity = frame_best_sim;
                    best_model_id = frame_best_id;
                }
                if frame_best_sim >= threshold && !frame_matched {
                    matched_frame_embeddings.push(*embedding);
                    frame_matched = true;
                }
            }

            // Frame variance check + landmark liveness check
            if config.security.require_frame_variance {
                if matched_frame_embeddings.len() >= config.security.min_auth_frames as usize
                    && check_frame_variance(&matched_frame_embeddings)
                {
                    // If landmark liveness is required, check it too
                    if config.security.require_landmark_liveness
                        && !landmark_tracker.check_liveness()
                    {
                        debug!(
                            frame = frame_count,
                            landmark_frames = landmark_tracker.frame_count(),
                            "landmark liveness not yet satisfied, continuing"
                        );
                        continue;
                    }

                    let duration = start.elapsed();
                    info!(
                        user,
                        similarity = format!("{best_similarity:.4}"),
                        frames = frame_count,
                        duration_ms = duration.as_millis() as u64,
                        "authentication succeeded"
                    );
                    let response = DaemonResponse::AuthResult(MatchResult {
                        matched: true,
                        model_id: best_model_id,
                        label: best_model_id.and_then(&label_for),
                        similarity: best_similarity,
                    });
                    // Zeroize sensitive embedding data before returning
                    zeroize_stored_embeddings(&mut stored);
                    for emb in &mut matched_frame_embeddings {
                        zeroize_embedding(emb);
                    }
                    return response;
                }
            } else if best_similarity >= threshold {
                // If landmark liveness is required, check it even without variance
                if config.security.require_landmark_liveness && !landmark_tracker.check_liveness() {
                    debug!(
                        frame = frame_count,
                        landmark_frames = landmark_tracker.frame_count(),
                        "landmark liveness not yet satisfied, continuing"
                    );
                    continue;
                }

                let duration = start.elapsed();
                info!(
                    user,
                    similarity = format!("{best_similarity:.4}"),
                    frames = frame_count,
                    duration_ms = duration.as_millis() as u64,
                    "authentication succeeded"
                );
                let response = DaemonResponse::AuthResult(MatchResult {
                    matched: true,
                    model_id: best_model_id,
                    label: best_model_id.and_then(&label_for),
                    similarity: best_similarity,
                });
                // Zeroize sensitive embedding data before returning
                zeroize_stored_embeddings(&mut stored);
                for emb in &mut matched_frame_embeddings {
                    zeroize_embedding(emb);
                }
                return response;
            }
        }

        // Zeroize sensitive embedding data before returning on failure/timeout path
        zeroize_stored_embeddings(&mut stored);
        for emb in &mut matched_frame_embeddings {
            zeroize_embedding(emb);
        }

        let duration = start.elapsed();
        if dark_count == frame_count && frame_count > 0 {
            warn!(user, frames = frame_count, "all frames were dark");
            return DaemonResponse::Error {
                message: "all frames dark".into(),
            };
        }

        info!(
            user,
            similarity = format!("{best_similarity:.4}"),
            frames = frame_count,
            duration_ms = duration.as_millis() as u64,
            "authentication failed"
        );
        DaemonResponse::AuthResult(MatchResult {
            matched: false,
            model_id: None,
            label: None,
            similarity: best_similarity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facelock_camera::FormatInfo;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_config() -> Config {
        Config::parse(
            r#"
[device]
warmup_frames = 2
"#,
        )
        .unwrap()
    }

    fn make_device(path: &str, name: &str, fourcc: &str) -> DeviceInfo {
        DeviceInfo {
            path: path.to_string(),
            name: name.to_string(),
            driver: "uvcvideo".to_string(),
            capabilities: vec!["VIDEO_CAPTURE".to_string()],
            formats: vec![FormatInfo {
                fourcc: fourcc.to_string(),
                description: "Test".to_string(),
                sizes: vec![(640, 480)],
            }],
        }
    }

    fn write_quirks_dir(contents: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("facelock-direct-tests-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("test.toml"), contents).unwrap();
        dir
    }

    #[test]
    fn auto_detected_device_inherits_quirk_state() {
        let config = test_config();
        let device = make_device("/dev/video2", "Logitech BRIO IR", "MJPG");
        let dir = write_quirks_dir(
            r#"
[[quirk]]
name_pattern = "(?i)brio.*ir"
force_ir = true
warmup_frames = 9
"#,
        );
        let mut quirks = QuirksDb::default();
        quirks.load_dir(&dir);

        let resolved = build_resolved_camera_device(&config, device, &quirks);

        assert_eq!(resolved.device.path.as_deref(), Some("/dev/video2"));
        assert!(resolved.device_is_ir);
        assert_eq!(
            resolved.device_quirk.as_ref().and_then(|q| q.warmup_frames),
            Some(9)
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unmatched_device_keeps_default_warmup_and_non_ir_state() {
        let config = test_config();
        let device = make_device("/dev/video3", "USB Camera", "MJPG");
        let quirks = QuirksDb::default();

        let resolved = build_resolved_camera_device(&config, device, &quirks);

        assert_eq!(resolved.device.path.as_deref(), Some("/dev/video3"));
        assert_eq!(resolved.device.warmup_frames, 2);
        assert!(!resolved.device_is_ir);
        assert!(resolved.device_quirk.is_none());
    }
}

mod facelock_daemon_enroll {
    use facelock_camera::is_dark_with_config;
    use facelock_core::config::Config;
    use facelock_core::ipc::DaemonResponse;
    use facelock_core::traits::{CameraSource, FaceProcessor};
    use facelock_store::FaceStore;
    use facelock_tpm::SoftwareSealer;
    use std::time::{Duration, Instant};
    use tracing::{debug, info, warn};

    const MIN_CAPTURES: usize = 3;
    const MAX_CAPTURES: usize = 10;
    const INTER_FRAME_DELAY: Duration = Duration::from_millis(200);

    pub fn enroll<C: CameraSource, E: FaceProcessor>(
        camera: &mut C,
        engine: &mut E,
        store: &FaceStore,
        config: &Config,
        user: &str,
        label: &str,
        sealer: Option<&SoftwareSealer>,
    ) -> DaemonResponse {
        match store.remove_model_by_label(user, label) {
            Ok(true) => info!(user, label, "removed existing model for re-enrollment"),
            Ok(false) => {}
            Err(e) => {
                return DaemonResponse::Error {
                    message: format!("storage error clearing old model: {e}"),
                };
            }
        }

        let enroll_secs = (config.recognition.timeout_secs as u64).max(5) * 3;
        let deadline = Instant::now() + Duration::from_secs(enroll_secs);
        let mut stored_count: u32 = 0;
        let mut model_id: Option<u32> = None;
        let mut last_capture = Instant::now() - INTER_FRAME_DELAY;

        while Instant::now() < deadline && (stored_count as usize) < MAX_CAPTURES {
            let since_last = Instant::now().duration_since(last_capture);
            if since_last < INTER_FRAME_DELAY {
                std::thread::sleep(INTER_FRAME_DELAY - since_last);
            }

            let frame = match camera.capture() {
                Ok(f) => f,
                Err(e) => {
                    debug!("capture error: {e}");
                    continue;
                }
            };

            if is_dark_with_config(
                &frame,
                config.device.dark_threshold,
                config.device.dark_pixel_value,
            ) {
                continue;
            }

            let faces = match engine.process(&frame) {
                Ok(f) => f,
                Err(e) => {
                    warn!("face engine error: {e}");
                    continue;
                }
            };

            if faces.is_empty() || faces.len() > 1 {
                continue;
            }

            let (_det, embedding) = &faces[0];

            // When a sealer is provided, encrypt each embedding before storage.
            let store_result = if let Some(sealer) = sealer {
                match sealer.seal_embedding(embedding) {
                    Ok(encrypted) => match model_id {
                        None => store
                            .add_model_raw(
                                user,
                                label,
                                &encrypted,
                                true,
                                &config.recognition.embedder_model,
                            )
                            .map(Some),
                        Some(id) => store.add_embedding_raw(id, &encrypted, true).map(|()| None),
                    },
                    Err(e) => {
                        warn!("failed to encrypt embedding: {e}");
                        return DaemonResponse::Error {
                            message: format!("encryption error: {e}"),
                        };
                    }
                }
            } else {
                match model_id {
                    None => store
                        .add_model(user, label, embedding, &config.recognition.embedder_model)
                        .map(Some),
                    Some(id) => store.add_embedding(id, embedding).map(|()| None),
                }
            };

            match store_result {
                Ok(Some(id)) => {
                    model_id = Some(id);
                    stored_count += 1;
                    info!(model_id = id, encrypted = sealer.is_some(), "created model");
                }
                Ok(None) => {
                    stored_count += 1;
                }
                Err(e) => {
                    if model_id.is_none() {
                        return DaemonResponse::Error {
                            message: format!("storage error: {e}"),
                        };
                    } else {
                        warn!("failed to store embedding: {e}");
                    }
                }
            }
            last_capture = Instant::now();
        }

        if stored_count < MIN_CAPTURES as u32 {
            return DaemonResponse::Error {
                message: format!(
                    "only captured {stored_count} frames, need at least {MIN_CAPTURES}"
                ),
            };
        }

        info!(
            user,
            label,
            model_id = model_id.unwrap_or(0),
            embedding_count = stored_count,
            encrypted = sealer.is_some(),
            "enrollment complete"
        );
        DaemonResponse::Enrolled {
            model_id: model_id.unwrap_or(0),
            embedding_count: stored_count,
        }
    }
}
