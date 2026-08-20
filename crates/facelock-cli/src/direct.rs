//! Direct (daemonless) implementations of CLI operations.
//!
//! Used when daemon is unavailable on D-Bus or `daemon.mode = oneshot`.
//! Opens camera, loads models, and accesses the database directly.

use std::path::Path;

use anyhow::{Context, bail};
use facelock_camera::quirks::QuirksDb;
use facelock_camera::{Camera, ResolvedCamera, auto_detect_device, list_devices, validate_device};
use facelock_core::config::{Config, EncryptionMethod};
use facelock_core::types::MatchResult;
use facelock_daemon::audit::AuditSource;
use facelock_daemon::auth::AuthOutcome;
use facelock_daemon::auth::{PreCheckContext, pre_check_audited_with_context};
use facelock_daemon::cancel::CancelToken;
use facelock_daemon::enroll::EnrollOutcome;
use facelock_daemon::rate_limit::RateLimiter;
use facelock_face::FaceEngine;
use facelock_store::{FaceStore, StoreError};
use tracing::debug;

/// Open the face database, applying the state-directory layout first.
///
/// This is the single choke point for every direct-mode store access —
/// `enroll`, `list`, `remove`, `clear`, `test`, `preview` and the marker
/// refresh all arrive here — which is why the layout hook lives at this layer
/// rather than being repeated at each command. `daemon`, `auth` and `setup`
/// keep their own explicit calls: they are cheap, and they document that those
/// three must not depend on some later store access to fix the modes for them.
///
/// A layout failure only means modes could not be set — it cannot change what
/// is read — so it is logged rather than blocking the caller; unprivileged
/// callers hit it constantly.
pub fn open_store(config: &Config) -> anyhow::Result<FaceStore> {
    crate::state_layout::ensure_state_layout_best_effort(config);

    // `create`: first-time `enroll` legitimately brings the database into
    // being here.
    FaceStore::create(Path::new(&config.storage.db_path)).context("failed to open database")
}

/// Like [`open_store`], but never creates: [`StoreError::Absent`] comes back
/// as a value instead of an empty database materializing at the path.
///
/// This is the constructor for guards and reporters — the call shapes that
/// need "fresh install" and "cannot read" to be different answers. The typed
/// error is the point: callers match `Absent` to proceed on "nothing
/// enrolled" and treat every other class as "cannot tell", without sniffing
/// message strings.
pub fn open_store_existing(config: &Config) -> Result<FaceStore, StoreError> {
    crate::state_layout::ensure_state_layout_best_effort(config);
    FaceStore::open_existing(Path::new(&config.storage.db_path))
}

/// Resolve and interrogate the configured (or auto-detected) camera device
/// without opening it. The returned [`ResolvedCamera`] carries the caps
/// (`is_ir`, fingerprint, formats, applied quirks) that used to be threaded
/// around as loose values — pre-flight gates read `resolved.caps`, and the
/// camera opened from it carries the very same caps.
pub(crate) fn resolve_camera_device(config: &Config) -> anyhow::Result<ResolvedCamera> {
    let quirks = QuirksDb::load();
    let device_info = match config.device.path.as_deref() {
        Some(path) => validate_device(path)
            .with_context(|| format!("failed to query configured camera {path}"))?,
        None => {
            auto_detect_device().context("no camera device specified and auto-detection failed")?
        }
    };

    Ok(ResolvedCamera::interrogate(device_info, &quirks))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CameraPurpose {
    Authentication,
    Capture,
}

fn warmup_capture_count(
    purpose: CameraPurpose,
    ir_texture_scale: facelock_core::types::IrTextureScale,
    requested: u32,
) -> u32 {
    match (purpose, ir_texture_scale) {
        (CameraPurpose::Authentication, facelock_core::types::IrTextureScale::UnverifiedY16) => 0,
        _ => requested,
    }
}

/// Open an already-resolved camera and apply its purpose-specific warmup.
fn open_resolved_camera(
    config: &Config,
    resolved: ResolvedCamera,
    purpose: CameraPurpose,
) -> anyhow::Result<Camera<'static>> {
    // Discard warmup frames for AGC/AE stabilization.
    // Quirk override takes precedence over config value.
    let warmup = resolved
        .quirk
        .as_ref()
        .and_then(|q| q.warmup_frames)
        .unwrap_or(config.device.warmup_frames);
    let mut camera = resolved
        .open(&config.device)
        .context("failed to open camera")?;
    let captures = warmup_capture_count(purpose, camera.capabilities().ir_texture_scale, warmup);
    if captures > 0 {
        debug!(warmup = captures, "discarding warmup frames");
        for _ in 0..captures {
            let _ = camera.capture();
        }
    }

    Ok(camera)
}

