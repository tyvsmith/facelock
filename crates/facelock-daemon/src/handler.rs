use std::time::{Duration, Instant};

use facelock_camera::MMAP_BUFFERS;
use facelock_core::config::{Config, DeviceConfig, EncryptionMethod};
use facelock_core::ipc::PreviewFace;
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_core::types::best_match;
use facelock_store::FaceStore;
use image::codecs::jpeg::JpegEncoder;
use tracing::{debug, info, warn};

use crate::audit::AuditSource;
use crate::auth::{self, AuthOutcome, CANCELLED_MESSAGE, PreCheckContext};
use crate::cancel::CancelToken;
use crate::enroll::{self, EnrollOutcome};
use crate::rate_limit::RateLimiter;

/// Why an authentication is being run.
///
/// This is the *declared purpose of the call*, never an inference from the
/// caller's privilege. The daemon used to infer it — a root D-Bus caller was
/// assumed to be root-only `facelock test` and had its failed attempts
/// exempted from the rate limit — but "caller is root" is not a proxy for
/// "this is a test run": `sudo` is setuid-root, and `login`, `su` and
/// root-run display-manager greeters run their PAM stack as root too. Real
/// failed authentications therefore reached the daemon as UID 0 and were
/// never charged, leaving the documented 5-attempts/user/60s limit inert on
/// the project's primary documented PAM target. The intent now travels with
/// the request: the `Authenticate` D-Bus method is always
/// [`AuthIntent::Authenticate`], and the root-only `TestAuthenticate` method
/// is the only producer of [`AuthIntent::Test`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthIntent {
    /// A real authentication — every PAM stack (sudo, login, screen
    /// lockers), the polkit agent, `facelock auth`. Fully enforced, and a
    /// failed attempt charges the shared rate-limit budget.
    Authenticate,
    /// A diagnostic run of root-only `facelock test` (N11, issue #96).
    /// Skips only the SSH/lid physical-presence gates, and never charges the
    /// budget — a handful of test runs must not lock the user out of real
    /// authentication.
    Test,
}

impl AuthIntent {
    /// Does a failed attempt consume the shared (SQLite-backed) rate-limit
    /// budget? Only real authentication does.
    pub fn charges_rate_limit(self) -> bool {
        matches!(self, AuthIntent::Authenticate)
    }

    /// Which of `pre_check`'s environment gates this intent may skip. Every
    /// other gate — `disabled`, enrollment/`suppress_unknown`, the
    /// rate-limit *check*, `require_ir` — applies to both intents.
    pub fn pre_check_context(self) -> PreCheckContext {
        match self {
            AuthIntent::Authenticate => PreCheckContext::enforced(),
            AuthIntent::Test => PreCheckContext::test(),
        }
    }

    /// The audit `source` stamped on the entries this intent produces. The
    /// field records the enforcement path, so a diagnostic run that skipped
    /// the SSH/lid gates and charged nothing must not be logged as an
    /// ordinary daemon authentication.
    pub fn audit_source(self) -> AuditSource {
        match self {
            AuthIntent::Authenticate => AuditSource::Daemon,
            AuthIntent::Test => AuditSource::Test,
        }
    }
}

/// The handler's input vocabulary — one variant per operation the daemon can
/// perform. Internal to this crate (D5): the D-Bus server (`crate::server`)
/// builds these from decoded method calls, and the CLI talks through typed
/// clients and the [`AuthOutcome`]/[`EnrollOutcome`] vocabulary instead. The
/// request/wire double translation is deliberate — it is what keeps this
/// handler transport-agnostic and mock-testable.
#[derive(Debug, Clone)]
pub enum DaemonRequest {
    Authenticate {
        user: String,
    },
    Enroll {
        user: String,
        label: String,
    },
    ListModels {
        user: String,
    },
    RemoveModel {
        user: String,
        model_id: u32,
    },
    ClearModels {
        user: String,
    },
    PreviewFrame,
    /// Preview with face detection + recognition against the given user's models.
    PreviewDetectFrame {
        user: String,
    },
    ListDevices,
    ReleaseCamera,
    Ping,
    Shutdown,
}

/// The handler's output vocabulary, mirroring [`DaemonRequest`]. Internal to
/// this crate (D5); `crate::server` maps it onto the D-Bus reply types.
#[derive(Debug, Clone)]
pub enum DaemonResponse {
    AuthResult(facelock_core::types::MatchResult),
    Enrolled {
        model_id: u32,
        embedding_count: u32,
    },
    Models(Vec<facelock_core::types::FaceModelInfo>),
    Removed,
    Frame {
        jpeg_data: Vec<u8>,
    },
    /// Preview frame with face detection results.
    DetectFrame {
        jpeg_data: Vec<u8>,
        faces: Vec<PreviewFace>,
    },
    Devices(Vec<facelock_core::ipc::IpcDeviceInfo>),
    Ok,
    /// User has no enrolled models and `suppress_unknown` is enabled.
    /// PAM should map this to `PAM_AUTHINFO_UNAVAIL` to let the stack fall through.
    Suppressed,
    Error {
        message: String,
    },
}

impl From<AuthOutcome> for DaemonResponse {
    fn from(outcome: AuthOutcome) -> Self {
        match outcome {
            AuthOutcome::AuthResult(result) => DaemonResponse::AuthResult(result),
            AuthOutcome::Suppressed => DaemonResponse::Suppressed,
            AuthOutcome::Error { message, .. } => DaemonResponse::Error { message },
            // The wire has no cancellation shape of its own, so a cancelled
            // attempt travels as a recoverable error whose message is the
            // frozen `cancelled` string. PAM maps it to PAM_IGNORE — the
            // stack falls through to password, which is what the user who
            // just typed one expects (docs/contracts.md).
            AuthOutcome::Cancelled => DaemonResponse::Error {
                message: CANCELLED_MESSAGE.to_string(),
            },
        }
    }
}

impl From<EnrollOutcome> for DaemonResponse {
    fn from(outcome: EnrollOutcome) -> Self {
        match outcome {
            EnrollOutcome::Enrolled {
                model_id,
                embedding_count,
            } => DaemonResponse::Enrolled {
                model_id,
                embedding_count,
            },
            EnrollOutcome::Error { message } => DaemonResponse::Error { message },
            EnrollOutcome::Cancelled => DaemonResponse::Error {
                message: CANCELLED_MESSAGE.to_string(),
            },
        }
    }
}

/// Type alias for the camera factory closure.
type CameraFactory<C> = Box<dyn Fn(&Config) -> Result<C, String> + Send + Sync>;

const JPEG_BUF_CAPACITY: usize = 128 * 1024;

