use std::time::{Duration, Instant};

use facelock_camera::capture::is_dark_with_config;
use facelock_core::config::Config;
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_core::types::FaceEmbedding;
use facelock_store::FaceStore;
use facelock_tpm::SoftwareSealer;
use tracing::{debug, info, warn};

use crate::cancel::CancelToken;
use crate::quality;

/// The outcome of an enrollment attempt — shared by the daemon handler
/// (which maps it onto its wire response) and the CLI's direct path, so the
/// CLI never needs the handler's own request/response enums (D5).
#[derive(Debug, Clone)]
pub enum EnrollOutcome {
    Enrolled {
        model_id: u32,
        embedding_count: u32,
    },
    Error {
        message: String,
    },
    /// The caller went away, the system is suspending, or the process was
    /// signalled. Same rule as [`crate::auth::AuthOutcome::Cancelled`]: the
    /// enrollment was abandoned, not refused, and the camera closes at once
    /// (ADR 008 §5).
    Cancelled,
}

/// The store refuses a model with fewer embeddings than this, so the capture
/// gate and the row invariant are one number (#308).
const MIN_CAPTURES: usize = facelock_store::MIN_EMBEDDINGS_PER_MODEL;
const MAX_CAPTURES: usize = 10;
const INTER_FRAME_DELAY: Duration = Duration::from_millis(200);

/// Per-frame rejection tally for the enrollment loop, so a failed enrollment
/// can explain *why* frames were rejected instead of a bare capture count
/// (issue #89: an all-dark session reported only "captured 0 frames").
#[derive(Default)]
struct RejectionStats {
    dark: u32,
    no_face: u32,
    multiple_faces: u32,
    low_quality: u32,
    capture_errors: u32,
    last_capture_error: Option<String>,
    engine_errors: u32,
    last_engine_error: Option<String>,
}

impl RejectionStats {
    fn total(&self) -> u32 {
        self.dark
            + self.no_face
            + self.multiple_faces
            + self.low_quality
            + self.capture_errors
            + self.engine_errors
    }

    /// Human-readable breakdown appended to the insufficient-captures error,
    /// with a remediation hint when one cause dominates. Empty when nothing
    /// was rejected (e.g. the camera produced no frames at all).
    fn summary(&self) -> String {
        if self.total() == 0 {
            return String::new();
        }
        let mut parts = Vec::new();
        if self.dark > 0 {
            parts.push(format!("{} too dark", self.dark));
        }
        if self.no_face > 0 {
            parts.push(format!("{} no face", self.no_face));
        }
        if self.multiple_faces > 0 {
            parts.push(format!("{} multiple faces", self.multiple_faces));
        }
        if self.low_quality > 0 {
            parts.push(format!("{} low quality", self.low_quality));
        }
        if self.capture_errors > 0 {
            match &self.last_capture_error {
                Some(e) => parts.push(format!(
                    "{} capture errors (last: {e})",
                    self.capture_errors
                )),
                None => parts.push(format!("{} capture errors", self.capture_errors)),
            }
        }
        if self.engine_errors > 0 {
            match &self.last_engine_error {
                Some(e) => parts.push(format!("{} engine errors (last: {e})", self.engine_errors)),
                None => parts.push(format!("{} engine errors", self.engine_errors)),
            }
        }
        let hint = self.hint().map(|h| format!(". {h}")).unwrap_or_default();
        format!(" — rejected frames: {}{hint}", parts.join(", "))
    }

