use std::path::Path;
use std::time::Instant;

use facelock_camera::capture::is_dark_with_config;
use facelock_camera::preprocess::check_ir_texture;
use facelock_core::config::{Config, SnapshotConfig};
use facelock_core::fs_security::{ensure_dir, ensure_private_dir, write_file};
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_core::types::{
    AuthFailureReason, CameraCaps, FaceEmbedding, Frame, FrameVarianceWindow, MatchResult,
    best_match, device_allowed_model_ids, zeroize_stored_embeddings,
};
use facelock_store::FaceStore;
use image::codecs::jpeg::JpegEncoder;
use nix::unistd::Uid;
use tracing::{debug, info, warn};

use crate::audit::{self, AuditEntry, AuditSource};
use crate::cancel::CancelToken;
use crate::liveness::LandmarkTracker;
use crate::rate_limit::RateLimiter;

/// The class of a recoverable authentication rejection — the discriminant
/// every consumer switches on.
///
/// It exists because the same English sentence used to be four things at
/// once: the user-facing message, the audit `result` label, the PAM wire
/// discriminant, and the oneshot exit-code selector. Four independent
/// substring matchers read it, so rewording a message silently changed PAM
/// policy and mislabelled the audit trail. The class now travels as a type
/// and the prose is *rendered* from it at the boundary, never parsed back
/// out of it.
///
/// [`ErrorKind::render`] is the single producer of that prose, and two of the
/// strings it renders are **frozen protocol**: PAM substring-matches
/// "rate limited" and "IR camera required" to pick `PAM_AUTH_ERR` vs
/// `PAM_IGNORE` (`crates/pam-facelock/src/lib.rs`), and
/// `tests/server_authz.rs` pins them byte-exactly. They are documented in
/// docs/contracts.md. Change the rendering of any other class freely; changing
/// those two is a protocol break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// `security.disabled` is set.
    Disabled,
    /// `security.abort_if_ssh` and the caller is on an SSH session.
    SshSession,
    /// `security.abort_if_lid_closed` and the lid is closed.
    LidClosed,
    /// The face store could not be read. Carries the underlying error.
    Storage,
    /// The user has exhausted their face-auth budget. **Frozen wire string.**
    RateLimited,
    /// The rate-limit *check itself* failed (a storage fault, not a
    /// rejection). Deliberately distinct from [`ErrorKind::RateLimited`]:
    /// PAM must not read a broken limiter as a deliberate lockout. Carries
    /// the underlying error.
    RateLimitCheckFailed,
    /// `security.require_ir` and the resolved device is not IR.
    /// **Frozen wire string.**
    IrRequired,
    /// Every captured frame was below the darkness threshold — the camera
    /// produced no usable image, which is not a non-match.
    AllFramesDark,
    /// A class this build does not name. Only [`ErrorKind::classify`]
    /// produces it, for a message from a daemon of a different version.
    /// Carries that message verbatim.
    Internal,
}

impl ErrorKind {
    /// Every variant, so a table-driven test cannot silently skip one.
    pub const ALL: &'static [ErrorKind] = &[
        ErrorKind::Disabled,
        ErrorKind::SshSession,
        ErrorKind::LidClosed,
        ErrorKind::Storage,
        ErrorKind::RateLimited,
        ErrorKind::RateLimitCheckFailed,
        ErrorKind::IrRequired,
        ErrorKind::AllFramesDark,
        ErrorKind::Internal,
    ];

    /// The exact user- and wire-facing text for this class.
    ///
    /// `detail` is the interpolated underlying error for the classes that
    /// carry one ([`Storage`](ErrorKind::Storage),
    /// [`RateLimitCheckFailed`](ErrorKind::RateLimitCheckFailed),
    /// [`Internal`](ErrorKind::Internal)); the fixed-text classes ignore it.
    /// This is the *only* place any of these sentences is written.
    pub fn render(self, detail: &str) -> String {
        match self {
            ErrorKind::Disabled => "facelock is disabled".to_string(),
            ErrorKind::SshSession => "SSH session detected".to_string(),
            ErrorKind::LidClosed => "lid closed".to_string(),
            ErrorKind::Storage => format!("storage error: {detail}"),
            ErrorKind::RateLimited => "rate limited".to_string(),
            ErrorKind::RateLimitCheckFailed => format!("rate limit check failed: {detail}"),
            ErrorKind::IrRequired => "IR camera required for authentication. Set security.require_ir = false to override (NOT RECOMMENDED).".to_string(),
            ErrorKind::AllFramesDark => "all frames dark".to_string(),
            ErrorKind::Internal => detail.to_string(),
        }
    }

    /// The audit log's `result` field for this class. Was
    /// `message.contains("rate limited")`.
    ///
    /// Exhaustive on purpose: a new class must be given a label here, not
    /// silently fall into `"error"`.
    pub fn audit_result(self) -> &'static str {
        match self {
            ErrorKind::RateLimited => "rate_limited",
            ErrorKind::Disabled
            | ErrorKind::SshSession
            | ErrorKind::LidClosed
            | ErrorKind::Storage
            | ErrorKind::RateLimitCheckFailed
            | ErrorKind::IrRequired
            | ErrorKind::AllFramesDark
            | ErrorKind::Internal => "error",
        }
    }

    /// Recover the class from a message that crossed the D-Bus wire.
    ///
    /// The wire has no room for the discriminant (`AuthResult` carries only
    /// `matched`/`model_id`/`label`/`similarity`, and its signature is
    /// frozen), so the CLI's client rebuilds the class from the rendered
    /// text. This is the inverse of [`ErrorKind::render`] and the *only*
    /// remaining place a rejection message is matched as text on this side of
    /// the wire — `daemon_error_classify_inverts_render` pins the round trip.
    ///
    /// Unrecognized text is [`ErrorKind::Internal`], which is what a message
    /// from an older or newer daemon lands on.
    pub fn classify(message: &str) -> ErrorKind {
        for kind in ErrorKind::ALL {
            match kind {
                // Detail-carrying classes match on their rendered prefix.
                ErrorKind::Storage | ErrorKind::RateLimitCheckFailed => {
                    let prefix = kind.render("");
                    if message.starts_with(&prefix) {
                        return *kind;
                    }
                }
                // `Internal` renders the detail verbatim, so it has no
                // recognizable form; it is the fallback below.
                ErrorKind::Internal => {}
                _ => {
                    if message == kind.render("") {
                        return *kind;
                    }
                }
            }
        }
        ErrorKind::Internal
    }
}