/// How often [`Handler::expire_camera`] is polled, and therefore the accuracy
/// of the warm hold. The deadline itself is absolute, so a tick that finds
/// the handler locked loses nothing — the next one releases at the same
/// wall-clock instant, just late (ADR 008 §8).
pub const CAMERA_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Floor on the warm hold a preview frame sets.
///
/// A preview streams at roughly 10 fps through one-frame-per-request D-Bus
/// calls, so it must never reopen the camera between frames — not even with
/// `camera_release_secs = 0`, whose contract is about *authentication* not
/// holding the camera open after a failed attempt. The CLI still calls
/// `ReleaseCamera` on exit; this floor is what bounds a *crashed* preview
/// (ADR 008 §4).
const PREVIEW_MIN_HOLD: Duration = Duration::from_secs(2);

/// What a finished request means for the camera stream.
///
/// The single rule of ADR 008 §1: the camera is on only while an
/// authentication is in progress or a retry is plausibly imminent. A retry is
/// plausible after exactly one class of ending — the user was there and was
/// not recognized — so that is the only one that holds the stream open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// Matched (or enrolled). The interaction is over — the session is
    /// unlocked, the sudo command is running — so this releases the camera
    /// unless the operator opted into a hold with
    /// `device.camera_release_after_success_secs` (default `0`).
    Success,
    /// No match, including the timeout that follows a face the daemon saw but
    /// could not match. The one outcome a retry plausibly follows.
    Failure,
    /// The caller went away, the system is suspending, or a privileged
    /// `ReleaseCamera` arrived. Nobody is waiting on a retry.
    Cancelled,
    /// Anything that was not an answer about this face: a camera or capture
    /// failure, a pre-flight rejection, an all-dark scan. Never held warm —
    /// a broken stream must not be reused (ADR 008 §8).
    Error,
}

impl Outcome {
    /// Every variant, so a table-driven test cannot silently skip one.
    pub const ALL: &'static [Outcome] = &[
        Outcome::Success,
        Outcome::Failure,
        Outcome::Cancelled,
        Outcome::Error,
    ];

    /// How many seconds this ending keeps the stream open for a warm retry —
    /// `0` closes it now. The whole policy, in one function, so
    /// [`CameraLease::finish`] has no rule of its own to drift from it.
    ///
    /// Only two endings can hold, and both are the operator's call by value.
    /// A failure holds for `camera_release_secs` (default 3) because a retry
    /// plausibly follows it. A success holds only for
    /// `camera_release_after_success_secs`, which is `0` by default: a
    /// success ends the interaction, and on IR hardware a hold past it is an
    /// emitter LED burning after the screen has already unlocked. It exists
    /// for the one shape that does retry immediately — privileged actions
    /// repeated with no auth caching in front of them, `sudo` with
    /// `timestamp_timeout=0` — and was added on maintainer request as the
    /// opt-in ADR 008 §3 had deferred.
    ///
    /// A cancellation or an error never holds, whatever either key says:
    /// nobody abandons an attempt in order to retry it a moment later, and a
    /// stream that just failed must never be handed to the next request
    /// (ADR 008 §8).
    pub fn hold_secs(self, device: &DeviceConfig) -> u32 {
        match self {
            Outcome::Failure => device.camera_release_secs,
            Outcome::Success => device.camera_release_after_success_secs,
            Outcome::Cancelled | Outcome::Error => 0,
        }
    }
}

/// The authentication vocabulary's projection onto the camera policy.
///
/// Deliberately exhaustive with no wildcard arm: a new [`AuthOutcome`]
/// variant must be classified here before the daemon compiles, rather than
/// silently inheriting whichever behavior a `_` arm happened to name.
impl From<&AuthOutcome> for Outcome {
    fn from(outcome: &AuthOutcome) -> Self {
        match outcome {
            AuthOutcome::AuthResult(result) if result.matched => Outcome::Success,
            // Includes the timeout: `matched = false` with or without a face.
            AuthOutcome::AuthResult(_) => Outcome::Failure,
            // No enrolled models and `suppress_unknown` — the stack falls
            // through to another module and nobody retries a face here.
            AuthOutcome::Suppressed => Outcome::Error,
            // Every `ErrorKind`, including `AllFramesDark` and the camera and
            // capture failures.
            AuthOutcome::Error { .. } => Outcome::Error,
            AuthOutcome::Cancelled => Outcome::Cancelled,
        }
    }
}

/// Enrollment's projection onto the same policy. An enrollment that ended —
/// stored or failed — is a finished interaction: the CLI prints its result
/// and any re-run is human-paced, so there is no imminent retry to keep the
/// stream open for.
impl From<&EnrollOutcome> for Outcome {
    fn from(outcome: &EnrollOutcome) -> Self {
        match outcome {
            EnrollOutcome::Enrolled { .. } => Outcome::Success,
            EnrollOutcome::Error { .. } => Outcome::Error,
            EnrollOutcome::Cancelled => Outcome::Cancelled,
        }
    }
}