    /// Remediation hint when a single cause accounts for the majority of
    /// rejections.
    fn hint(&self) -> Option<&'static str> {
        let majority = self.total() / 2 + 1;
        if self.dark >= majority {
            Some("Hint: the scene is too dark — improve lighting and retry")
        } else if self.capture_errors >= majority {
            Some(
                "Hint: the camera is not delivering usable frames — check device.path and the camera format (see docs/troubleshooting.md)",
            )
        } else if self.engine_errors >= majority {
            Some(
                "Hint: the face engine is failing on captured frames — check the model files and recognition.execution_provider (see docs/troubleshooting.md)",
            )
        } else if self.no_face >= majority {
            Some(
                "Hint: no face was detected — face the camera directly and check `facelock preview`",
            )
        } else {
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn enroll<C: CameraSource, E: FaceProcessor>(
    camera: &mut C,
    engine: &mut E,
    store: &FaceStore,
    config: &Config,
    user: &str,
    label: &str,
    sealer: Option<&SoftwareSealer>,
    device_id: Option<&str>,
    cancel: &CancelToken,
) -> EnrollOutcome {
    // Opt-in hard device binding (Plan 04): when enabled, fold this camera's
    // device id into the AES-GCM AAD so the template can only be decrypted
    // under the same camera. With no device id there is nothing to bind to,
    // and that is a refusal (#312), made before the store is touched so a
    // re-enrollment under the same label keeps its previous template. The
    // callers judge the live fingerprint first; this holds for every caller.
    let device_aad = match config.security.require_device_aad(device_id) {
        Ok(aad) => aad,
        Err(message) => {
            warn!(user, label, "enroll refused: {message}");
            return EnrollOutcome::Error { message };
        }
    };

    // Shared deadline formula (see Config::enroll_timeout_secs): the CLI's
    // D-Bus Enroll timeout is derived from the same value plus margin.
    let enroll_secs = config.enroll_timeout_secs();
    let deadline = Instant::now() + Duration::from_secs(enroll_secs);
    debug!(timeout_secs = enroll_secs, "starting enrollment");
    let mut last_capture = Instant::now() - INTER_FRAME_DELAY; // allow immediate first capture
    // Accepted embeddings wait here until the whole set has passed the gates;
    // nothing reaches the store before then (#308). The guard wipes them on
    // every exit path, unwind included.
    let mut accepted: facelock_core::types::Wiped<Vec<FaceEmbedding>, FaceEmbedding> =
        facelock_core::types::Wiped::with_capacity(MAX_CAPTURES);
    let mut rejections = RejectionStats::default();

    while Instant::now() < deadline && accepted.len() < MAX_CAPTURES {
        // Checked before the inter-frame sleep and the blocking capture, so
        // an abandoned enrollment ends within one frame (ADR 008 §5).
        if cancel.is_cancelled() {
            info!(
                user,
                label,
                captures = accepted.len(),
                "enrollment cancelled"
            );
            return EnrollOutcome::Cancelled;
        }
        // Delay between captures for varied angles
        let since_last = Instant::now().duration_since(last_capture);
        if since_last < INTER_FRAME_DELAY {
            std::thread::sleep(INTER_FRAME_DELAY - since_last);
        }

        let capture_start = Instant::now();
        let frame = match camera.capture() {
            Ok(f) => f,
            Err(e) => {
                debug!("capture error during enroll: {e}");
                rejections.capture_errors += 1;
                rejections.last_capture_error = Some(e.to_string());
                continue;
            }
        };
        let capture_ms = capture_start.elapsed().as_millis();

        if is_dark_with_config(
            &frame,
            config.device.dark_threshold,
            config.device.dark_pixel_value,
        ) {
            warn!(capture_ms, "skipping dark frame during enroll");
            rejections.dark += 1;
            continue;
        }

        let detect_start = Instant::now();
        let faces = match engine.process(&frame) {
            Ok(f) => f,
            Err(e) => {
                warn!("face engine error during enroll: {e}");
                rejections.engine_errors += 1;
                rejections.last_engine_error = Some(e.to_string());
                continue;
            }
        };
        let detect_ms = detect_start.elapsed().as_millis();

        // Require exactly 1 face
        if faces.is_empty() {
            info!(capture_ms, detect_ms, "no face detected during enroll");
            rejections.no_face += 1;
            continue;
        }
        if faces.len() > 1 {
            warn!(
                count = faces.len(),
                "multiple faces detected during enroll, skipping frame"
            );
            rejections.multiple_faces += 1;
            continue;
        }

        let (det, embedding) = &faces[0];

        // Quality gate: skip low-quality frames
        let frame_quality = quality::score_frame(det, &frame.gray, frame.width, frame.height);
        if !quality::meets_quality_threshold(&frame_quality) {
            if let Some(hint) = quality::quality_hint(&frame_quality) {
                debug!(
                    overall = format!("{:.2}", frame_quality.overall),
                    hint, "skipping low-quality enrollment frame"
                );
            } else {
                debug!(
                    overall = format!("{:.2}", frame_quality.overall),
                    "skipping low-quality enrollment frame"
                );
            }
            rejections.low_quality += 1;
            continue;
        }

        accepted.push(*embedding);
        debug!(
            capture_ms,
            detect_ms,
            count = accepted.len(),
            "accepted enrollment frame"
        );

        last_capture = Instant::now();
    }

    // A cancellation that landed on the last frame is still a cancellation:
    // the caller is gone, and a template it will never hear about is not
    // stored on its behalf.
    if cancel.is_cancelled() {
        info!(
            user,
            label,
            captures = accepted.len(),
            "enrollment cancelled before commit"
        );
        return EnrollOutcome::Cancelled;
    }

    // Check angle diversity: reject if all embeddings are too similar
    if accepted.len() >= MIN_CAPTURES && !quality::check_angle_diversity(&accepted) {
        warn!(
            user,
            label,
            captured = accepted.len(),
            "insufficient angle diversity during enrollment"
        );
        return EnrollOutcome::Error {
            message: "insufficient angle diversity: please move your head to different angles during enrollment".into(),
        };
    }

    if accepted.len() < MIN_CAPTURES {
        warn!(
            user,
            label,
            captured = accepted.len(),
            required = MIN_CAPTURES,
            dark = rejections.dark,
            no_face = rejections.no_face,
            multiple_faces = rejections.multiple_faces,
            low_quality = rejections.low_quality,
            capture_errors = rejections.capture_errors,
            engine_errors = rejections.engine_errors,
            "insufficient face captures during enrollment"
        );
        return EnrollOutcome::Error {
            message: format!(
                "only captured {} frames, need at least {MIN_CAPTURES}{}",
                accepted.len(),
                rejections.summary()
            ),
        };
    }

    persist_enrollment(
        store,
        &config.recognition.embedder_model,
        user,
        label,
        sealer,
        device_id,
        device_aad.as_deref(),
        &accepted,
        cancel,
    )
}

/// What enrollment asks of a sealer: one embedding in, its ciphertext out.
///
/// [`SoftwareSealer`] is the only implementation outside tests. The seam
/// exists because AES-GCM under a valid key never fails, and the promise that
/// a sealing failure writes nothing needs a sealer that can.
trait EmbeddingSealer {
    fn seal_embedding_with_aad(
        &self,
        embedding: &FaceEmbedding,
        aad: Option<&[u8]>,
    ) -> facelock_core::error::Result<Vec<u8>>;
}

impl EmbeddingSealer for SoftwareSealer {
    fn seal_embedding_with_aad(
        &self,
        embedding: &FaceEmbedding,
        aad: Option<&[u8]>,
    ) -> facelock_core::error::Result<Vec<u8>> {
        SoftwareSealer::seal_embedding_with_aad(self, embedding, aad)
    }
}

/// Enrollment's one write: seal every accepted embedding, then replace the
/// user's same-label model in a single store transaction.
///
/// Runs only after the capture gates passed, so a template a later
/// authentication can load is always a complete one; and because nothing was
/// written before this point, a failure here — sealing or storage — leaves
/// the store exactly as it was, previous same-label model included (#308).
/// `cancel` is re-checked right before the write, so a caller that departs
/// during sealing gets `Cancelled` and the same unchanged store.
#[allow(clippy::too_many_arguments)]
fn persist_enrollment<S: EmbeddingSealer>(
    store: &FaceStore,
    embedder_model: &str,
    user: &str,
    label: &str,
    sealer: Option<&S>,
    device_id: Option<&str>,
    device_aad: Option<&[u8]>,
    accepted: &[FaceEmbedding],
    cancel: &CancelToken,
) -> EnrollOutcome {
    let mut sealed: Vec<Vec<u8>> = Vec::new();
    if let Some(sealer) = sealer {
        sealed.reserve_exact(accepted.len());
        for embedding in accepted {
            match sealer.seal_embedding_with_aad(embedding, device_aad) {
                Ok(blob) => sealed.push(blob),
                Err(e) => {
                    warn!(user, label, "failed to encrypt embedding: {e}");
                    return EnrollOutcome::Error {
                        message: format!("encryption error: {e}"),
                    };
                }
            }
        }
    }
    // Plaintext rows borrow straight from the guarded buffer: no unguarded
    // copy of a template exists at any point.
    let blobs: Vec<&[u8]> = if sealer.is_some() {
        sealed.iter().map(Vec::as_slice).collect()
    } else {
        accepted
            .iter()
            .map(|embedding| bytemuck::cast_slice(embedding.as_slice()))
            .collect()
    };

    // Last look at the token before the only write: the caller-departure
    // watch can fire while the set is being sealed, after the loop's final
    // check. Past this point the commit itself is the residual window.
    if cancel.is_cancelled() {
        info!(
            user,
            label,
            captures = accepted.len(),
            "enrollment cancelled before commit"
        );
        return EnrollOutcome::Cancelled;
    }

    match store.replace_model_with_embeddings(
        user,
        label,
        &blobs,
        sealer.is_some(),
        embedder_model,
        device_id,
    ) {
        Ok(model_id) => {
            info!(
                user,
                label,
                model_id,
                embedding_count = accepted.len(),
                encrypted = sealer.is_some(),
                "enrollment complete"
            );
            EnrollOutcome::Enrolled {
                model_id,
                embedding_count: accepted.len() as u32,
            }
        }
        Err(e) => {
            warn!(user, label, "failed to store enrollment: {e}");
            EnrollOutcome::Error {
                message: format!("storage error: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facelock_test_support::{MockCamera, MockFaceEngine, fixtures};

    #[test]
    fn summary_empty_when_no_rejections() {
        let stats = RejectionStats::default();
        assert_eq!(stats.summary(), "");
    }

    #[test]
    fn summary_all_dark_includes_lighting_hint() {
        // Issue #89: an all-dark enrollment session must say so, not just
        // "captured 0 frames".
        let stats = RejectionStats {
            dark: 42,
            ..Default::default()
        };
        let s = stats.summary();
        assert!(s.contains("42 too dark"), "got: {s}");
        assert!(s.contains("improve lighting"), "got: {s}");
    }

    #[test]
    fn summary_capture_errors_include_last_error() {
        let stats = RejectionStats {
            capture_errors: 5,
            last_capture_error: Some("unsupported format: NV12".into()),
            ..Default::default()
        };
        let s = stats.summary();
        assert!(
            s.contains("5 capture errors (last: unsupported format: NV12)"),
            "got: {s}"
        );
        assert!(s.contains("check device.path"), "got: {s}");
    }

    #[test]
    fn summary_no_face_majority_hints_preview() {
        let stats = RejectionStats {
            no_face: 10,
            dark: 2,
            ..Default::default()
        };
        let s = stats.summary();
        assert!(s.contains("10 no face"), "got: {s}");
        assert!(s.contains("2 too dark"), "got: {s}");
        assert!(s.contains("facelock preview"), "got: {s}");
    }

    #[test]
    fn summary_mixed_causes_has_no_hint() {
        // No single majority cause -> breakdown only, no misleading hint.
        let stats = RejectionStats {
            dark: 3,
            no_face: 3,
            low_quality: 3,
            ..Default::default()
        };
        let s = stats.summary();
        assert!(s.contains("rejected frames:"), "got: {s}");
        assert!(stats.hint().is_none(), "got: {:?}", stats.hint());
    }

    #[test]
    fn hint_requires_a_strict_majority() {
        // 2 of 4 is not a majority: no cause dominates, so no hint.
        let tied = RejectionStats {
            dark: 2,
            no_face: 2,
            ..Default::default()
        };
        assert!(tied.hint().is_none(), "got: {:?}", tied.hint());

        // 2 of 3 is: the dominant cause gets its remediation hint.
        let dark_majority = RejectionStats {
            dark: 2,
            no_face: 1,
            ..Default::default()
        };
        assert!(
            dark_majority
                .hint()
                .is_some_and(|h| h.contains("improve lighting")),
            "got: {:?}",
            dark_majority.hint()
        );
    }

    #[test]
    fn summary_engine_errors_are_distinct_from_capture_errors() {
        // A failing face engine must not be reported as a camera problem:
        // the "check device.path" hint sends the user after healthy hardware.
        let stats = RejectionStats {
            engine_errors: 6,
            last_engine_error: Some("onnxruntime: invalid input shape".into()),
            ..Default::default()
        };
        let s = stats.summary();
        assert!(
            s.contains("6 engine errors (last: onnxruntime: invalid input shape)"),
            "got: {s}"
        );
        assert!(s.contains("check the model files"), "got: {s}");
        assert!(!s.contains("check device.path"), "got: {s}");
    }

    #[test]
    fn summary_lists_multiple_faces_and_low_quality() {
        let stats = RejectionStats {
            multiple_faces: 2,
            low_quality: 4,
            ..Default::default()
        };
        let s = stats.summary();
        assert!(s.contains("2 multiple faces"), "got: {s}");
        assert!(s.contains("4 low quality"), "got: {s}");
    }

    #[test]
    fn angle_diversity_failure_leaves_no_model_behind() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db_path = std::env::temp_dir().join(format!(
            "facelock-enroll-cleanup-{}-{unique}.db",
            std::process::id()
        ));
        let store = FaceStore::create(&db_path).unwrap();
        let config = Config::parse("[recognition]\ntimeout_secs = 2\n").unwrap();

        // One repeated embedding: every frame passes the quality gate and is
        // stored, then check_angle_diversity rejects the template because all
        // pairs are identical. This is the failure that used to leave an
        // authenticatable model row behind.
        let mut camera = MockCamera::bright(640, 480, 3);
        let mut engine = MockFaceEngine::one_face(fixtures::known_embedding(0));

        let response = enroll(
            &mut camera,
            &mut engine,
            &store,
            &config,
            "alice",
            "2026-08-08-1",
            None,
            None,
            &CancelToken::new(),
        );

        match &response {
            EnrollOutcome::Error { message } => {
                assert!(message.contains("angle diversity"), "got: {message}")
            }
            other => panic!("expected an angle-diversity error, got: {other:?}"),
        }
        assert_eq!(
            store.list_models("alice").unwrap().len(),
            0,
            "rejected template must not remain in the database"
        );

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
    }

    // ---- persistence atomicity (#308) --------------------------------------

    /// Four embeddings far enough apart that every frame is accepted and the
    /// angle-diversity gate passes.
    fn diverse_engine() -> MockFaceEngine {
        MockFaceEngine::cycling(vec![
            fixtures::known_embedding(0),
            fixtures::known_embedding(40),
            fixtures::known_embedding(80),
            fixtures::known_embedding(120),
        ])
    }

    /// A camera whose `nth` capture cancels `cancel` — the frame it serves is
    /// still accepted, so the cancellation lands after that frame.
    fn camera_cancelling_on(nth: usize, cancel: &CancelToken) -> MockCamera {
        let token = cancel.clone();
        MockCamera::bright(640, 480, 4).with_capture_hook(move |n| {
            if n == nth {
                token.cancel();
            }
        })
    }

    fn enroll_alice(
        camera: &mut MockCamera,
        engine: &mut MockFaceEngine,
        store: &FaceStore,
        sealer: Option<&SoftwareSealer>,
        label: &str,
        cancel: &CancelToken,
    ) -> EnrollOutcome {
        let config = Config::parse("[recognition]\ntimeout_secs = 2\n").unwrap();
        enroll(
            camera, engine, store, &config, "alice", label, sealer, None, cancel,
        )
    }

    /// No model row and no embedding row of any kind, sealed or plain.
    fn assert_store_untouched(store: &FaceStore) {
        assert!(
            store.list_models("alice").unwrap().is_empty(),
            "no model may remain: {:?}",
            store.list_models("alice").unwrap()
        );
        assert_eq!(
            store.count_sealed().unwrap(),
            (0, 0),
            "no embedding row may remain (sealed, unsealed)"
        );
    }

    #[test]
    fn cancellation_before_the_first_accepted_frame_leaves_no_model() {
        let store = FaceStore::open_memory().unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut camera = MockCamera::bright(640, 480, 4);

        let outcome = enroll_alice(
            &mut camera,
            &mut diverse_engine(),
            &store,
            None,
            "front",
            &cancel,
        );

        assert!(
            matches!(outcome, EnrollOutcome::Cancelled),
            "got: {outcome:?}"
        );
        assert_eq!(camera.captures(), 0, "a cancelled request captures nothing");
        assert_store_untouched(&store);
    }

    #[test]
    fn cancellation_after_the_first_accepted_frame_leaves_no_model_or_embeddings() {
        let store = FaceStore::open_memory().unwrap();
        let cancel = CancelToken::new();
        let mut camera = camera_cancelling_on(1, &cancel);

        let outcome = enroll_alice(
            &mut camera,
            &mut diverse_engine(),
            &store,
            Some(&SoftwareSealer::from_key([7u8; 32])),
            "front",
            &cancel,
        );

        assert!(
            matches!(outcome, EnrollOutcome::Cancelled),
            "got: {outcome:?}"
        );
        assert_eq!(camera.captures(), 1, "the loop ends within one frame");
        assert_store_untouched(&store);
    }

    #[test]
    fn cancellation_at_finalization_persists_nothing() {
        // The cancellation lands on the capture that completes the set, so
        // the loop ends by MAX_CAPTURES with the token set: finalization
        // must honour it rather than commit a template the caller abandoned.
        let store = FaceStore::open_memory().unwrap();
        let cancel = CancelToken::new();
        let mut camera = camera_cancelling_on(MAX_CAPTURES, &cancel);

        let outcome = enroll_alice(
            &mut camera,
            &mut diverse_engine(),
            &store,
            Some(&SoftwareSealer::from_key([7u8; 32])),
            "front",
            &cancel,
        );

        assert!(
            matches!(outcome, EnrollOutcome::Cancelled),
            "got: {outcome:?}"
        );
        assert_eq!(camera.captures(), MAX_CAPTURES);
        assert_store_untouched(&store);
    }

    /// Fails on its `fail_at`-th call and counts every call: the failure a
    /// real AES-GCM sealer never produces.
    struct FailingSealer {
        fail_at: usize,
        calls: std::cell::Cell<usize>,
    }

    impl EmbeddingSealer for FailingSealer {
        fn seal_embedding_with_aad(
            &self,
            _embedding: &FaceEmbedding,
            _aad: Option<&[u8]>,
        ) -> facelock_core::error::Result<Vec<u8>> {
            let call = self.calls.get() + 1;
            self.calls.set(call);
            if call == self.fail_at {
                Err(facelock_core::error::FacelockError::Encryption(
                    "injected seal failure".into(),
                ))
            } else {
                Ok(vec![0xAB; 16])
            }
        }
    }

    #[test]
    fn seal_failure_on_the_nth_embedding_writes_nothing() {
        let store = FaceStore::open_memory().unwrap();
        let previous = store
            .add_model("alice", "front", &fixtures::known_embedding(9), "")
            .unwrap();
        let accepted: Vec<FaceEmbedding> = [0, 40, 80]
            .into_iter()
            .map(fixtures::known_embedding)
            .collect();
        let sealer = FailingSealer {
            fail_at: 2,
            calls: std::cell::Cell::new(0),
        };

        let outcome = persist_enrollment(
            &store,
            "",
            "alice",
            "front",
            Some(&sealer),
            None,
            None,
            &accepted,
            &CancelToken::new(),
        );

        match outcome {
            EnrollOutcome::Error { message } => assert!(
                message.contains("encryption error") && message.contains("injected seal failure"),
                "got: {message}"
            ),
            other => panic!("expected an encryption error, got: {other:?}"),
        }
        assert_eq!(sealer.calls.get(), 2, "sealing stops at the failure");

        // The previous same-label model is untouched and nothing from this
        // attempt exists: not the model, not the one blob that did seal.
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1, "got: {models:?}");
        assert_eq!(models[0].id, previous);
        assert_eq!(store.count_sealed().unwrap(), (0, 1));
    }

    /// Seals normally but cancels `token` on its first call: the caller
    /// departs while finalization is sealing, after the loop's last check.
    struct CancellingSealer {
        token: CancelToken,
        calls: std::cell::Cell<usize>,
    }

    impl EmbeddingSealer for CancellingSealer {
        fn seal_embedding_with_aad(
            &self,
            _embedding: &FaceEmbedding,
            _aad: Option<&[u8]>,
        ) -> facelock_core::error::Result<Vec<u8>> {
            self.calls.set(self.calls.get() + 1);
            self.token.cancel();
            Ok(vec![0xCD; 16])
        }
    }

    #[test]
    fn cancellation_during_sealing_persists_nothing() {
        let store = FaceStore::open_memory().unwrap();
        let previous = store
            .add_model("alice", "front", &fixtures::known_embedding(9), "")
            .unwrap();
        let previous_rows = store.get_user_embeddings_raw("alice").unwrap();
        let accepted: Vec<FaceEmbedding> = [0, 40, 80]
            .into_iter()
            .map(fixtures::known_embedding)
            .collect();
        let cancel = CancelToken::new();
        let sealer = CancellingSealer {
            token: cancel.clone(),
            calls: std::cell::Cell::new(0),
        };

        let outcome = persist_enrollment(
            &store,
            "",
            "alice",
            "front",
            Some(&sealer),
            None,
            None,
            &accepted,
            &cancel,
        );

        assert!(
            matches!(outcome, EnrollOutcome::Cancelled),
            "got: {outcome:?}"
        );
        assert_eq!(
            sealer.calls.get(),
            accepted.len(),
            "sealing itself is not interrupted"
        );
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1, "got: {models:?}");
        assert_eq!(
            models[0].id, previous,
            "the previous same-label model survived"
        );
        assert_eq!(
            store.get_user_embeddings_raw("alice").unwrap(),
            previous_rows,
            "its rows are untouched and nothing sealed here was written"
        );
    }

    #[test]
    fn successful_sealed_enrollment_persists_every_accepted_embedding() {
        let store = FaceStore::open_memory().unwrap();
        let sealer = SoftwareSealer::from_key([7u8; 32]);
        let mut camera = MockCamera::bright(640, 480, 4);

        let outcome = enroll_alice(
            &mut camera,
            &mut diverse_engine(),
            &store,
            Some(&sealer),
            "front",
            &CancelToken::new(),
        );

        let (model_id, embedding_count) = match outcome {
            EnrollOutcome::Enrolled {
                model_id,
                embedding_count,
            } => (model_id, embedding_count),
            other => panic!("expected success, got: {other:?}"),
        };
        assert_eq!(embedding_count as usize, MAX_CAPTURES);

        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, model_id);
        assert_eq!(models[0].label, "front");

        let rows = store.get_user_embeddings_raw("alice").unwrap();
        assert_eq!(rows.len(), MAX_CAPTURES, "one row per accepted frame");
        let known: Vec<FaceEmbedding> = [0, 40, 80, 120]
            .into_iter()
            .map(fixtures::known_embedding)
            .collect();
        for (id, blob, sealed) in &rows {
            assert_eq!(*id, model_id);
            assert!(sealed, "every row is sealed under the sealer's key");
            let plain = sealer.unseal_embedding_with_aad(blob, None).unwrap();
            assert!(
                known.contains(&plain),
                "each row unseals to one of the accepted embeddings"
            );
        }
    }

    #[test]
    fn successful_plaintext_enrollment_persists_every_accepted_embedding() {
        let store = FaceStore::open_memory().unwrap();
        let mut camera = MockCamera::bright(640, 480, 4);

        let outcome = enroll_alice(
            &mut camera,
            &mut diverse_engine(),
            &store,
            None,
            "front",
            &CancelToken::new(),
        );

        assert!(
            matches!(outcome, EnrollOutcome::Enrolled { embedding_count, .. } if embedding_count as usize == MAX_CAPTURES),
            "got: {outcome:?}"
        );
        let rows = store.get_user_embeddings("alice").unwrap();
        assert_eq!(rows.len(), MAX_CAPTURES);
        assert_eq!(rows[0].1, fixtures::known_embedding(0));
        assert_eq!(rows[1].1, fixtures::known_embedding(40));
        assert_eq!(store.count_sealed().unwrap(), (0, MAX_CAPTURES as u32));
    }

    #[test]
    fn cancelled_re_enrollment_keeps_the_previous_model() {
        let store = FaceStore::open_memory().unwrap();
        let sealer = SoftwareSealer::from_key([7u8; 32]);

        let first = enroll_alice(
            &mut MockCamera::bright(640, 480, 4),
            &mut diverse_engine(),
            &store,
            Some(&sealer),
            "front",
            &CancelToken::new(),
        );
        let previous_id = match first {
            EnrollOutcome::Enrolled { model_id, .. } => model_id,
            other => panic!("first enrollment must succeed, got: {other:?}"),
        };
        let previous_rows = store.get_user_embeddings_raw("alice").unwrap();
        assert_eq!(previous_rows.len(), MAX_CAPTURES);

        // Re-enroll under the same label, abandoned after two accepted frames.
        let cancel = CancelToken::new();
        let outcome = enroll_alice(
            &mut camera_cancelling_on(2, &cancel),
            &mut diverse_engine(),
            &store,
            Some(&sealer),
            "front",
            &cancel,
        );
        assert!(
            matches!(outcome, EnrollOutcome::Cancelled),
            "got: {outcome:?}"
        );

        // The template that was there before is there still, row for row:
        // what authenticated before the attempt authenticates after it.
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1, "got: {models:?}");
        assert_eq!(models[0].id, previous_id, "the previous model row survived");
        assert_eq!(
            store.get_user_embeddings_raw("alice").unwrap(),
            previous_rows,
            "the previous model's embeddings are untouched"
        );
    }

    /// #312, inside the loop itself: with hard binding on and no device id,
    /// enrollment is refused before a frame is captured or the store is
    /// touched, so a same-label re-enrollment keeps the template it had.
    #[test]
    fn hard_binding_without_a_device_id_refuses_before_touching_the_store() {
        use facelock_core::traits::CameraSource;
        use facelock_test_support::{MockCamera, MockFaceEngine, fixtures};

        let store = FaceStore::open_memory().unwrap();
        let kept = store
            .add_model("alice", "front", &fixtures::known_embedding(1), "")
            .unwrap();
        let config = Config::parse("[security]\nbind_device_aad = true\n").unwrap();
        let mut camera = MockCamera::bright(640, 480, 3);
        let mut engine = MockFaceEngine::one_face(fixtures::known_embedding(0));

        let response = enroll(
            &mut camera,
            &mut engine,
            &store,
            &config,
            "alice",
            "front",
            None,
            None,
            &CancelToken::new(),
        );

        match &response {
            EnrollOutcome::Error { message } => {
                assert!(
                    message.contains("security.bind_device_aad"),
                    "got: {message}"
                )
            }
            other => panic!("expected a hard-binding refusal, got: {other:?}"),
        }
        assert_eq!(camera.captures(), 0, "refused before any capture");
        let models = store.list_models("alice").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, kept, "the previous template must survive");
        assert_eq!(camera.capabilities().fingerprint.canonical(), "::");
    }
}