/// The outcome of an authentication attempt or its pre-flight gates — the
/// vocabulary every transport shares. The daemon handler maps it onto its
/// wire response; the direct path (`facelock test`, oneshot `facelock auth`)
/// and the D-Bus client consume it as-is, so the CLI never needs the
/// handler's own request/response enums (D5).
#[derive(Debug, Clone)]
pub enum AuthOutcome {
    /// The comparison loop ran (or a gate produced a definitive non-match).
    AuthResult(MatchResult),
    /// No enrolled models and `suppress_unknown` is enabled.
    Suppressed,
    /// A recoverable error (rate limited, IR required, storage/camera
    /// failure) that must not read as "no match".
    ///
    /// `kind` is what consumers switch on. `message` is its rendering, kept
    /// alongside because it is what crosses the D-Bus wire and what the user
    /// sees; nothing on this side of the wire may re-derive `kind` from it.
    Error { kind: ErrorKind, message: String },
    /// The caller went away, the system is suspending, or the process was
    /// signalled: the attempt was abandoned, not answered (ADR 008 §5).
    ///
    /// Deliberately **not** an [`ErrorKind`]. A rejection class is a
    /// statement about this user's face; a cancellation is the absence of
    /// one. It charges no rate limit, audits as `cancelled`, and reaches the
    /// wire through the recoverable-error encoding as
    /// [`CANCELLED_MESSAGE`].
    Cancelled,
}

/// The wire and log rendering of [`AuthOutcome::Cancelled`]. **Frozen
/// protocol**: the PAM module substring-matches it to choose `PAM_IGNORE`
/// (`crates/pam-facelock/src/lib.rs`), exactly as it does for the two frozen
/// [`ErrorKind`] strings, and it cannot link this crate to share the
/// constant (its dependency ceiling is libc/toml/serde/zbus). It is also the
/// audit log's `result` label for an abandoned attempt. Documented in
/// docs/contracts.md.
pub const CANCELLED_MESSAGE: &str = "cancelled";

impl AuthOutcome {
    /// A rejection of a class whose message is fixed.
    pub fn error(kind: ErrorKind) -> Self {
        Self::error_with(kind, "")
    }

    /// A rejection of a class that interpolates an underlying error
    /// ([`Storage`](ErrorKind::Storage),
    /// [`RateLimitCheckFailed`](ErrorKind::RateLimitCheckFailed),
    /// [`Internal`](ErrorKind::Internal)).
    pub fn error_with(kind: ErrorKind, detail: impl std::fmt::Display) -> Self {
        let message = kind.render(&detail.to_string());
        Self::Error { kind, message }
    }
}

/// Which of [`pre_check`]'s environment gates a caller may skip.
///
/// The only intended consumer is `facelock test` (N11, issue #96): it is
/// root-only, and an admin legitimately runs it over SSH or with the lid
/// closed on a docked laptop while diagnosing recognition, which is exactly
/// what `abort_if_ssh`/`abort_if_lid_closed` exist to stop an *attacker* from
/// doing. Every other gate in `pre_check` (disabled, enrollment,
/// rate-limiting, `require_ir`) still applies to `test` unchanged — this
/// struct exists so that carve-out is explicit at every call site instead of
/// a parallel copy of the gate logic (see #95, which this whole `pre_check`
/// unification closes).
#[derive(Clone, Copy, Debug, Default)]
pub struct PreCheckContext {
    pub skip_ssh_gate: bool,
    pub skip_lid_gate: bool,
}

impl PreCheckContext {
    /// The default, fully-enforced context every real authentication path
    /// (daemon `Authenticate`, oneshot `facelock auth`) uses.
    pub fn enforced() -> Self {
        Self::default()
    }

    /// `facelock test`'s context (N11): skip the SSH/lid gates, keep
    /// everything else enforced.
    pub fn test() -> Self {
        Self {
            skip_ssh_gate: true,
            skip_lid_gate: true,
        }
    }
}

/// Run pre-flight checks that don't need the camera, fully enforced.
/// Returns Some(response) to short-circuit, or None to proceed with auth.
///
/// `caps` are the *resolved* device's capabilities ([`CameraCaps`]), computed
/// without opening a camera stream — the `require_ir` gate must run before
/// any camera (and its indicator LED) is touched, so this is the one boundary
/// where caps arrive as a parameter instead of being asked of an open camera.
pub fn pre_check(
    config: &Config,
    store: &FaceStore,
    user: &str,
    rate_limiter: &RateLimiter,
    caps: &CameraCaps,
) -> Option<AuthOutcome> {
    pre_check_with_context(
        config,
        store,
        user,
        rate_limiter,
        caps,
        PreCheckContext::enforced(),
    )
}

/// Run pre-flight checks that don't need the camera, honoring `ctx`'s gate
/// overrides. See [`PreCheckContext`].
pub fn pre_check_with_context(
    config: &Config,
    store: &FaceStore,
    user: &str,
    rate_limiter: &RateLimiter,
    caps: &CameraCaps,
    ctx: PreCheckContext,
) -> Option<AuthOutcome> {
    if config.security.disabled {
        warn!(user, "facelock is disabled");
        return Some(AuthOutcome::error(ErrorKind::Disabled));
    }

    if !ctx.skip_ssh_gate && config.security.abort_if_ssh && is_ssh_session() {
        info!(user, "SSH session detected, aborting");
        return Some(AuthOutcome::error(ErrorKind::SshSession));
    }

    if !ctx.skip_lid_gate && config.security.abort_if_lid_closed && is_lid_closed() {
        info!(user, "lid closed, aborting");
        return Some(AuthOutcome::error(ErrorKind::LidClosed));
    }

    let has_models = match store.has_models(user) {
        Ok(v) => v,
        Err(e) => {
            return Some(AuthOutcome::error_with(ErrorKind::Storage, e));
        }
    };
    if !has_models {
        if config.security.suppress_unknown {
            info!(
                user,
                "no enrolled models, suppressing (suppress_unknown=true)"
            );
            return Some(AuthOutcome::Suppressed);
        }
        return Some(AuthOutcome::AuthResult(MatchResult {
            matched: false,
            model_id: None,
            label: None,
            similarity: 0.0,
            // No camera was opened on this path, so no face can have been
            // seen — the gate rejected before any capture.
            face_detected: false,
            failure_reason: None,
        }));
    }

    match rate_limiter.check(store, user) {
        Ok(true) => {}
        Ok(false) => {
            warn!(user, "rate limited");
            return Some(AuthOutcome::error(ErrorKind::RateLimited));
        }
        Err(e) => {
            warn!(user, error = %e, "rate limit check failed");
            return Some(AuthOutcome::error_with(ErrorKind::RateLimitCheckFailed, e));
        }
    }

    if config.security.require_ir && !caps.is_ir {
        warn!(user, "IR camera required but device is not IR");
        return Some(AuthOutcome::error(ErrorKind::IrRequired));
    }

    None
}