/// Dequeue and throw away `count` frames.
///
/// The one discard both callers use: a cold open discards `warmup_frames`
/// while the sensor's AGC/AE settles, and a warm reuse discards the stale
/// MMAP buffers V4L2 left filled after the previous request. Sharing it is
/// what keeps "the frames a warm stream hands back first are not analyzed"
/// true by construction rather than by two similar loops agreeing.
fn discard_frames<C: CameraSource>(
    camera: &mut C,
    count: u32,
    reason: &str,
    cancel: &CancelToken,
) -> Result<(), String> {
    if count == 0 {
        return Ok(());
    }
    debug!(count, reason, "discarding frames");
    for _ in 0..count {
        // A discard is a blocking capture like any other, so it honors the
        // token too: cancelling during warmup must not wait out the whole
        // ring before the camera closes (ADR 008 §8).
        if cancel.is_cancelled() {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        camera.capture().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The camera and everything that decides when it closes.
///
/// One owner for the open stream and the warm-hold deadline, so the invariant
/// of ADR 008 §8 — *every exit from an active request either sets a deadline
/// or drops the camera* — is a property of four methods rather than of every
/// request arm remembering to update two fields. Nothing outside this struct
/// touches `camera` or `deadline`.
///
/// The cancel token is deliberately *not* a field: it belongs to the request,
/// not to the lease, and outlives neither (see [`crate::cancel`]). It arrives
/// as an argument to [`CameraLease::acquire`], which is the only place here
/// that can block long enough to need one.
pub(crate) struct CameraLease<C: CameraSource> {
    camera: Option<C>,
    /// Absolute instant the warm hold ends. `None` means "not holding" —
    /// either nothing is open, or a request is in flight.
    deadline: Option<Instant>,
    factory: Option<CameraFactory<C>>,
    /// Quirk-overridden warmup frames (takes precedence over config).
    warmup_frames_override: Option<u32>,
}

impl<C: CameraSource> CameraLease<C> {
    fn new(factory: Option<CameraFactory<C>>, warmup_frames_override: Option<u32>) -> Self {
        Self {
            camera: None,
            deadline: None,
            factory,
            warmup_frames_override,
        }
    }

    /// Open the camera (or reuse a warm one) for a request that is about to
    /// run. Clears any warm-hold deadline: from here until `finish`, the
    /// stream belongs to this request. A request whose token is already set
    /// never opens the camera at all.
    ///
    /// `cancel` is this request's token and nobody else's, so a cancellation
    /// seen here is always about the request being served.
    ///
    /// The returned borrow is tied to the lease, not to `config`, so a caller
    /// can still reach the handler's other fields while holding the camera.
    fn acquire<'a>(
        &'a mut self,
        config: &Config,
        cancel: &CancelToken,
    ) -> Result<&'a mut C, String> {
        self.deadline = None;
        if cancel.is_cancelled() {
            self.close("cancelled before the camera was needed");
            return Err(CANCELLED_MESSAGE.to_string());
        }

        if self.camera.is_some() {
            // Warm reuse. The stream never stopped, so its buffers still hold
            // the frames captured right after the previous request — analyzing
            // those would let a fresh attempt match on the tail of the last
            // one. AE is already settled, so no warmup discard is needed.
            let stale = match self.camera.as_mut() {
                Some(camera) => discard_frames(camera, MMAP_BUFFERS - 1, "stale", cancel),
                None => Ok(()),
            };
            if let Err(e) = stale {
                // A cancellation is not a broken stream, and the difference is
                // load-bearing: `CANCELLED_MESSAGE` is matched *exactly* by
                // PAM (→ PAM_IGNORE) and by the caller that writes the
                // `cancelled` audit row, so wrapping it as "capture error:
                // cancelled" would silently downgrade an abandoned attempt to
                // an error on both. Return it verbatim, as the cold-warmup
                // branch below always did (docs/contracts.md, ADR 008 §5).
                if cancel.is_cancelled() {
                    self.close("cancelled during the stale-buffer discard");
                    return Err(CANCELLED_MESSAGE.to_string());
                }
                // A dequeue failure on a warm stream means the stream is gone.
                // Never hand it to a request (ADR 008 §8).
                warn!("warm camera reuse failed, closing camera: {e}");
                self.camera = None;
                return Err(format!("capture error: {e}"));
            }
        } else {
            let factory = self
                .factory
                .as_ref()
                .ok_or_else(|| "no camera available".to_string())?;
            debug!("opening camera");
            let mut camera = factory(config).map_err(|e| format!("failed to open camera: {e}"))?;
            let warmup = self
                .warmup_frames_override
                .unwrap_or(config.device.warmup_frames);
            // A cold warmup discard is advisory (AGC/AE settling); a failure
            // here is left for the scan loop to surface, as it always was.
            if let Err(e) = discard_frames(&mut camera, warmup, "warmup", cancel) {
                debug!("warmup discard failed: {e}");
                // A cancellation during warmup aborts the acquire outright:
                // the request is over before it started (ADR 008 §8).
                if cancel.is_cancelled() {
                    return Err(CANCELLED_MESSAGE.to_string());
                }
            }
            self.camera = Some(camera);
        }

        self.camera
            .as_mut()
            .ok_or_else(|| "no camera available".to_string())
    }

    /// End the request that `acquire` started. Either sets a deadline or
    /// drops the camera — never neither.
    ///
    /// What the outcome is worth in seconds is [`Outcome::hold_secs`]; all
    /// that happens here is spending it.
    fn finish(&mut self, outcome: Outcome, config: &Config) {
        let hold_secs = outcome.hold_secs(&config.device);
        if hold_secs == 0 {
            self.close(match outcome {
                // The default for a success: the interaction is over, so the
                // stream goes out with the reply.
                Outcome::Success => "authentication succeeded",
                // `camera_release_secs = 0` used to be silently substituted
                // with 5 seconds. It now means what it says.
                Outcome::Failure => "camera hold disabled (device.camera_release_secs = 0)",
                Outcome::Cancelled => "request cancelled",
                Outcome::Error => "request ended without an answer",
            });
            return;
        }
        self.deadline = Some(Instant::now() + Duration::from_secs(hold_secs as u64));
        // Only the two holding outcomes reach here, and they hold for
        // opposite reasons — one because a retry is likely, one because the
        // operator said so — so the log says which, and by which key.
        if outcome == Outcome::Success {
            debug!(
                hold_secs,
                "holding camera warm after a successful attempt \
                 (device.camera_release_after_success_secs)"
            );
        } else {
            debug!(
                hold_secs,
                "holding camera warm after a failed attempt (retry expected)"
            );
        }
    }

    /// Extend the hold for a preview stream, which is a sequence of
    /// single-frame requests rather than one long one.
    fn touch_preview(&mut self, config: &Config) {
        let hold =
            Duration::from_secs(config.device.camera_release_secs as u64).max(PREVIEW_MIN_HOLD);
        self.deadline = Some(Instant::now() + hold);
    }

    /// Close the camera if its warm hold has run out. Driven by the daemon's
    /// [`CAMERA_POLL_INTERVAL`] tick against the absolute deadline.
    fn expire(&mut self, now: Instant) {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            self.close("warm hold expired");
        }
    }

    /// Close the camera now — the `ReleaseCamera`, shutdown and suspend
    /// paths.
    ///
    /// Cancelling the request that may be holding the camera is *not* done
    /// here, because it cannot be: this method needs the handler mutex, and a
    /// capture in flight is holding it. The token is set first, lock-free, by
    /// whoever is asking (see `FacelockService::release_camera_as` and the
    /// suspend watcher); by the time this runs, that request has already
    /// returned.
    ///
    /// Nothing needs un-setting afterwards: the token that was cancelled
    /// belongs to the request that has just ended, and the next request
    /// brings its own. This is what used to wedge every later preview frame —
    /// one `ReleaseCamera` latched the daemon-lifetime token, and a `release`
    /// that forgot to clear it left the flag set for everyone after.
    fn release(&mut self) {
        self.deadline = None;
        self.close("camera release requested");
    }

    /// Drop the stream (`Drop` runs STREAMOFF and disables the IR emitter).
    fn close(&mut self, reason: &str) {
        if self.camera.is_some() {
            debug!(reason, "releasing camera");
            self.camera = None;
        }
    }
}