/// Open camera with quirks support and warmup frame discarding.
pub fn open_camera(config: &Config) -> anyhow::Result<Camera<'static>> {
    open_resolved_camera(
        config,
        resolve_camera_device(config)?,
        CameraPurpose::Capture,
    )
}

pub fn load_engine(config: &Config) -> anyhow::Result<FaceEngine> {
    FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir))
        .context("failed to load face engine")
}

/// Direct authentication — returns the full match result (including an
/// internal failure reason when frames matched but a liveness gate blocked).
///
/// This backs `facelock test` only (root-only, N11/issue #96). It runs the
/// same pre-flight gates real authentication does — disabled check,
/// enrollment/`suppress_unknown`, rate-limit *check*, `require_ir` — via
/// `pre_check_audited_with_context`, except SSH/lid abort, which exist to
/// stop an *attacker*'s physical-access shortcuts and are explicitly skipped
/// for `test` via [`PreCheckContext::test`]. A failed attempt here never
/// consumes the shared rate-limit budget: unlike the daemon and oneshot
/// paths, this function simply never calls `RateLimiter::record_failure`.
/// Audit entries are stamped `AuditSource::Test`.
pub fn authenticate(config: &Config, user: &str) -> anyhow::Result<MatchResult> {
    let store = open_store(config)?;

    // Cheap device resolution only (no camera I/O), so a pre-check rejection
    // never touches the camera. The same resolution is opened below once auth
    // actually proceeds, so the caps the gates saw are the caps the camera
    // carries.
    let resolved = resolve_camera_device(config).context("failed to resolve camera device")?;

    let rl = &config.security.rate_limit;
    let rate_limiter = RateLimiter::new(rl.max_attempts, rl.window_secs);

    if let Some(resp) = pre_check_audited_with_context(
        config,
        &store,
        user,
        &rate_limiter,
        &resolved.caps,
        AuditSource::Test,
        PreCheckContext::test(),
    ) {
        return match resp {
            AuthOutcome::AuthResult(mr) => Ok(mr),
            // `suppress_unknown` short-circuit: no result to report, same as
            // the plain not-enrolled case below from `test`'s point of view.
            AuthOutcome::Suppressed => Ok(MatchResult {
                matched: false,
                model_id: None,
                label: None,
                similarity: 0.0,
                face_detected: false,
                failure_reason: None,
            }),
            AuthOutcome::Error { message, .. } => bail!("{message}"),
            // No camera has been opened yet, so nothing can have cancelled.
            AuthOutcome::Cancelled => bail!("authentication cancelled"),
        };
    }

    let mut camera = open_resolved_camera(config, resolved, CameraPurpose::Authentication)?;
    let mut engine = load_engine(config)?;

    // Load embeddings with encryption support, matching the daemon handler path.
    let mut stored = load_user_embeddings(&store, config, user)?;
    // Same shape as the daemon's `handle_authenticate` (C3): a storage failure
    // is an error, not an empty model list that guarantees "no match". No
    // rate-limit charge exists on this path, but the falsehood is the same.
    let models = store
        .list_models(user)
        .context("storage error listing face models")?;

    // Shared with daemon mode (crates/facelock-daemon/src/auth.rs). A local copy
    // of this loop previously lived here and silently drifted — do not re-fork it.
    // It wipes `stored` (D11), so nothing below may read it again.
    let response = facelock_daemon::auth::authenticate_with_embeddings(
        &mut camera,
        &mut engine,
        &mut stored,
        &models,
        config,
        user,
        AuditSource::Test,
        // The direct path has no bus connection to watch and no signal
        // handler of its own; `facelock auth` installs one (ADR 008 §7).
        &CancelToken::new(),
    );

    match response {
        AuthOutcome::AuthResult(result) => Ok(result),
        AuthOutcome::Error { message, .. } => bail!("{message}"),
        AuthOutcome::Suppressed => bail!("unexpected auth response"),
        AuthOutcome::Cancelled => bail!("authentication cancelled"),
    }
}