/// Run [`pre_check`] and, when a gate short-circuits, write the audit entry
/// for the rejection. The daemon handler and the oneshot `facelock auth`
/// binary both go through this wrapper so rejection auditing cannot drift
/// between the two paths (#95: rate-limit rejections on the oneshot path
/// were never audited).
pub fn pre_check_audited(
    config: &Config,
    store: &FaceStore,
    user: &str,
    rate_limiter: &RateLimiter,
    caps: &CameraCaps,
    source: AuditSource,
) -> Option<AuthOutcome> {
    pre_check_audited_with_context(
        config,
        store,
        user,
        rate_limiter,
        caps,
        source,
        PreCheckContext::enforced(),
    )
}

/// [`pre_check_audited`], honoring `ctx`'s gate overrides. See [`PreCheckContext`].
pub fn pre_check_audited_with_context(
    config: &Config,
    store: &FaceStore,
    user: &str,
    rate_limiter: &RateLimiter,
    caps: &CameraCaps,
    source: AuditSource,
    ctx: PreCheckContext,
) -> Option<AuthOutcome> {
    let resp = pre_check_with_context(config, store, user, rate_limiter, caps, ctx)?;
    let (result, error) = match &resp {
        // The label comes from the rejection's class, never from its prose
        // (review C4): rewording a message must not relabel the audit trail.
        AuthOutcome::Error { kind, message } => {
            (kind.audit_result().to_string(), Some(message.clone()))
        }
        AuthOutcome::AuthResult(mr) if !mr.matched => ("failure".to_string(), None),
        AuthOutcome::Suppressed => ("suppressed".to_string(), None),
        _ => ("error".to_string(), None),
    };
    audit::write_audit_entry(
        &config.audit,
        &AuditEntry {
            timestamp: audit::now_iso8601(),
            user: user.to_string(),
            result,
            source: Some(source),
            similarity: None,
            frame_count: None,
            duration_ms: None,
            device: config.device.path.clone(),
            model_label: None,
            error,
        },
    );
    Some(resp)
}

/// Save a snapshot of the last captured frame to disk.
/// Failures are logged but never propagate — snapshots must not block auth.
fn save_snapshot(snapshot_config: &SnapshotConfig, user: &str, similarity: f32, frame: &Frame) {
    let dir = Path::new(&snapshot_config.dir);
    let ensure_snapshot_dir = if Uid::current().is_root() {
        ensure_private_dir(dir, 0o700)
    } else {
        ensure_dir(dir, 0o700)
    };
    if let Err(e) = ensure_snapshot_dir {
        warn!(dir = %dir.display(), error = %e, "failed to secure snapshot directory");
        return;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let filename = format!("{user}_{timestamp}_{similarity:.2}.jpg");
    let path = dir.join(&filename);

    let mut buf = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, 80);
    if let Err(e) = encoder.encode(
        &frame.rgb,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgb8,
    ) {
        warn!(path = %path.display(), error = %e, "failed to encode snapshot JPEG");
        return;
    }

    if let Err(e) = write_file(&path, &buf, 0o600) {
        warn!(path = %path.display(), error = %e, "failed to write snapshot");
        return;
    }

    debug!(path = %path.display(), "saved auth snapshot");
}

/// Plaintext embeddings wiped when the guard leaves scope — on every return
/// path *and* on an unwind, which the hand-written per-return-site wipes this
/// replaces could not cover (D11).
///
/// Generic over how the plaintext is held so the same guard covers both sets
/// this module handles: the caller's buffer (`&mut [_]`, borrowed) and the
/// device-filtered compare set (`Vec<_>`, owned). `zeroize`'s own `Zeroizing`
/// cannot: it needs `T: Zeroize`, which `(u32, FaceEmbedding)` is not.
struct Wiped<T>(T)
where
    T: AsRef<[(u32, FaceEmbedding)]> + AsMut<[(u32, FaceEmbedding)]>;