pub struct Handler<C: CameraSource, E: FaceProcessor> {
    pub config: Config,
    pub engine: E,
    pub store: FaceStore,
    pub rate_limiter: RateLimiter,
    /// Capabilities of the resolved (not necessarily open) camera device,
    /// computed once at handler build. `pre_check` gates `require_ir` on
    /// `device_caps.is_ir` *before* any camera is opened; once a camera is
    /// open, its own `capabilities()` are authoritative.
    pub device_caps: facelock_core::types::CameraCaps,
    pub shutdown_requested: bool,
    /// The open camera, its warm-hold deadline and the in-flight cancel
    /// token. The only path to any of the three.
    pub(crate) lease: CameraLease<C>,
    jpeg_buf: Vec<u8>,
    /// Held TPM sealer for `tpm.seal_database` stores. Without the `tpm`
    /// feature this is the passthrough sealer, whose per-row unseal reports a
    /// clear "compile with tpm" error instead of misreading sealed blobs.
    tpm_sealer: Option<facelock_tpm::TpmSealer>,
    software_sealer: Option<facelock_tpm::SoftwareSealer>,
    /// Why the software sealer could not be initialized for a configured
    /// encryption method. `Some` means enroll must fail CLOSED rather than
    /// silently downgrade to plaintext biometric storage (auth is unaffected).
    sealer_init_error: Option<String>,
}