/// Initialize a software sealer based on encryption config.
/// Returns `None` if encryption is disabled.
fn init_software_sealer(config: &Config) -> anyhow::Result<Option<facelock_tpm::SoftwareSealer>> {
    match config.encryption.method {
        EncryptionMethod::Keyfile => {
            let key_path = Path::new(&config.encryption.key_path);
            // Encrypt-by-default (finding #8): generate the key on first use so
            // the keyfile default actually encrypts new templates.
            if !key_path.exists() {
                facelock_tpm::SoftwareSealer::generate_key_file(key_path)
                    .context("failed to auto-generate encryption key")?;
                debug!("generated encryption key at {}", key_path.display());
            }
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

/// Load user embeddings, decrypting software-encrypted or TPM-sealed blobs as
/// needed. The per-row decrypt is the daemon handler's own implementation
/// (`facelock_daemon::embeddings`, N10) — a local copy previously lived here
/// and drifted (B3: it predated `tpm.seal_database` rows, which broke `bench`,
/// `preview --text-only`, `test` and oneshot on sealed stores).
pub fn load_user_embeddings(
    store: &FaceStore,
    config: &Config,
    user: &str,
) -> anyhow::Result<Vec<(u32, facelock_core::types::FaceEmbedding)>> {
    let software_sealer = init_software_sealer(config)?;

    // Fast path: nothing is configured that could have written encrypted rows.
    // `seal_database` forces the raw path even without a software sealer, so a
    // TPM-sealed blob is never misread as a raw embedding.
    if !facelock_daemon::embeddings::needs_raw_rows(config, software_sealer.is_some()) {
        return store
            .get_user_embeddings(user)
            .context("storage error loading embeddings");
    }

    // Slow path: load raw blobs (with device ids for opt-in AAD) and decrypt.
    // The TPM connection is lazy — only made when a TPM-sealed row is present.
    let raw_rows = store
        .get_user_embeddings_raw_with_device(user)
        .context("storage error loading raw embeddings")?;

    facelock_daemon::embeddings::decrypt_user_embeddings(
        &raw_rows,
        config,
        software_sealer.as_ref(),
        facelock_daemon::embeddings::TpmAccess::Lazy {
            tcti: &config.tpm.tcti,
            sealer: None,
        },
    )
    .map_err(|message| anyhow::anyhow!(message))
}

/// Direct enrollment — returns (model_id, embedding_count).
pub fn enroll(config: &Config, user: &str, label: &str) -> anyhow::Result<(u32, u32)> {
    // Refuse to store a plaintext template unless explicitly opted in.
    config
        .ensure_enroll_encryption_allowed()
        .map_err(|m| anyhow::anyhow!(m))?;

    let store = open_store(config)?;
    let mut camera = open_camera(config)?;
    // The enrolling camera's own identity — asked of the camera that records
    // the template.
    let device_id = camera.capabilities().fingerprint.canonical_for_storage();
    let mut engine = load_engine(config)?;

    // Initialize sealer if encryption is configured
    let software_sealer = init_software_sealer(config)?;

    // Shared with daemon mode (facelock-daemon/src/enroll.rs): same quality
    // gate, angle-diversity check, rejection breakdown, and deadline. A local
    // copy of this loop previously lived here and silently drifted (no
    // quality/diversity gates, no rejection breakdown) — do not re-fork it.
    let response = facelock_daemon::enroll::enroll(
        &mut camera,
        &mut engine,
        &store,
        config,
        user,
        label,
        software_sealer.as_ref(),
        device_id.as_deref(),
        &CancelToken::new(),
    );

    match response {
        EnrollOutcome::Enrolled {
            model_id,
            embedding_count,
        } => Ok((model_id, embedding_count)),
        EnrollOutcome::Error { message } => bail!("{message}"),
        EnrollOutcome::Cancelled => bail!("enrollment cancelled"),
    }
}

/// Direct device enumeration (no daemon needed), in the transport shape the
/// backend seam hands to the renderer.
///
/// The quirks DB is consulted so the reported `is_ir` matches the
/// authoritative decision the auth path makes (e.g. a quirks `force_ir`
/// camera), with node-level disambiguation for multi-node USB devices.
/// Formats survive here — the D-Bus `DeviceInfo` type does not carry them
/// (`BackendCaps::device_formats`).
pub fn list_devices_info() -> anyhow::Result<Vec<facelock_core::ipc::IpcDeviceInfo>> {
    let devices = list_devices().context("failed to enumerate devices")?;
    let quirks = facelock_camera::QuirksDb::load();
    let sources = facelock_camera::classify_ir_sources(&devices, Some(&quirks));
    Ok(devices
        .iter()
        .zip(&sources)
        .map(|(dev, source)| facelock_core::ipc::IpcDeviceInfo {
            path: dev.path.clone(),
            name: dev.name.clone(),
            driver: dev.driver.clone(),
            is_ir: *source != facelock_camera::IrSource::None,
            formats: dev
                .formats
                .iter()
                .map(|f| facelock_core::ipc::IpcFormatInfo {
                    fourcc: f.fourcc.clone(),
                    description: f.description.clone(),
                    sizes: f.sizes.clone(),
                })
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use facelock_camera::{DeviceInfo, FormatInfo};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

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
        let device = make_device("/dev/video2", "BRIO IR", "GREY");
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

        let resolved = ResolvedCamera::interrogate(device, &quirks);

        assert_eq!(resolved.info.path, "/dev/video2");
        assert!(resolved.caps.is_ir);
        assert_eq!(resolved.caps.applied_quirks.len(), 1);
        assert_eq!(
            resolved.quirk.as_ref().and_then(|q| q.warmup_frames),
            Some(9)
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unmatched_device_keeps_default_warmup_and_non_ir_state() {
        let device = make_device("/dev/video3", "USB Camera", "MJPG");
        let quirks = QuirksDb::default();

        let resolved = ResolvedCamera::interrogate(device, &quirks);

        assert_eq!(resolved.info.path, "/dev/video3");
        assert!(!resolved.caps.is_ir);
        // No quirk match: `open_resolved_camera` falls back to the config's
        // own warmup_frames.
        assert!(resolved.quirk.is_none());
        assert!(resolved.caps.applied_quirks.is_empty());
    }

    /// Post-open unverified Y16 is fatal only to authentication. Authentication
    /// must reach its provenance rejection without capturing, while preview,
    /// enrollment, and bench retain the requested AGC/AE warmup that also
    /// drives lazy non-auth Y16 calibration.
    #[test]
    fn direct_open_warmup_policy_is_request_purpose_aware() {
        let requested = 7;
        let scale = facelock_core::types::IrTextureScale::UnverifiedY16;

        assert_eq!(
            warmup_capture_count(CameraPurpose::Authentication, scale, requested),
            0,
            "authentication must reject post-open Y16 provenance before capture"
        );
        assert_eq!(
            warmup_capture_count(CameraPurpose::Capture, scale, requested),
            requested,
            "non-auth direct capture must preserve configured warmup"
        );
        assert_eq!(
            warmup_capture_count(
                CameraPurpose::Authentication,
                facelock_core::types::IrTextureScale::NotY16,
                requested,
            ),
            requested,
            "GREY and color authentication must preserve configured warmup"
        );
    }

    // --- N8: encrypted-store loading in the direct path ---

    #[test]
    fn load_user_embeddings_decrypts_software_sealed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        let config = Config::parse(&format!(
            "[encryption]\nmethod = \"keyfile\"\nkey_path = \"{}\"\n",
            key_path.display()
        ))
        .unwrap();

        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let sealer = facelock_tpm::SoftwareSealer::from_key_file(&key_path).unwrap();
        let emb: facelock_core::types::FaceEmbedding = [0.25; 512];
        let blob = sealer.seal_embedding(&emb).unwrap();

        let store = FaceStore::open_memory().unwrap();
        store
            .add_model_raw("alice", "front", &blob, true, "embedder")
            .unwrap();

        let loaded = load_user_embeddings(&store, &config, "alice").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].1, emb);
    }

    /// B3: a TPM-sealed row (version byte 0x01, written under
    /// `tpm.seal_database`) must reach the TPM unseal path — never be misread
    /// as a raw embedding via the fast path. Without the `tpm` feature the
    /// passthrough sealer reports a clear "without TPM support" error; actual
    /// hardware unsealing cannot be asserted in CI and is exercised only on a
    /// machine with a TPM.
    #[cfg(not(feature = "tpm"))]
    #[test]
    fn tpm_sealed_rows_error_clearly_without_tpm_support() {
        let config =
            Config::parse("[encryption]\nmethod = \"none\"\n\n[tpm]\nseal_database = true\n")
                .unwrap();
        let store = FaceStore::open_memory().unwrap();
        let mut blob = vec![0x01u8];
        blob.extend_from_slice(&[0u8; 64]);
        store
            .add_model_raw("alice", "front", &blob, true, "embedder")
            .unwrap();

        let err = load_user_embeddings(&store, &config, "alice").unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("TPM unseal failed for embedding"), "{chain}");
        assert!(chain.contains("without TPM support"), "{chain}");
    }

    /// `seal_database` forces the raw-row path even with no software sealer;
    /// plaintext rows stored before sealing was enabled must still load.
    #[test]
    fn seal_database_slow_path_still_loads_plaintext_rows() {
        let config =
            Config::parse("[encryption]\nmethod = \"none\"\n\n[tpm]\nseal_database = true\n")
                .unwrap();
        let store = FaceStore::open_memory().unwrap();
        let emb: facelock_core::types::FaceEmbedding = [0.75; 512];
        store.add_model("alice", "front", &emb, "embedder").unwrap();

        let loaded = load_user_embeddings(&store, &config, "alice").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].1, emb);
    }
}