impl<T> std::ops::Deref for Wiped<T>
where
    T: AsRef<[(u32, FaceEmbedding)]> + AsMut<[(u32, FaceEmbedding)]>,
{
    type Target = [(u32, FaceEmbedding)];

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl<T> Drop for Wiped<T>
where
    T: AsRef<[(u32, FaceEmbedding)]> + AsMut<[(u32, FaceEmbedding)]>,
{
    fn drop(&mut self) {
        zeroize_stored_embeddings(self.0.as_mut());
    }
}

/// Run the camera-based authentication loop with pre-loaded (decrypted) embeddings.
///
/// This is the only entry point: callers MUST load embeddings through their
/// decryption-aware path (the daemon handler and the oneshot `facelock auth`
/// binary both do), because embeddings are encrypted at rest by default
/// (Plan 04). There is deliberately no store-reading variant here — reading
/// `get_user_embeddings` directly would treat an encrypted blob as a raw
/// embedding and fail.
///
/// **`stored` is consumed, not borrowed** (D11): this function wipes the
/// caller's plaintext buffer as soon as it has filtered its own compare set
/// out of it, so no caller has to remember to. That is why the parameter is
/// `&mut` — the rule used to be caller-side convention, implemented once in a
/// CLI-only wrapper and again inline in the daemon handler, which the daemon
/// could not share. Every embedding here is zeroized on every exit path,
/// including an unwind (see [`Wiped`]). Do not read `stored` after the call.
///
/// `source` records which code path ran the loop; it is stamped into every
/// audit entry written here. It marks the enforcement path, not caller intent:
/// only `Test` (direct-mode `facelock test`) skips `pre_check`, so a `success`
/// stamped `test` is a recognition result rather than an approved
/// authentication.
///
/// The device's IR-ness (gating the IR texture liveness check) and its
/// fingerprint (restricting the compare set to templates enrolled on this
/// camera) are asked of `camera.capabilities()` — the camera in use, not a
/// parameter a caller could get out of sync with it (gap D8).
#[allow(clippy::too_many_arguments)]
pub fn authenticate_with_embeddings<C: CameraSource, E: FaceProcessor>(
    camera: &mut C,
    engine: &mut E,
    stored: &mut [(u32, FaceEmbedding)],
    models: &[facelock_core::types::FaceModelInfo],
    config: &Config,
    user: &str,
    source: AuditSource,
    cancel: &CancelToken,
) -> AuthOutcome {
    let stored = Wiped(stored);
    let device_is_ir = camera.capabilities().is_ir;
    let live_fingerprint = camera.capabilities().fingerprint.clone();
    let start = Instant::now();
    let save_snapshots = config.snapshots.mode != facelock_core::config::SnapshotMode::Off;
    let label_for =
        |id: u32| -> Option<String> { models.iter().find(|m| m.id == id).map(|m| m.label.clone()) };

    // Device coupling (Plan 02): restrict the compare set to templates whose
    // enrolling camera matches the live camera at the configured granularity.
    // A mismatched template is dropped here so its embeddings are never compared
    // — the outcome degrades to "no match" → password, never a success and never
    // a lockout. Fail SOFT by construction.
    let policy = config.security.device_binding_policy();
    let allowed = device_allowed_model_ids(models, &live_fingerprint, &policy);
    let compare_set = Wiped(
        stored
            .iter()
            .filter(|(id, _)| allowed.contains(id))
            .cloned()
            .collect::<Vec<(u32, FaceEmbedding)>>(),
    );
    if policy.enabled && compare_set.len() != stored.len() {
        let skipped = stored.len() - compare_set.len();
        warn!(
            user,
            skipped,
            total = stored.len(),
            granularity = ?policy.granularity,
            "device coupling: skipped templates whose enrolling camera does not match the live camera (falling through to password if no allowed template matches)"
        );
    }
    if policy.enabled
        && models
            .iter()
            .any(|m| m.device_id.as_deref().unwrap_or("").is_empty())
    {
        info!(
            user,
            "device coupling: authenticating legacy template(s) with no device id (bind_legacy_templates); re-enroll to couple them to this camera"
        );
    }
    // `compare_set` holds independent copies of the allowed embeddings, so the
    // caller's full set is done here — dropping the guard wipes it (D11). This
    // is the contract stated on this function: the caller keeps no plaintext
    // after the call and has nothing to remember.
    drop(stored);

    let deadline =
        Instant::now() + std::time::Duration::from_secs(config.recognition.timeout_secs as u64);
    // The shorter deadline that applies only while the camera has seen nobody
    // at all (ADR 008 §4). `None` when the early exit is disabled. Clamped to
    // `timeout_secs` by the accessor, so it can never outlive `deadline`.
    let no_face_timeout = config.recognition.effective_no_face_timeout();
    let no_face_deadline = no_face_timeout.map(|d| start + d);
    let threshold = config.recognition.threshold;
    let mut best_similarity: f32 = 0.0;
    // Sliding window over the most recent matched-frame embeddings. The gate
    // evaluates only this window, so an early too-still moment is forgotten
    // once the user moves (a static input still never passes: every window
    // of a static sequence stays above the cutoff). Like `compare_set` and the
    // captured `Frame`s, it zeroizes itself on drop, so every exit path from
    // here on — including an unwind — wipes it.
    let mut variance_window = FrameVarianceWindow::new(config.security.min_auth_frames);
    let mut matched_frames_total: u32 = 0;
    let mut variance_ever_passed = false;
    let mut dark_count: u32 = 0;
    let mut frame_count: u32 = 0;
    // Whether the detector ever found a face during this attempt. Reported to
    // clients so "we looked and nobody was there" is distinguishable from "we
    // saw you and it wasn't a match" without reading the similarity score,
    // which is redacted to 0.0 for non-root callers (review C4 / #108's N12).
    let mut face_detected = false;
    let mut best_model_id: Option<u32> = None;
    let mut landmark_tracker = LandmarkTracker::new(
        10,
        config.security.landmark_displacement_px,
        config.security.landmark_min_moving as usize,
    );
    #[allow(unused_assignments)]
    let mut last_frame: Option<Frame> = None;

    while Instant::now() < deadline {
        // Checked before the blocking capture, so the attempt ends within one
        // frame of the token being set — not at `timeout_secs` (ADR 008 §5).
        if cancel.is_cancelled() {
            return cancelled(config, user, source, start, frame_count);
        }
        // Nobody is there. Also checked before the blocking capture, and only
        // while no face has been seen: once one has, this is the "seen you,
        // not matched yet" case that `timeout_secs` exists to bound, and the
        // user is worth waiting for. Scanning an empty chair for the full
        // timeout only keeps the IR emitter lit for nothing (ADR 008 §4).
        //
        // Ends the attempt exactly as the outer deadline does — `break`, not
        // a separate outcome: to every caller this is the same "no face was
        // detected" non-match, which the rate limiter then declines to charge.
        if !face_detected && no_face_deadline.is_some_and(|d| Instant::now() >= d) {
            debug!(
                user,
                frames = frame_count,
                no_face_timeout_secs = no_face_timeout.map_or(0, |d| d.as_secs()),
                "no face seen within the no-face timeout, ending the attempt early"
            );
            break;
        }
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
            debug!(frame = frame_count, "dark frame, skipping");
            continue;
        }

        if save_snapshots {
            last_frame = Some(frame.clone());
        }

        let faces = match engine.process(&frame) {
            Ok(f) => f,
            Err(e) => {
                debug!(frame = frame_count, "face engine error: {e}");
                continue;
            }
        };

        if faces.is_empty() {
            debug!(frame = frame_count, "no faces detected");
            continue;
        }
        // Detection, not recognition: a face rejected below by the IR texture
        // gate or by the compare set still means the camera saw somebody.
        face_detected = true;

        // Push landmarks from the first detected face for liveness tracking
        if let Some((det, _)) = faces.first() {
            landmark_tracker.push(det.landmarks);
        }

        // IR texture check: when using an IR camera, verify each detected face
        // has real skin texture (not a flat photo/screen replay attack).
        // Only applied to IR frames — RGB texture varies too much and would
        // cause false positives.
        //
        // Runs on the RAW grayscale frame. CLAHE is deliberately NOT applied here:
        // equalizing the frame inflates the std_dev of flat surfaces and masks the
        // very photo/screen replays this check exists to catch (H3).
        let ir_texture_min = config.security.ir_texture_min_stddev;
        if device_is_ir {
            let all_flat = faces.iter().all(|(det, _)| {
                !check_ir_texture(&frame.gray, &det.bbox, frame.width, ir_texture_min)
            });
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
            if device_is_ir
                && !check_ir_texture(&frame.gray, &det.bbox, frame.width, ir_texture_min)
            {
                debug!(
                    frame = frame_count,
                    "IR texture check failed for face, skipping"
                );
                continue;
            }
            let (frame_best_sim, frame_best_id) = best_match(embedding, &compare_set);

            if frame_best_sim > best_similarity {
                best_similarity = frame_best_sim;
                best_model_id = frame_best_id;
            }

            if frame_best_sim >= threshold && !frame_matched {
                variance_window.push(*embedding);
                matched_frames_total += 1;
                // Log drift values (never embeddings) so field tuning of
                // frame_variance_max_similarity has real data to work with.
                if let Some((min_sim, max_sim)) = variance_window.min_max_pair_similarity() {
                    debug!(
                        frame = frame_count,
                        min_pair_similarity = format!("{min_sim:.4}"),
                        max_pair_similarity = format!("{max_sim:.4}"),
                        window = variance_window.len(),
                        "frame variance window"
                    );
                }
                frame_matched = true;
            }

            debug!(
                frame = frame_count,
                similarity = format!("{frame_best_sim:.4}"),
                matched_frames = matched_frames_total,
                "face comparison"
            );
        }

        // Frame variance check + landmark liveness check
        if config.security.require_frame_variance {
            if variance_window.passes(config.security.frame_variance_max_similarity) {
                variance_ever_passed = true;
                // If landmark liveness is required, check it too
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
                    matched = matched_frames_total,
                    duration_ms = duration.as_millis() as u64,
                    "authentication succeeded"
                );
                audit::write_audit_entry(
                    &config.audit,
                    &AuditEntry {
                        timestamp: audit::now_iso8601(),
                        user: user.to_string(),
                        result: "success".into(),
                        source: Some(source),
                        similarity: Some(best_similarity),
                        frame_count: Some(frame_count),
                        duration_ms: Some(duration.as_millis() as u64),
                        device: config.device.path.clone(),
                        model_label: best_model_id.and_then(&label_for),
                        error: None,
                    },
                );
                if config.snapshots.should_save(true)
                    && let Some(ref snap_frame) = last_frame
                {
                    save_snapshot(&config.snapshots, user, best_similarity, snap_frame);
                }
                // Device-coupling invariant: the winning model must be one the
                // device policy permitted into `compare_set`. If this ever fires,
                // a mismatched template reached the success branch.
                debug_assert!(
                    best_model_id.is_none_or(|id| allowed.contains(&id)),
                    "device coupling invariant violated: skipped template reached success"
                );
                let response = AuthOutcome::AuthResult(MatchResult {
                    matched: true,
                    model_id: best_model_id,
                    label: best_model_id.and_then(&label_for),
                    similarity: best_similarity,
                    face_detected,
                    failure_reason: None,
                });
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
                "authentication succeeded (no variance check)"
            );
            audit::write_audit_entry(
                &config.audit,
                &AuditEntry {
                    timestamp: audit::now_iso8601(),
                    user: user.to_string(),
                    result: "success".into(),
                    source: Some(source),
                    similarity: Some(best_similarity),
                    frame_count: Some(frame_count),
                    duration_ms: Some(duration.as_millis() as u64),
                    device: config.device.path.clone(),
                    model_label: best_model_id.and_then(&label_for),
                    error: None,
                },
            );
            if config.snapshots.should_save(true)
                && let Some(ref snap_frame) = last_frame
            {
                save_snapshot(&config.snapshots, user, best_similarity, snap_frame);
            }
            let response = AuthOutcome::AuthResult(MatchResult {
                matched: true,
                model_id: best_model_id,
                label: best_model_id.and_then(&label_for),
                similarity: best_similarity,
                face_detected,
                failure_reason: None,
            });
            return response;
        }
    }

    let duration = start.elapsed();

    // Timeout expired. If frames DID match above the recognition threshold but
    // the variance gate never passed, say so — "no match" would be misleading.
    let failure_reason = if config.security.require_frame_variance
        && variance_window.is_full()
        && !variance_ever_passed
    {
        Some(AuthFailureReason::VarianceNotSatisfied)
    } else {
        None
    };

    if dark_count == frame_count && frame_count > 0 {
        warn!(
            user,
            frames = frame_count,
            duration_ms = duration.as_millis() as u64,
            "all frames were dark"
        );
        audit::write_audit_entry(
            &config.audit,
            &AuditEntry {
                timestamp: audit::now_iso8601(),
                user: user.to_string(),
                result: "error".into(),
                source: Some(source),
                similarity: None,
                frame_count: Some(frame_count),
                duration_ms: Some(duration.as_millis() as u64),
                device: config.device.path.clone(),
                model_label: None,
                error: Some(ErrorKind::AllFramesDark.render("")),
            },
        );
        // No snapshot for all-dark: last_frame is None since dark frames are skipped
        return AuthOutcome::error(ErrorKind::AllFramesDark);
    }

    info!(
        user,
        similarity = format!("{best_similarity:.4}"),
        frames = frame_count,
        matched = matched_frames_total,
        duration_ms = duration.as_millis() as u64,
        variance_blocked = failure_reason.is_some(),
        "authentication failed"
    );

    audit::write_audit_entry(
        &config.audit,
        &AuditEntry {
            timestamp: audit::now_iso8601(),
            user: user.to_string(),
            result: "failure".into(),
            source: Some(source),
            similarity: Some(best_similarity),
            frame_count: Some(frame_count),
            duration_ms: Some(duration.as_millis() as u64),
            device: config.device.path.clone(),
            model_label: None,
            error: None,
        },
    );

    if config.snapshots.should_save(false)
        && let Some(ref snap_frame) = last_frame
    {
        save_snapshot(&config.snapshots, user, best_similarity, snap_frame);
    }

    AuthOutcome::AuthResult(MatchResult {
        matched: false,
        model_id: None,
        label: None,
        similarity: best_similarity,
        face_detected,
        failure_reason,
    })
}