impl<C: CameraSource, E: FaceProcessor> Handler<C, E> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        engine: E,
        store: FaceStore,
        rate_limiter: RateLimiter,
        device_caps: facelock_core::types::CameraCaps,
        camera_factory: Option<CameraFactory<C>>,
        warmup_frames_override: Option<u32>,
    ) -> Result<Self, String> {
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

        // Initialize software sealer based on encryption method. On failure for a
        // method that requires encryption, we record `sealer_init_error` and leave
        // the sealer `None` so ENROLL can fail closed (see `handle`). We do NOT
        // fail the whole handler here: that would take the daemon down and block
        // the auth path, which must keep falling through to password as before.
        let mut sealer_init_error: Option<String> = None;
        let software_sealer = match config.encryption.method {
            EncryptionMethod::Keyfile => {
                let key_path = std::path::Path::new(&config.encryption.key_path);
                // Encrypt-by-default (finding #8): auto-generate the key on first
                // use so a keyfile default actually encrypts. Safe — if a key was
                // lost, any prior encrypted rows were already unreadable, and a new
                // key only affects future writes; plaintext rows stay readable.
                if !key_path.exists() {
                    match facelock_tpm::SoftwareSealer::generate_key_file(key_path) {
                        Ok(()) => info!(
                            "generated encryption key at {} (encrypt-by-default)",
                            key_path.display()
                        ),
                        // Not necessarily fatal on its own; the read-back below is
                        // the authoritative check for whether encryption works.
                        Err(e) => warn!(
                            "failed to auto-generate encryption key at {}: {e}",
                            key_path.display()
                        ),
                    }
                }
                match facelock_tpm::SoftwareSealer::from_key_file(key_path) {
                    Ok(sealer) => {
                        info!(
                            "software encryption sealer initialized from {}",
                            key_path.display()
                        );
                        Some(sealer)
                    }
                    Err(e) => {
                        // Fail CLOSED on enroll: record the cause so `handle`
                        // refuses to enroll rather than silently storing the
                        // biometric template as plaintext (finding: silent
                        // plaintext downgrade).
                        let msg = format!(
                            "{} keyfile could not be created/read: {e}",
                            key_path.display()
                        );
                        warn!(
                            "software encryption sealer unavailable — enroll will be refused: {msg}"
                        );
                        sealer_init_error = Some(msg);
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
            device_caps,
            shutdown_requested: false,
            lease: CameraLease::new(camera_factory, warmup_frames_override),
            jpeg_buf: Vec::with_capacity(JPEG_BUF_CAPACITY),
            tpm_sealer,
            software_sealer,
            sealer_init_error,
        })
    }

    /// Close the camera if its warm hold has run out. Called from the
    /// daemon's [`CAMERA_POLL_INTERVAL`] tick; a tick that finds the handler
    /// locked simply misses, because the deadline is absolute.
    pub fn expire_camera(&mut self, now: Instant) {
        self.lease.expire(now);
    }

    /// Load user embeddings, decrypting TPM-sealed or software-encrypted blobs
    /// through the shared per-row implementation (`crate::embeddings`, N10).
    /// Falls back to the standard `get_user_embeddings` path when nothing could
    /// have written an encrypted row.
    fn load_user_embeddings(
        &mut self,
        user: &str,
    ) -> Result<Vec<(u32, facelock_core::types::FaceEmbedding)>, DaemonResponse> {
        if !crate::embeddings::needs_raw_rows(&self.config, self.software_sealer.is_some()) {
            // Fast path: no encryption, use standard method (no overhead)
            return self
                .store
                .get_user_embeddings(user)
                .map_err(|e| DaemonResponse::Error {
                    message: format!("storage error: {e}"),
                });
        }

        // Slow path: load raw blobs (with each template's device id, for opt-in
        // AAD binding) and decrypt as needed.
        let raw_rows = self
            .store
            .get_user_embeddings_raw_with_device(user)
            .map_err(|e| DaemonResponse::Error {
                message: format!("storage error: {e}"),
            })?;

        crate::embeddings::decrypt_user_embeddings(
            &raw_rows,
            &self.config,
            self.software_sealer.as_ref(),
            crate::embeddings::TpmAccess::Held(self.tpm_sealer.as_mut()),
        )
        .map_err(|message| DaemonResponse::Error { message })
    }

    /// Serve a request that nobody can cancel.
    ///
    /// The fresh token this mints is never handed out, so it is never set:
    /// that is exactly right for the arms that do not scan (`ListModels`,
    /// `ReleaseCamera`, …) and for a preview frame, which is one capture long
    /// and whose successor must not inherit a cancellation from whatever ran
    /// before it. Requests that *can* be cancelled — the two authentications
    /// and enroll — come in through [`Handler::handle_with_cancel`] or
    /// [`Handler::handle_authenticate`] carrying their caller's token.
    pub fn handle(&mut self, request: DaemonRequest) -> DaemonResponse {
        self.handle_with_cancel(request, &CancelToken::new())
    }

    /// [`Handler::handle`], for a request whose caller holds the other end of
    /// `cancel` — the D-Bus server's caller-departure watch, the suspend
    /// path, `ReleaseCamera`, shutdown.
    pub fn handle_with_cancel(
        &mut self,
        request: DaemonRequest,
        cancel: &CancelToken,
    ) -> DaemonResponse {
        debug!(?request, "handling request");
        match request {
            DaemonRequest::Ping => DaemonResponse::Ok,

            DaemonRequest::Shutdown => {
                info!("shutdown requested via IPC");
                self.lease.release();
                self.shutdown_requested = true;
                DaemonResponse::Ok
            }

            DaemonRequest::ReleaseCamera => {
                self.lease.release();
                DaemonResponse::Ok
            }

            DaemonRequest::Authenticate { user } => {
                self.handle_authenticate(user, AuthIntent::Authenticate, cancel)
            }

            DaemonRequest::Enroll { user, label } => {
                // Refuse to enroll a plaintext template unless explicitly opted in.
                if let Err(message) = self.config.ensure_enroll_encryption_allowed() {
                    warn!(user, "enroll refused: {message}");
                    return DaemonResponse::Error { message };
                }
                // Fail CLOSED: an encryption method is configured but its sealer
                // could not be initialized (e.g. keyfile IO/permission error).
                // Refuse to enroll rather than silently storing the biometric
                // template as plaintext. This is enroll-only — the auth path is
                // untouched and keeps falling through to password as before. The
                // legitimate `method = "none"` + `allow_plaintext` path is handled
                // above and never reaches here (its sealer is intentionally None).
                if self.config.encryption.method != EncryptionMethod::None
                    && self.software_sealer.is_none()
                {
                    let cause = self.sealer_init_error.clone().unwrap_or_else(|| {
                        "the configured encryption sealer could not be initialized".to_string()
                    });
                    let message = format!(
                        "refusing to enroll: {cause}. Storing your face would otherwise fall \
                         back to plaintext. Fix the keyfile path/permissions (or set \
                         encryption.method = \"none\" with security.allow_plaintext = true to \
                         intentionally store plaintext)."
                    );
                    warn!(user, "enroll refused (encryption unavailable): {message}");
                    return DaemonResponse::Error { message };
                }
                let camera = match self.lease.acquire(&self.config, cancel) {
                    Ok(camera) => camera,
                    Err(message) => return DaemonResponse::Error { message },
                };
                // The enrolling camera's own identity — asked of the camera
                // actually recording the template, not a handler-level copy.
                let device_id = camera.capabilities().fingerprint.canonical_for_storage();
                let result = enroll::enroll(
                    camera,
                    &mut self.engine,
                    &self.store,
                    &self.config,
                    &user,
                    &label,
                    self.software_sealer.as_ref(),
                    device_id.as_deref(),
                    cancel,
                );
                self.lease.finish(Outcome::from(&result), &self.config);
                result.into()
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
                use facelock_camera::{IrSource, QuirksDb, classify_ir_sources, list_devices};
                // Consult the quirks DB so the reported is_ir matches the
                // authoritative decision the auth path makes, with node-level
                // disambiguation for multi-node USB devices.
                let quirks = QuirksDb::load();
                match list_devices() {
                    Ok(devices) => {
                        let sources = classify_ir_sources(&devices, Some(&quirks));
                        DaemonResponse::Devices(
                            devices
                                .iter()
                                .zip(&sources)
                                .map(|(d, source)| facelock_core::ipc::IpcDeviceInfo {
                                    path: d.path.clone(),
                                    name: d.name.clone(),
                                    driver: d.driver.clone(),
                                    is_ir: *source != IrSource::None,
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
                        )
                    }
                    Err(e) => DaemonResponse::Error {
                        message: format!("device enumeration failed: {e}"),
                    },
                }
            }

            DaemonRequest::PreviewFrame => {
                let captured = match self.lease.acquire(&self.config, cancel) {
                    Ok(camera) => camera.capture_rgb_only(),
                    Err(message) => return DaemonResponse::Error { message },
                };
                match captured {
                    Ok(frame) => {
                        self.lease.touch_preview(&self.config);
                        self.encode_frame_response(&frame.rgb, frame.width, frame.height)
                    }
                    Err(e) => {
                        self.lease.finish(Outcome::Error, &self.config);
                        DaemonResponse::Error {
                            message: format!("capture error: {e}"),
                        }
                    }
                }
            }

            DaemonRequest::PreviewDetectFrame { user } => {
                let captured = match self.lease.acquire(&self.config, cancel) {
                    Ok(camera) => camera.capture(),
                    Err(message) => return DaemonResponse::Error { message },
                };
                let frame = match captured {
                    Ok(frame) => {
                        self.lease.touch_preview(&self.config);
                        frame
                    }
                    Err(e) => {
                        self.lease.finish(Outcome::Error, &self.config);
                        return DaemonResponse::Error {
                            message: format!("capture error: {e}"),
                        };
                    }
                };
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
        }
    }

    /// Run an authentication, enforced and audited according to `intent`.
    ///
    /// [`AuthIntent::Authenticate`] — every real authentication, whatever the
    /// caller's privilege — runs every gate and charges the shared
    /// (SQLite-backed) rate-limit budget on a failed attempt.
    /// [`AuthIntent::Test`] is reached only from the root-only
    /// `TestAuthenticate` D-Bus method: it skips the SSH/lid gates and
    /// charges nothing.
    ///
    /// The rate-limit *check* (whether `user` is already over budget) is
    /// unaffected by the intent: an already-limited user still sees "rate
    /// limited" from `test`, because that decision is made before this
    /// function knows whether the attempt itself will fail.
    pub fn handle_authenticate(
        &mut self,
        user: String,
        intent: AuthIntent,
        cancel: &CancelToken,
    ) -> DaemonResponse {
        if let Some(resp) = auth::pre_check_audited_with_context(
            &self.config,
            &self.store,
            &user,
            &self.rate_limiter,
            &self.device_caps,
            intent.audit_source(),
            intent.pre_check_context(),
        ) {
            return resp.into();
        }

        // A storage failure here must surface as an error, never fold into an
        // empty model list (C3, issue #105): empty `models` means an empty
        // device-allowed set, a guaranteed "no match", and a rate-limit charge
        // for an attempt the user never got to make — retries then walk
        // straight into a lockout. Matches what `pre_check` already returns
        // for the same failure class, and runs before the camera is touched.
        let models = match self.store.list_models(&user) {
            Ok(m) => m,
            Err(e) => {
                return DaemonResponse::Error {
                    message: format!("storage error: {e}"),
                };
            }
        };

        // Pre-load and decrypt embeddings (handles TPM + software encryption)
        // *before* the camera opens: nothing here needs a frame, and every
        // millisecond between `open` and the first analyzed frame is LED-on
        // time the user reads as a strobe (ADR 008 §1).
        let mut stored = match self.load_user_embeddings(&user) {
            Ok(s) => s,
            Err(resp) => return resp,
        };

        let attempt_started = Instant::now();
        let camera = match self.lease.acquire(&self.config, cancel) {
            Ok(camera) => camera,
            Err(message) => {
                // The callee below is what normally wipes `stored` (D11);
                // on this path it never runs, so wipe it here.
                facelock_core::types::zeroize_stored_embeddings(&mut stored);
                // A cancellation that lands before the camera is open is
                // still a cancellation, and docs/contracts.md promises every
                // one of them an audit row. Returning the error straight from
                // here skipped the writer entirely, so the trail recorded
                // nothing at all for the fastest cancellations — the ones a
                // locker that aborts PAM the instant a password is typed
                // produces most often. Same writer `auth.rs` uses when the
                // token is noticed mid-scan; `frame_count` is 0 because none
                // were captured.
                if message == CANCELLED_MESSAGE {
                    return auth::cancelled(
                        &self.config,
                        &user,
                        intent.audit_source(),
                        attempt_started,
                        0,
                    )
                    .into();
                }
                return DaemonResponse::Error { message };
            }
        };
        // `stored` is wiped by the callee (D11) — this plaintext set must not
        // be read again below.
        let result = auth::authenticate_with_embeddings(
            camera,
            &mut self.engine,
            &mut stored,
            &models,
            &self.config,
            &user,
            intent.audit_source(),
            cancel,
        );
        self.lease.finish(Outcome::from(&result), &self.config);
        // Only failed auths count against the rate limit, and only for the
        // real-authentication intent (see [`AuthIntent`]).
        //
        // An attempt where the camera never saw a face is not one of them: an
        // empty chair is not a guess (ADR 008 §4). A locker that starts face
        // auth on every wake, or a laptop opened in front of nobody, would
        // otherwise burn the user's whole budget without a single attempt
        // being made — and the real one, when they sit down, meets a lockout.
        // A face that *was* seen and did not match still charges: that is a
        // guess, and a wrong one.
        if let AuthOutcome::AuthResult(ref mr) = result
            && !mr.matched
            && mr.face_detected
            && intent.charges_rate_limit()
            && let Err(e) = self.rate_limiter.record_failure(&self.store, &user)
        {
            warn!(user, error = %e, "failed to record auth failure");
        }
        result.into()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The one table that decides what an authentication costs and how it is
    /// enforced. Real authentication charges the rate limit; the diagnostic
    /// intent does not. Nothing here consults the caller's UID — that
    /// inference is exactly what let a failed face auth at a `sudo` prompt
    /// (setuid-root, so UID 0 at the daemon) escape the limiter.
    #[test]
    fn only_real_authentication_charges_the_rate_limit() {
        assert!(AuthIntent::Authenticate.charges_rate_limit());
        assert!(!AuthIntent::Test.charges_rate_limit());
    }

    /// The SSH/lid physical-presence gates are skippable for the diagnostic
    /// intent only (N11); every other gate applies to both.
    #[test]
    fn only_the_test_intent_skips_the_ssh_and_lid_gates() {
        let real = AuthIntent::Authenticate.pre_check_context();
        assert!(!real.skip_ssh_gate);
        assert!(!real.skip_lid_gate);

        let test = AuthIntent::Test.pre_check_context();
        assert!(test.skip_ssh_gate);
        assert!(test.skip_lid_gate);
    }

    /// The audit `source` names the enforcement path that ran, so the two
    /// intents must never share a stamp.
    #[test]
    fn each_intent_stamps_its_own_audit_source() {
        assert_eq!(AuthIntent::Authenticate.audit_source(), AuditSource::Daemon);
        assert_eq!(AuthIntent::Test.audit_source(), AuditSource::Test);
    }

    // -----------------------------------------------------------------
    // CameraLease (ADR 008 §4, §9)
    // -----------------------------------------------------------------

    use crate::auth::ErrorKind;
    use facelock_core::types::MatchResult;
    use facelock_test_support::MockCamera;

    /// How many frames the mock camera can serve before it wraps (and its
    /// capture counter resets), kept far above anything these tests take.
    const MOCK_FRAMES: usize = 256;

    fn lease_config(release_secs: u32) -> Config {
        lease_config_with_success_hold(release_secs, 0)
    }

    /// The same, plus the opt-in success hold — `0` in every test that does
    /// not name it, which is both the shipped default and the behavior every
    /// pre-existing test here was written against.
    fn lease_config_with_success_hold(release_secs: u32, success_secs: u32) -> Config {
        let mut config = Config::default();
        config.device.camera_release_secs = release_secs;
        config.device.camera_release_after_success_secs = success_secs;
        config
    }

    /// The hold pairs worth running a whole outcome table against:
    /// `(camera_release_secs, camera_release_after_success_secs)` — the
    /// shipped default, the maintainer's opt-in, the opt-in with the failure
    /// hold turned off, and both off.
    const HOLD_PAIRS: &[(u32, u32)] = &[(3, 0), (3, 2), (0, 2), (0, 0)];

    fn lease() -> CameraLease<MockCamera> {
        CameraLease::new(
            Some(Box::new(|_| Ok(MockCamera::bright(64, 64, MOCK_FRAMES)))),
            None,
        )
    }

    /// The token a request that nobody cancels would bring. Named so the
    /// tests below read as "acquire for a live request" rather than as an
    /// argument nobody looked at.
    fn live() -> CancelToken {
        CancelToken::new()
    }

    fn match_result(matched: bool) -> MatchResult {
        MatchResult {
            matched,
            model_id: matched.then_some(1),
            label: None,
            similarity: if matched { 0.9 } else { 0.1 },
            face_detected: true,
            failure_reason: None,
        }
    }

    /// The classification table of ADR 008 §4, over **every** shape an
    /// authentication can end in — including every `ErrorKind`, so a new
    /// rejection class cannot quietly inherit a camera policy nobody chose.
    ///
    /// The `From` impl it exercises has no wildcard arm, so a new
    /// `AuthOutcome` variant fails to compile until it is classified here.
    #[test]
    fn every_auth_outcome_classifies_into_the_camera_policy() {
        assert_eq!(
            Outcome::from(&AuthOutcome::AuthResult(match_result(true))),
            Outcome::Success
        );
        assert_eq!(
            Outcome::from(&AuthOutcome::AuthResult(match_result(false))),
            Outcome::Failure
        );
        assert_eq!(Outcome::from(&AuthOutcome::Suppressed), Outcome::Error);
        // Nobody is waiting on a retry they abandoned, so a cancellation
        // closes the stream rather than holding it — the row this table was
        // missing while `Cancelled` was the newest variant.
        assert_eq!(Outcome::from(&AuthOutcome::Cancelled), Outcome::Cancelled);
        for kind in ErrorKind::ALL {
            assert_eq!(
                Outcome::from(&AuthOutcome::error(*kind)),
                Outcome::Error,
                "{kind:?} must never hold the camera open"
            );
        }
    }

    /// Enrollment's half of the same table.
    #[test]
    fn every_enroll_outcome_classifies_into_the_camera_policy() {
        assert_eq!(
            Outcome::from(&EnrollOutcome::Enrolled {
                model_id: 1,
                embedding_count: 3
            }),
            Outcome::Success
        );
        assert_eq!(
            Outcome::from(&EnrollOutcome::Error {
                message: "boom".into()
            }),
            Outcome::Error
        );
        assert_eq!(Outcome::from(&EnrollOutcome::Cancelled), Outcome::Cancelled);
    }

    /// The whole policy, over every outcome and every combination of the two
    /// hold keys: a failure holds for `camera_release_secs`, a success for
    /// `camera_release_after_success_secs` (`0` unless asked for), and
    /// nothing makes a cancellation or an error hold.
    #[test]
    fn hold_secs_is_the_whole_camera_policy() {
        for (failure_secs, success_secs) in HOLD_PAIRS {
            let config = lease_config_with_success_hold(*failure_secs, *success_secs);
            for outcome in Outcome::ALL {
                let expected = match outcome {
                    Outcome::Failure => *failure_secs,
                    Outcome::Success => *success_secs,
                    Outcome::Cancelled | Outcome::Error => 0,
                };
                assert_eq!(
                    outcome.hold_secs(&config.device),
                    expected,
                    "{outcome:?} with camera_release_secs = {failure_secs}, \
                     camera_release_after_success_secs = {success_secs}"
                );
            }
        }
    }

    /// The default an install that never touches the key gets: a success
    /// closes the stream as the reply goes out, exactly as it did before the
    /// key existed.
    #[test]
    fn success_closes_the_camera_by_default() {
        let config = lease_config(3);
        assert_eq!(config.device.camera_release_after_success_secs, 0);
        let mut lease = lease();
        lease.acquire(&config, &live()).expect("mock camera opens");
        lease.finish(Outcome::Success, &config);
        assert!(lease.camera.is_none(), "a success must release the camera");
        assert!(lease.deadline.is_none());
    }

    /// Opted in, a success holds on the same machinery a failure does: an
    /// absolute deadline, a stream the next request reuses warm, and an
    /// `expire` that closes it once the deadline passes.
    #[test]
    fn a_configured_success_hold_keeps_the_camera_warm_and_then_expires() {
        let config = lease_config_with_success_hold(3, 2);
        let warmup = config.device.warmup_frames;
        let mut lease = lease();
        lease.acquire(&config, &live()).expect("mock camera opens");

        let at_finish = Instant::now();
        lease.finish(Outcome::Success, &config);
        let deadline = lease.deadline.expect("the success hold must set one");
        assert!(
            deadline >= at_finish + Duration::from_secs(2),
            "the hold must run the configured 2 s"
        );
        assert!(
            deadline < at_finish + Duration::from_secs(3),
            "and must not borrow the failure key's 3 s"
        );
        assert!(
            lease.camera.is_some(),
            "the stream stays open for the retry"
        );

        // And it is a real warm stream: the next request reuses it, paying
        // the stale-buffer discard and no second warmup.
        lease
            .acquire(&config, &live())
            .expect("the next request reuses the warm camera");
        assert_eq!(
            lease.camera.as_ref().map(|c| c.captures()),
            Some((warmup + MMAP_BUFFERS - 1) as usize),
            "a warm success hold must not reopen the camera"
        );

        // Then it ends, on its deadline, like any other hold.
        let at_second_finish = Instant::now();
        lease.finish(Outcome::Success, &config);
        lease.expire(at_second_finish + Duration::from_secs(2) - CAMERA_POLL_INTERVAL);
        assert!(lease.camera.is_some(), "released a full tick early");
        lease.expire(at_second_finish + Duration::from_secs(2) + CAMERA_POLL_INTERVAL);
        assert!(lease.camera.is_none());
        assert!(lease.deadline.is_none());
    }

    /// The success hold holds successes and nothing else: an abandoned
    /// attempt and a broken stream still close immediately with it set, so
    /// the key can never become a way to leave the IR emitter lit after a
    /// cancel (ADR 008 §1, §8).
    #[test]
    fn a_success_hold_never_leaks_into_a_cancellation_or_an_error() {
        let config = lease_config_with_success_hold(3, 30);
        for outcome in [Outcome::Cancelled, Outcome::Error] {
            let mut lease = lease();
            lease.acquire(&config, &live()).expect("mock camera opens");
            lease.finish(outcome, &config);
            assert!(
                lease.camera.is_none(),
                "{outcome:?} must close the stream even with a success hold set"
            );
            assert!(lease.deadline.is_none(), "{outcome:?}");
        }
    }

    /// The invariant of ADR 008 §8: every exit from an active request either
    /// sets a deadline or drops the camera — never neither, never both.
    #[test]
    fn finish_either_sets_a_deadline_or_drops_the_camera() {
        for (failure_secs, success_secs) in HOLD_PAIRS {
            let config = lease_config_with_success_hold(*failure_secs, *success_secs);
            for outcome in Outcome::ALL {
                let mut lease = lease();
                lease.acquire(&config, &live()).expect("mock camera opens");
                lease.finish(*outcome, &config);
                let case = format!(
                    "{outcome:?} with camera_release_secs = {failure_secs}, \
                     camera_release_after_success_secs = {success_secs}"
                );
                if outcome.hold_secs(&config.device) > 0 {
                    assert!(lease.camera.is_some(), "{case} must keep the stream");
                    assert!(lease.deadline.is_some(), "{case} must set a deadline");
                } else {
                    assert!(lease.camera.is_none(), "{case} must close the stream");
                    assert!(lease.deadline.is_none(), "{case} must clear the hold");
                }
            }
        }
    }

    /// `camera_release_secs = 0` means never hold. It used to be silently
    /// substituted with 5 seconds, which is why honoring it is a fix.
    #[test]
    fn zero_release_secs_closes_the_camera_even_on_failure() {
        let config = lease_config(0);
        let mut lease = lease();
        lease.acquire(&config, &live()).expect("mock camera opens");
        lease.finish(Outcome::Failure, &config);
        assert!(lease.camera.is_none());
        assert!(lease.deadline.is_none());
    }

    /// The hold runs to its deadline and not past it. The deadline is
    /// absolute, so a tick that arrives late still releases at the right
    /// wall-clock instant rather than restarting the clock.
    #[test]
    fn expire_closes_the_camera_only_after_the_deadline() {
        let config = lease_config(3);
        let mut lease = lease();
        lease.acquire(&config, &live()).expect("mock camera opens");
        let at_finish = Instant::now();
        lease.finish(Outcome::Failure, &config);

        // One poll tick before the deadline: still warm.
        lease.expire(at_finish + Duration::from_secs(3) - CAMERA_POLL_INTERVAL);
        assert!(lease.camera.is_some(), "released a full tick early");

        // One tick after: closed, and the hold is cleared with it.
        lease.expire(at_finish + Duration::from_secs(3) + CAMERA_POLL_INTERVAL);
        assert!(lease.camera.is_none());
        assert!(lease.deadline.is_none());
    }

    /// A warm reuse discards exactly the frames V4L2 left in the ring and
    /// runs no warmup: AE is already settled, and the stale buffers are the
    /// only thing standing between this attempt and the previous one's tail.
    #[test]
    fn warm_reuse_discards_the_stale_buffers_and_skips_warmup() {
        let config = lease_config(3);
        let warmup = config.device.warmup_frames;
        let mut lease = lease();

        lease.acquire(&config, &live()).expect("mock camera opens");
        let after_cold = lease.camera.as_ref().map(|c| c.captures());
        assert_eq!(after_cold, Some(warmup as usize), "cold open runs warmup");

        lease.finish(Outcome::Failure, &config);
        assert!(lease.camera.is_some(), "a failure holds the stream");

        lease
            .acquire(&config, &live())
            .expect("warm camera is reused");
        let after_warm = lease.camera.as_ref().map(|c| c.captures());
        assert_eq!(
            after_warm,
            Some((warmup + MMAP_BUFFERS - 1) as usize),
            "warm reuse must discard exactly MMAP_BUFFERS - 1 frames and no warmup"
        );
    }

    /// A preview is a stream of one-frame requests at ~10 fps, so its hold
    /// has a floor that `camera_release_secs = 0` cannot drop below —
    /// otherwise every frame would reopen the camera.
    #[test]
    fn preview_holds_the_camera_for_at_least_the_floor() {
        let config = lease_config(0);
        let mut lease = lease();
        lease.acquire(&config, &live()).expect("mock camera opens");
        let before = Instant::now();
        lease.touch_preview(&config);

        let deadline = lease.deadline.expect("preview must set a hold");
        assert!(
            deadline >= before + PREVIEW_MIN_HOLD,
            "preview hold fell below the floor with camera_release_secs = 0"
        );
        // And it really is a hold: a tick inside the floor leaves it open.
        lease.expire(before + PREVIEW_MIN_HOLD - CAMERA_POLL_INTERVAL);
        assert!(lease.camera.is_some());
    }

    /// A larger `camera_release_secs` raises the preview hold rather than
    /// being clamped to the floor.
    #[test]
    fn preview_hold_takes_the_larger_of_the_floor_and_the_configured_hold() {
        let config = lease_config(10);
        let mut lease = lease();
        lease.acquire(&config, &live()).expect("mock camera opens");
        let before = Instant::now();
        lease.touch_preview(&config);
        let deadline = lease.deadline.expect("preview must set a hold");
        assert!(deadline >= before + Duration::from_secs(10));
    }

    /// `ReleaseCamera`, suspend and shutdown all land here. The camera goes
    /// at once, and the *next* request is unaffected: the token whoever asked
    /// had set belongs to the request that was in flight, and the next request
    /// brings its own. A shared token that `release` forgot to clear is how a
    /// single `ReleaseCamera` used to wedge every later preview frame.
    #[test]
    fn release_closes_the_camera_and_never_reaches_the_next_request() {
        let config = lease_config(3);
        let mut lease = lease();
        let in_flight = live();
        lease
            .acquire(&config, &in_flight)
            .expect("mock camera opens");

        in_flight.cancel();
        lease.release();
        assert!(lease.camera.is_none());
        assert!(lease.deadline.is_none());

        // ...and the next request, with its own token, still works.
        lease
            .acquire(&config, &live())
            .expect("a later request still opens");
        assert!(lease.camera.is_some());
    }

    /// The other half of the token contract: a request that finds its own
    /// token set never opens the camera at all (ADR 008 §8).
    #[test]
    fn a_cancelled_request_never_opens_the_camera() {
        let config = lease_config(3);
        let mut lease = lease();
        let cancelled = live();
        cancelled.cancel();
        let err = match lease.acquire(&config, &cancelled) {
            Ok(_) => panic!("a cancelled request must not get a camera"),
            Err(e) => e,
        };
        assert_eq!(err, CANCELLED_MESSAGE);
        assert!(lease.camera.is_none());
    }

    /// A cancellation during the *warm* stale discard must reach the caller
    /// as the frozen `cancelled` string, byte for byte.
    ///
    /// It used to arrive as `"capture error: cancelled"`, which nothing
    /// matches: PAM compares this message exactly (→ `PAM_IGNORE`), and so
    /// does the caller that writes the `cancelled` audit row. Wrapped, an
    /// abandoned retry was answered and logged as a camera error instead —
    /// and only on the warm path, so the same cancellation meant two
    /// different things depending on whether the previous attempt had failed.
    #[test]
    fn a_cancellation_during_the_stale_discard_is_reported_verbatim() {
        let config = lease_config(3);
        let mut lease = lease();

        // Get warm: only a failed attempt leaves the stream open.
        lease.acquire(&config, &live()).expect("mock camera opens");
        lease.finish(Outcome::Failure, &config);
        assert!(lease.camera.is_some(), "a failure holds the stream");

        let cancelled = live();
        cancelled.cancel();
        let err = match lease.acquire(&config, &cancelled) {
            Ok(_) => panic!("a cancelled request must not get a warm camera either"),
            Err(e) => e,
        };
        assert_eq!(
            err, CANCELLED_MESSAGE,
            "a warm-path cancellation must not be wrapped as a capture error"
        );
        assert!(
            lease.camera.is_none(),
            "a cancelled request closes the stream (ADR 008 §4)"
        );
    }

    /// The regression the per-request token exists to make impossible: one
    /// request's cancellation is invisible to the next, so a cancelled
    /// authentication cannot wedge whatever runs after it.
    #[test]
    fn a_cancelled_request_does_not_cancel_the_next_one() {
        let config = lease_config(3);
        let mut lease = lease();

        let cancelled = live();
        cancelled.cancel();
        assert!(lease.acquire(&config, &cancelled).is_err());
        lease.finish(Outcome::Cancelled, &config);

        lease
            .acquire(&config, &live())
            .expect("the request after a cancelled one must still open the camera");
        assert!(lease.camera.is_some());
    }

    /// A camera factory that cannot open is an error, never a warm state.
    #[test]
    fn a_failed_open_leaves_nothing_open() {
        let config = lease_config(3);
        let mut lease: CameraLease<MockCamera> =
            CameraLease::new(Some(Box::new(|_| Err("no such device".into()))), None);
        let err = match lease.acquire(&config, &live()) {
            Ok(_) => panic!("a factory that returns Err must not yield a camera"),
            Err(e) => e,
        };
        assert!(err.contains("failed to open camera"));
        assert!(lease.camera.is_none());
        assert!(lease.deadline.is_none());
    }
}