/// End an attempt that was abandoned rather than answered.
///
/// Audited (as `cancelled`, so the trail distinguishes "we stopped looking"
/// from "we looked and said no") and logged, but never a `MatchResult`: the
/// rate limiter charges failed *attempts*, and the user never got to make
/// one.
///
/// Crate-visible because a cancellation can also be noticed *before* the scan
/// loop is reached — the token can already be set when `CameraLease::acquire`
/// runs — and that ending must reach the same writer, or the audit trail
/// would depend on how early the caller went away (`Handler`, ADR 008 §5).
pub(crate) fn cancelled(
    config: &Config,
    user: &str,
    source: AuditSource,
    start: Instant,
    frame_count: u32,
) -> AuthOutcome {
    let duration = start.elapsed();
    info!(
        user,
        frames = frame_count,
        duration_ms = duration.as_millis() as u64,
        "authentication cancelled"
    );
    audit::write_audit_entry(
        &config.audit,
        &AuditEntry {
            timestamp: audit::now_iso8601(),
            user: user.to_string(),
            result: CANCELLED_MESSAGE.into(),
            source: Some(source),
            similarity: None,
            frame_count: Some(frame_count),
            duration_ms: Some(duration.as_millis() as u64),
            device: config.device.path.clone(),
            model_label: None,
            error: None,
        },
    );
    AuthOutcome::Cancelled
}

fn is_ssh_session() -> bool {
    std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok()
}

fn is_lid_closed() -> bool {
    std::fs::read_to_string("/proc/acpi/button/lid/LID0/state")
        .map(|s| s.contains("closed"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two strings PAM substring-matches to choose `PAM_AUTH_ERR` over
    /// `PAM_IGNORE` are frozen protocol (docs/contracts.md). They now come out
    /// of [`ErrorKind::render`]; this pins that they come out byte-identical.
    /// `tests/server_authz.rs` pins them again where they reach the wire.
    #[test]
    fn frozen_protocol_strings_render_byte_exactly() {
        assert_eq!(ErrorKind::RateLimited.render(""), "rate limited");
        assert_eq!(
            ErrorKind::IrRequired.render(""),
            "IR camera required for authentication. Set security.require_ir = false to override (NOT RECOMMENDED)."
        );
    }

    /// PAM's own predicates, replicated here because `pam-facelock` cannot
    /// depend on this crate (its dependency ceiling is libc/toml/serde/zbus),
    /// so nothing else can catch the two classifications drifting apart.
    ///
    /// The converse direction is the one that used to be invisible: if some
    /// *other* class started containing PAM's needles, it would silently
    /// inherit that class's PAM code.
    #[test]
    fn only_the_two_intended_classes_trip_pams_matchers() {
        // Verbatim from `pam_code_for_daemon_error`.
        let is_rate_limited = |m: &str| m.contains("rate_limit") || m.contains("rate limited");
        let is_ir_required =
            |m: &str| m.contains("IR camera required") || m.contains("ir_required");

        assert!(is_rate_limited(&ErrorKind::RateLimited.render("")));
        assert!(is_ir_required(&ErrorKind::IrRequired.render("")));

        for kind in ErrorKind::ALL {
            // `Internal` renders arbitrary text from elsewhere, so it cannot
            // be constrained — it is the class for messages this build does
            // not name.
            if *kind == ErrorKind::Internal {
                continue;
            }
            let message = kind.render("boom");
            if *kind != ErrorKind::RateLimited {
                assert!(
                    !is_rate_limited(&message),
                    "{kind:?} renders {message:?}, which PAM would read as a rate-limit lockout"
                );
            }
            if *kind != ErrorKind::IrRequired {
                assert!(
                    !is_ir_required(&message),
                    "{kind:?} renders {message:?}, which PAM would read as an IR rejection"
                );
            }
        }
    }

    /// The D-Bus wire has no field for the class, so the CLI's client rebuilds
    /// it from the rendered message. That reconstruction must be the exact
    /// inverse of the rendering, or the one remaining text matcher reintroduces
    /// the defect it replaced.
    #[test]
    fn classify_inverts_render() {
        for kind in ErrorKind::ALL {
            // `Internal` renders its detail verbatim and so has no
            // recognizable form; it is what unrecognized text lands on.
            if *kind == ErrorKind::Internal {
                continue;
            }
            let message = kind.render("boom");
            assert_eq!(
                ErrorKind::classify(&message),
                *kind,
                "{kind:?} rendered {message:?}, which classified as something else"
            );
        }
        assert_eq!(
            ErrorKind::classify("a message from a daemon of another version"),
            ErrorKind::Internal
        );
    }

    /// The third frozen string. PAM matches `cancelled` exactly to choose
    /// `PAM_IGNORE`, so no rejection class may render it — otherwise a
    /// genuine refusal would be reported as an abandoned attempt.
    #[test]
    fn no_rejection_class_renders_the_cancelled_string() {
        assert_eq!(CANCELLED_MESSAGE, "cancelled");
        for kind in ErrorKind::ALL {
            if *kind == ErrorKind::Internal {
                // Renders arbitrary text from elsewhere; unconstrainable.
                continue;
            }
            assert_ne!(
                kind.render("boom"),
                CANCELLED_MESSAGE,
                "{kind:?} renders the frozen cancellation string"
            );
        }
    }

    /// A camera whose k-th capture cancels the attempt, so the loop's
    /// response to the token can be observed exactly.
    struct CancellingCamera {
        captures: u32,
        cancel_at: u32,
        token: CancelToken,
        caps: CameraCaps,
    }

    impl CameraSource for CancellingCamera {
        fn capabilities(&self) -> &CameraCaps {
            &self.caps
        }

        fn capture(&mut self) -> facelock_core::error::Result<Frame> {
            self.captures += 1;
            if self.captures == self.cancel_at {
                self.token.cancel();
            }
            Ok(Frame {
                rgb: vec![200u8; 64 * 64 * 3],
                gray: vec![200u8; 64 * 64],
                width: 64,
                height: 64,
            })
        }

        fn capture_rgb_only(&mut self) -> facelock_core::error::Result<Frame> {
            self.capture()
        }
    }

    /// An engine that never finds a face, so nothing but the token can end
    /// the loop before its deadline.
    struct BlindEngine;

    impl FaceProcessor for BlindEngine {
        fn process(
            &mut self,
            _frame: &Frame,
        ) -> facelock_core::error::Result<Vec<(facelock_core::types::Detection, FaceEmbedding)>>
        {
            Ok(Vec::new())
        }
    }

    fn cancellable_config() -> Config {
        Config::parse(
            r#"
[recognition]
timeout_secs = 30

[security]
require_ir = false
require_frame_variance = false
require_landmark_liveness = false

[audit]
enabled = false
"#,
        )
        .unwrap_or_default()
    }

    /// The rule of ADR 008 §5: the attempt ends within *one* frame of the
    /// token being set — not at `timeout_secs`, which with a 30 s deadline
    /// is what this test would otherwise sit through.
    #[test]
    fn a_token_set_at_frame_k_ends_the_attempt_before_frame_k_plus_one() {
        let token = CancelToken::new();
        let mut camera = CancellingCamera {
            captures: 0,
            cancel_at: 3,
            token: token.clone(),
            caps: CameraCaps::default(),
        };
        let config = cancellable_config();
        let started = Instant::now();
        let outcome = authenticate_with_embeddings(
            &mut camera,
            &mut BlindEngine,
            &mut [],
            &[],
            &config,
            "alice",
            AuditSource::Daemon,
            &token,
        );

        assert!(
            matches!(outcome, AuthOutcome::Cancelled),
            "expected Cancelled, got {outcome:?}"
        );
        assert_eq!(
            camera.captures, 3,
            "the loop must stop before capturing frame k+1"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the attempt waited out its timeout instead of cancelling"
        );
    }

    /// A token already set when the loop starts costs zero frames.
    #[test]
    fn a_token_set_before_the_first_frame_captures_nothing() {
        let token = CancelToken::new();
        token.cancel();
        let mut camera = CancellingCamera {
            captures: 0,
            cancel_at: u32::MAX,
            token: token.clone(),
            caps: CameraCaps::default(),
        };
        let outcome = authenticate_with_embeddings(
            &mut camera,
            &mut BlindEngine,
            &mut [],
            &[],
            &cancellable_config(),
            "alice",
            AuditSource::Daemon,
            &token,
        );
        assert!(matches!(outcome, AuthOutcome::Cancelled));
        assert_eq!(camera.captures, 0);
    }

    /// An engine that finds a face on every frame and matches nothing (the
    /// compare set these tests pass is empty), i.e. the "we can see you, that
    /// is not you yet" case `timeout_secs` exists to bound.
    struct SeeingEngine;

    impl FaceProcessor for SeeingEngine {
        fn process(
            &mut self,
            _frame: &Frame,
        ) -> facelock_core::error::Result<Vec<(facelock_core::types::Detection, FaceEmbedding)>>
        {
            Ok(vec![(
                facelock_test_support::fixtures::center_detection(0.95),
                facelock_test_support::fixtures::known_embedding(0),
            )])
        }
    }

    fn no_face_config(no_face_timeout_secs: u32, timeout_secs: u32) -> Config {
        Config::parse(&format!(
            r#"
[recognition]
timeout_secs = {timeout_secs}
no_face_timeout_secs = {no_face_timeout_secs}

[security]
require_ir = false
require_frame_variance = false
require_landmark_liveness = false

[audit]
enabled = false
"#
        ))
        .expect("test config must parse")
    }

    /// Runs an attempt against a camera that never stops producing frames,
    /// returning the outcome and how long the loop actually ran.
    fn run_attempt<E: FaceProcessor>(
        engine: &mut E,
        config: &Config,
    ) -> (AuthOutcome, std::time::Duration) {
        let token = CancelToken::new();
        let mut camera = CancellingCamera {
            captures: 0,
            cancel_at: u32::MAX,
            token: token.clone(),
            caps: CameraCaps::default(),
        };
        let started = Instant::now();
        let outcome = authenticate_with_embeddings(
            &mut camera,
            engine,
            &mut [],
            &[],
            config,
            "alice",
            AuditSource::Daemon,
            &token,
        );
        (outcome, started.elapsed())
    }

    #[track_caller]
    fn assert_unmatched_without_a_face(outcome: &AuthOutcome) {
        match outcome {
            AuthOutcome::AuthResult(mr) => {
                assert!(!mr.matched, "must not authenticate: {mr:?}");
                assert!(
                    !mr.face_detected,
                    "nobody was there, so the reply must say so: {mr:?}"
                );
            }
            other => panic!("expected an ordinary non-match, got {other:?}"),
        }
    }

    /// ADR 008 §4: an empty chair ends the attempt at `no_face_timeout_secs`,
    /// not at `timeout_secs` — 1 s here rather than the 30 s the camera and
    /// its IR emitter would otherwise stay lit for.
    ///
    /// The ending is deliberately the *same* outcome the full timeout
    /// produces, so nothing downstream (PAM's sentinel, the audit trail)
    /// gains a case.
    #[test]
    fn no_face_ends_the_attempt_at_the_no_face_timeout() {
        let config = no_face_config(1, 30);
        let (outcome, elapsed) = run_attempt(&mut BlindEngine, &config);

        assert_unmatched_without_a_face(&outcome);
        assert!(
            elapsed >= std::time::Duration::from_secs(1),
            "ended before its own no-face deadline: {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "sat through timeout_secs instead of the no-face timeout: {elapsed:?}"
        );
    }

    /// The other half of the rule: the no-face deadline stops applying the
    /// moment a face is detected. Somebody who is present but not yet
    /// recognized — bad angle, glasses, a moment of stillness — gets the full
    /// `timeout_secs`, which is the whole reason the two are separate keys.
    #[test]
    fn a_face_seen_once_buys_the_full_timeout() {
        let config = no_face_config(1, 2);
        let (outcome, elapsed) = run_attempt(&mut SeeingEngine, &config);

        match outcome {
            AuthOutcome::AuthResult(mr) => {
                assert!(!mr.matched, "the compare set is empty: {mr:?}");
                assert!(mr.face_detected, "a face was detected: {mr:?}");
            }
            other => panic!("expected an ordinary non-match, got {other:?}"),
        }
        assert!(
            elapsed >= std::time::Duration::from_millis(1900),
            "the no-face deadline fired even though a face had been seen: {elapsed:?}"
        );
    }

    /// `0` disables the early exit: the attempt runs to `timeout_secs` even
    /// with nobody in front of the camera, which is what an operator who
    /// wants the old behavior back sets.
    #[test]
    fn a_zero_no_face_timeout_runs_to_the_full_timeout() {
        let config = no_face_config(0, 2);
        assert_eq!(
            config.recognition.effective_no_face_timeout(),
            None,
            "0 must switch the deadline off, not make it instant"
        );
        let (outcome, elapsed) = run_attempt(&mut BlindEngine, &config);

        assert_unmatched_without_a_face(&outcome);
        assert!(
            elapsed >= std::time::Duration::from_millis(1900),
            "the disabled no-face deadline still ended the attempt: {elapsed:?}"
        );
    }

    /// A rate-limit *check* that fails is a storage fault, not a lockout.
    /// PAM must not read it as a deliberate rejection, and the audit trail
    /// must not label it one — the substring matcher this replaced got both
    /// right only by accident of wording.
    #[test]
    fn a_failed_rate_limit_check_is_not_a_rate_limit_rejection() {
        let broken = ErrorKind::RateLimitCheckFailed;
        assert_ne!(broken.audit_result(), ErrorKind::RateLimited.audit_result());
        assert_eq!(broken.audit_result(), "error");
        assert!(!broken.render("disk gone").contains("rate limited"));
    }

    /// `SSH_CONNECTION` is process-global state; `cargo test` runs this
    /// file's tests on multiple threads, and four of them now mutate it
    /// (previously just one). Serialize access so they can't interleave.
    static SSH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_ssh_env() -> std::sync::MutexGuard<'static, ()> {
        SSH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn ssh_detection_with_env_vars() {
        let _guard = lock_ssh_env();
        let old_conn = std::env::var("SSH_CONNECTION").ok();
        let old_tty = std::env::var("SSH_TTY").ok();
        unsafe {
            std::env::remove_var("SSH_CONNECTION");
            std::env::remove_var("SSH_TTY");
        }

        assert!(!is_ssh_session());

        unsafe { std::env::set_var("SSH_CONNECTION", "1.2.3.4 5678 10.0.0.1 22") };
        assert!(is_ssh_session());
        unsafe { std::env::remove_var("SSH_CONNECTION") };

        unsafe { std::env::set_var("SSH_TTY", "/dev/pts/0") };
        assert!(is_ssh_session());
        unsafe { std::env::remove_var("SSH_TTY") };

        if let Some(v) = old_conn {
            unsafe { std::env::set_var("SSH_CONNECTION", v) };
        }
        if let Some(v) = old_tty {
            unsafe { std::env::set_var("SSH_TTY", v) };
        }
    }

    #[test]
    fn lid_closed_returns_false_on_missing_file() {
        let _result = is_lid_closed();
    }

    fn test_pre_check_config() -> Config {
        let toml =
            facelock_test_support::fixtures::test_config_toml("/tmp/facelock-precheck-test.db");
        let mut config = Config::parse(&toml).unwrap();
        config.security.abort_if_ssh = true;
        config
    }

    /// Caps of an IR-classified device (what these tests used to express as a
    /// bare `device_is_ir: true`).
    fn ir_caps() -> CameraCaps {
        CameraCaps {
            is_ir: true,
            ..Default::default()
        }
    }

    fn store_with_enrolled_user(user: &str) -> FaceStore {
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model(
                user,
                "front",
                &facelock_test_support::fixtures::known_embedding(0),
                "",
            )
            .unwrap();
        store
    }

    #[test]
    fn pre_check_context_test_skips_both_environment_gates() {
        let ctx = PreCheckContext::test();
        assert!(ctx.skip_ssh_gate);
        assert!(ctx.skip_lid_gate);
    }

    #[test]
    fn pre_check_context_enforced_skips_neither_gate() {
        let ctx = PreCheckContext::enforced();
        assert!(!ctx.skip_ssh_gate);
        assert!(!ctx.skip_lid_gate);
        // `Default` must agree with `enforced()` — `pre_check`/`pre_check_audited`
        // rely on this to stay fully enforced without threading a context.
        let default_ctx = PreCheckContext::default();
        assert!(!default_ctx.skip_ssh_gate);
        assert!(!default_ctx.skip_lid_gate);
    }

    #[test]
    fn pre_check_test_context_skips_ssh_gate() {
        let _guard = lock_ssh_env();
        let config = test_pre_check_config();
        let store = store_with_enrolled_user("alice");
        let rate_limiter = RateLimiter::new(
            config.security.rate_limit.max_attempts,
            config.security.rate_limit.window_secs,
        );

        let old_conn = std::env::var("SSH_CONNECTION").ok();
        unsafe { std::env::set_var("SSH_CONNECTION", "1.2.3.4 1 5.6.7.8 22") };
        let resp = pre_check_with_context(
            &config,
            &store,
            "alice",
            &rate_limiter,
            &ir_caps(),
            PreCheckContext::test(),
        );
        unsafe {
            match old_conn {
                Some(v) => std::env::set_var("SSH_CONNECTION", v),
                None => std::env::remove_var("SSH_CONNECTION"),
            }
        }

        assert!(
            resp.is_none(),
            "PreCheckContext::test() must proceed past the SSH gate, got: {resp:?}"
        );
    }

    #[test]
    fn pre_check_enforced_context_still_blocks_ssh() {
        let _guard = lock_ssh_env();
        let config = test_pre_check_config();
        let store = store_with_enrolled_user("alice");
        let rate_limiter = RateLimiter::new(
            config.security.rate_limit.max_attempts,
            config.security.rate_limit.window_secs,
        );

        let old_conn = std::env::var("SSH_CONNECTION").ok();
        unsafe { std::env::set_var("SSH_CONNECTION", "1.2.3.4 1 5.6.7.8 22") };
        let resp = pre_check_with_context(
            &config,
            &store,
            "alice",
            &rate_limiter,
            &ir_caps(),
            PreCheckContext::enforced(),
        );
        unsafe {
            match old_conn {
                Some(v) => std::env::set_var("SSH_CONNECTION", v),
                None => std::env::remove_var("SSH_CONNECTION"),
            }
        }

        assert!(
            matches!(resp, Some(AuthOutcome::Error { kind, .. }) if kind == ErrorKind::SshSession),
            "the default/enforced context must still reject an SSH session, got: {resp:?}"
        );
    }

    #[test]
    fn pre_check_without_context_matches_enforced() {
        // `pre_check` (no context param) is a thin wrapper — pin that it stays
        // equivalent to `pre_check_with_context(.., PreCheckContext::enforced())`
        // so real auth paths (daemon `Authenticate`, oneshot `facelock auth`)
        // never silently pick up a relaxed gate.
        let _guard = lock_ssh_env();
        let config = test_pre_check_config();
        let store = store_with_enrolled_user("alice");
        let rate_limiter = RateLimiter::new(
            config.security.rate_limit.max_attempts,
            config.security.rate_limit.window_secs,
        );

        let old_conn = std::env::var("SSH_CONNECTION").ok();
        unsafe { std::env::set_var("SSH_CONNECTION", "1.2.3.4 1 5.6.7.8 22") };
        let resp = pre_check(&config, &store, "alice", &rate_limiter, &ir_caps());
        unsafe {
            match old_conn {
                Some(v) => std::env::set_var("SSH_CONNECTION", v),
                None => std::env::remove_var("SSH_CONNECTION"),
            }
        }

        assert!(
            matches!(resp, Some(AuthOutcome::Error { kind, .. }) if kind == ErrorKind::SshSession),
            "pre_check() must stay fully enforced, got: {resp:?}"
        );
    }
}
