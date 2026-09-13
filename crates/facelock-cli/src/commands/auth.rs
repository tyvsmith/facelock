//! One-shot authentication subcommand.
//!
//! Exit codes: 0 = matched, 1 = scanned and not matched, 2 = error / no
//! opinion, 3 = rate limited, 4 = suppressed, 5 = all frames dark. The full
//! table and its compatibility invariants are frozen in docs/contracts.md
//! ("facelock auth Exit Codes"); [`oneshot_exit_code`] and
//! [`EXIT_SUPPRESSED`] are the implementation.

use std::path::Path;

use facelock_camera::quirks::QuirksDb;
use facelock_camera::{Camera, ResolvedCamera, auto_detect_device_with, validate_device};
use facelock_core::config::Config;
use facelock_core::types::CameraCaps;
use facelock_core::types::MatchResult;
use facelock_daemon::audit::{self, AuditEntry, AuditSource};
use facelock_daemon::auth;
use facelock_daemon::auth::{AuthOutcome, ErrorKind};
use facelock_daemon::cancel::CancelToken;
use facelock_daemon::rate_limit::RateLimiter;
use facelock_face::FaceEngine;
use facelock_store::FaceStore;
use tracing::{debug, error, info};

pub fn run(user: String, config_path: Option<String>, verbose: u8) -> i32 {
    // Deliberate parse (D7): `facelock auth` is its own one-shot process
    // spawned by PAM, so this is its top-of-process read — not a re-read.
    let config = match config_path {
        Some(ref p) => Config::load_from(Path::new(p)),
        None => Config::load(),
    };
    let mut config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("facelock auth: config error: {e}");
            return 2;
        }
    };

    // See crate::logging's module doc for why this must not build its own
    // subscriber by hand: it owns both the filter and the stderr writer.
    //
    // `Program::Cli`, not `Daemon`: this process is spawned by
    // `pam_facelock.so` with its stderr on a pipe nothing ever drains, and is
    // otherwise something a person ran in a terminal. Neither reader wants
    // INFO by default, and `facelock -v auth --user X` is how a run being
    // debugged asks for it.
    crate::logging::init_stderr(crate::logging::Program::Cli, verbose);

    // Loaded once, up front: auto-detection and the interrogation below must
    // be decided by the SAME quirk set, or the device picked and the device
    // classified could disagree.
    let quirks = QuirksDb::load();

    if config.device.path.is_none() {
        match auto_detect_device_with(&quirks) {
            Ok(dev) => {
                info!(device = %dev.path, name = %dev.name, "auto-detected camera");
                config.device.path = Some(dev.path);
            }
            Err(e) => {
                error!("no camera: {e}");
                return 2;
            }
        }
    }

    // Resolve and interrogate the live device before the pre-flight gates so
    // `pre_check` sees the real caps (IR classification, fingerprint),
    // exactly like the daemon does at startup. Tolerant of a device that
    // cannot be queried: non-IR caps with whatever identity sysfs still
    // offers, so `require_ir` rejects rather than a hard error here.
    let device_path = config.device.path.clone().unwrap();
    let resolved = match validate_device(&device_path) {
        Ok(info) => Some(ResolvedCamera::interrogate(info, &quirks)),
        Err(_) => None,
    };
    let caps = resolved
        .as_ref()
        .map(|r| r.caps.clone())
        .unwrap_or_else(|| {
            CameraCaps::unqueryable(facelock_camera::device_fingerprint(&device_path))
        });

    // Best-effort mode fixing before the store is opened: this is the PAM
    // path, the one entry point guaranteed to run on an oneshot-mode install
    // that never starts the daemon and never re-runs setup. A failure only
    // means modes could not be set and must never block an authentication.
    crate::state_layout::ensure_state_layout_best_effort(&config);

    // Open a writable store (the oneshot path runs as root). The rate limiter
    // is SQLite-backed through this database, so its window is shared with the
    // daemon and survives across process invocations.
    // `create`: on a genuinely fresh install this is what brings the
    // rate-limiter's storage into being, so switching to `open_existing` would
    // change auth-path behaviour, not harden it.
    let store = match FaceStore::create(Path::new(&config.storage.db_path)) {
        Ok(s) => s,
        Err(e) => {
            error!("database: {e}");
            return 2;
        }
    };

    let rl = &config.security.rate_limit;
    let rate_limiter = RateLimiter::new(rl.max_attempts, rl.window_secs);

    // The daemon's pre-flight gates (disabled/SSH/lid, enrollment +
    // suppress_unknown, rate limit, require_ir), with the daemon's rejection
    // auditing. This used to be an inline mirror that drifted (#95): rate-limit
    // rejections were never audited and suppress_unknown was ignored.
    if let Some(resp) = auth::pre_check_audited(
        &config,
        &store,
        &user,
        &rate_limiter,
        &caps,
        AuditSource::Oneshot,
    ) {
        // Each pre-flight rejection exits under its class's own code
        // (#141), so the PAM module can give it the same consequence the
        // daemon transport does instead of reading every rejection as "no
        // opinion" (exit 2) — which is what daemon unavailability used to
        // buy: a rate-limited user's PAM_AUTH_ERR softened to PAM_IGNORE
        // whenever PAM fell back to this path. A module older than the split
        // maps the new codes back to PAM_IGNORE, exactly the collapsed
        // behavior it always had. The table and its invariants are frozen in
        // docs/contracts.md ("facelock auth Exit Codes").
        debug!(?resp, "pre-check short-circuit");

        // The only marker work a *rejected* attempt performs, and the reason
        // the daemonless install can converge downward at all (#137 review).
        // See the helper for why this specific operation is admissible on this
        // side of the rate limiter when a marker *write* is not.
        clear_marker_the_store_contradicts(&config, &store, &user);

        return preflight_exit_code(&resp);
    }

    // The user's model list, read before anything opens the camera. The
    // attempt needs it (labels, and the device-coupling allow-set), the
    // convergence below is derived from its length, and reading it here rather
    // than after the camera opens is the ordering the daemon handler already
    // uses for ADR 008 §1 — every millisecond between the camera open and the
    // first analyzed frame is LED-on time the user reads as a strobe.
    //
    // Metadata only (ids, labels, device ids): no embeddings cross the camera
    // bring-up because of this line. `load_user_embeddings`, which does carry
    // plaintext biometric material, deliberately stays below.
    let listed = store.list_models(&user);

    // Converge this user's enrollment marker from the list we just read (#137).
    // Free: the count is already in hand. This is the daemonless install's only
    // convergence point — the daemon's startup reconcile never runs there.
    //
    // Below `pre_check_audited` and not above it: hoisting it would put a
    // marker rewrite (temp file, chown, rename) behind every rate-limited
    // attempt — filesystem work an attacker can drive from the wrong side of
    // the rate limiter. The cost is that a pre-flight rejection does not
    // converge; the next attempt that passes the gates does.
    //
    // Above the camera bring-up and not below it. Everything past this point
    // can end the attempt early for reasons that say nothing about whether the
    // user is enrolled: a cancel token set by SIGTERM/SIGINT/SIGHUP, a failed
    // model load, a camera another process is holding, an embedding that will
    // not decrypt, `recognition.no_face_timeout_secs` on an empty chair. None
    // of those are pre-flight rejections and none of them are evidence about
    // enrollment, so none of them may decide whether `is-enrolled` tells the
    // truth. Placing the call at the `list_models` line — where it sat before
    // the camera-lifecycle work moved that line below the camera open — would
    // have handed every one of them a veto.
    converge_enrollment_marker(&config, &user, listed.as_ref().ok().map(|m| m.len() as u32));

    // A storage failure here must surface as an error, never fold into an
    // empty model list (C3, issue #105) — the same refusal the daemon
    // handler makes, in the same words: empty `models` means an empty
    // device-allowed set, a guaranteed "no match" (exit 1, PAM_AUTH_ERR),
    // and a rate-limit charge for an attempt the user never got to make —
    // retries then walk straight into a lockout. Exit under the storage
    // class instead (2, PAM_IGNORE): the daemon maps the identical failure
    // to its `-2` error reply, which PAM reads the same way.
    let models = match listed {
        Ok(m) => m,
        Err(e) => {
            error!(user = %user, "failed to list models: {e}");
            return oneshot_exit_code(ErrorKind::Storage);
        }
    };

    // Signals, before anything that turns the camera on. `facelock auth` is a
    // one-shot: exit *is* the release, so the job of a signal is to end the
    // scan loop and let `Camera::drop` run (STREAMOFF, IR emitter off).
    // Killing the process outright would skip that and, on hardware with an
    // XU-controlled emitter, leave the LED lit (ADR 008 §7).
    let cancel = CancelToken::new();
    register_cancel_signals(&cancel);

    // Quirk override takes precedence over the config's warmup value.
    let warmup = resolved
        .as_ref()
        .and_then(|r| r.quirk.as_ref())
        .and_then(|q| q.warmup_frames)
        .unwrap_or(config.device.warmup_frames);

    let (mut engine, mut camera) = match start_engine_then_camera(
        &cancel,
        || {
            FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir))
                .map_err(|e| e.to_string())
        },
        || {
            match resolved {
                // The camera carries the caps the pre-flight gates just checked.
                Some(resolved) => resolved.open(&config.device),
                // Unqueryable at resolution time: attempt a fresh
                // resolve-and-open so the failure surfaces as the camera
                // error it is.
                None => Camera::open(&config.device, &quirks),
            }
            .map_err(|e| e.to_string())
        },
    ) {
        Ok(pair) => pair,
        Err(StartupAbort::Cancelled) => {
            info!(user = %user, "cancelled");
            return 2;
        }
        Err(StartupAbort::Engine(e)) => {
            error!("models: {e}");
            return 2;
        }
        Err(StartupAbort::Camera(e)) => {
            error!("camera: {e}");
            return 2;
        }
    };

    // Negotiation may drift from the preflight FourCC. Do not let warmup
    // capture precede the auth loop's stable unverified-Y16 rejection.
    if camera.capabilities().ir_texture_scale != facelock_core::types::IrTextureScale::UnverifiedY16
    {
        for _ in 0..warmup {
            if cancel.is_cancelled() {
                info!(user = %user, "cancelled");
                return 2;
            }
            let _ = camera.capture();
        }
    }

    // Load embeddings through the decryption-aware path so the oneshot binary
    // handles encrypted templates (encrypt-by-default, Plan 04) — the bare
    // `auth::authenticate` helper reads plaintext only.
    //
    // A failed load is a storage fault, not evidence about the face: the
    // first failing row fails the whole load (see facelock-daemon's
    // embeddings module), so a TPM unseal broken by rotated PCRs lands here
    // on every attempt. Exit 1 is frozen as "scanned and not matched" and
    // would fail a `required` stack against the correct password — a
    // lockout, not a fallback. Exit under the storage class (2, PAM_IGNORE),
    // matching the daemon handler's `-2` storage reply for the same failure.
    let mut stored = match crate::direct::load_user_embeddings(&store, &config, &user) {
        Ok(v) => v,
        Err(e) => {
            error!(user = %user, "failed to load embeddings: {e}");
            return oneshot_exit_code(ErrorKind::Storage);
        }
    };

    let start = std::time::Instant::now();
    // Wipes `stored` (D11): nothing below may read the plaintext set again.
    let response = facelock_daemon::auth::authenticate_with_embeddings(
        &mut camera,
        &mut engine,
        &mut stored,
        &models,
        &config,
        &user,
        AuditSource::Oneshot,
        &cancel,
    );
    let duration_ms = start.elapsed().as_millis() as u64;

    // Note: authenticate_with_embeddings already writes audit entries for the
    // camera-based auth loop. The oneshot path relies on those entries, so no
    // additional audit logging is needed here for the auth result itself.

    // The same rule the daemon handler applies: a failed attempt charges the
    // shared budget, but only if a face was actually seen. An attempt at an
    // empty chair is not a guess (ADR 008 §4), and both transports write to
    // the same `rate_limit` table, so they must agree on what counts.
    if matches!(
        response,
        AuthOutcome::AuthResult(MatchResult {
            matched: false,
            face_detected: true,
            ..
        })
    ) && let Err(e) = rate_limiter.record_failure(&store, &user)
    {
        error!("rate limit record: {e}");
    }

    match response {
        AuthOutcome::AuthResult(MatchResult {
            matched: true,
            similarity,
            ..
        }) => {
            info!(user = %user, similarity = format!("{similarity:.4}"), "authenticated");
            0
        }
        AuthOutcome::AuthResult(MatchResult {
            matched: false,
            similarity,
            failure_reason,
            ..
        }) => {
            // Exit 1 means exactly this arm: the scan ran and did not match.
            // The camera-failure class that used to share it (all frames
            // dark) now exits under its own code, so this is the only place
            // the binary can say "not you". The reason is diagnostic only.
            info!(
                user = %user,
                similarity = format!("{similarity:.4}"),
                variance_blocked = failure_reason.is_some(),
                "no match"
            );
            1
        }
        AuthOutcome::Cancelled => {
            // Already audited as `cancelled` by the auth loop. Exit 2 is the
            // "no opinion" code, which every module generation maps to
            // PAM_IGNORE — the same abstention the daemon transport's frozen
            // "cancelled" message produces. An abandoned attempt gets no code
            // of its own: it is the absence of an answer, not a class of one.
            info!(user = %user, duration_ms, "cancelled");
            2
        }
        AuthOutcome::Error { kind, .. }
            if matches!(
                kind,
                ErrorKind::AllFramesDark | ErrorKind::Y16BitDepthRequired
            ) =>
        {
            // `authenticate_with_embeddings` already audited both classes, so
            // unlike the arm below there is nothing to write here.
            info!(user = %user, error_kind = ?kind, "camera authentication rejected");
            oneshot_exit_code(kind)
        }
        AuthOutcome::Error { kind, message } => {
            // Errors not audited by the camera auth loop are written here,
            // under the class's own label.
            audit::write_audit_entry(
                &config.audit,
                &AuditEntry {
                    timestamp: audit::now_iso8601(),
                    user: user.clone(),
                    result: kind.audit_result().into(),
                    source: Some(AuditSource::Oneshot),
                    similarity: None,
                    frame_count: None,
                    duration_ms: Some(duration_ms),
                    device: config.device.path.clone(),
                    model_label: None,
                    error: Some(message.clone()),
                },
            );
            error!(user = %user, "auth error: {message}");
            oneshot_exit_code(kind)
        }
        _ => {
            error!("unexpected response");
            2
        }
    }
}

/// Write `user`'s enrollment marker from a model count just read from the
/// authoritative store (#137).
///
/// `models` is `None` when the store could not be listed — leave the existing
/// marker alone rather than guessing it away, the same rule
/// [`crate::commands::enrollment_marker::refresh`] follows.
///
/// Called after the pre-flight gates pass, before the camera is opened and
/// before the authentication attempt, so nothing the attempt goes on to do or
/// fail to do can decide whether it runs: an upgraded user whose face is not
/// recognised today — or whose camera is busy today — still needs
/// `is-enrolled` to tell the truth. Best-effort throughout — `set` logs its own
/// failures and never propagates, so nothing here can fail an authentication.
///
/// **This call converges upward only.** Reaching it means the enrollment gate
/// passed, i.e. `has_models` was true, so the count is ≥ 1 barring a
/// concurrent `remove` between the two reads. Clearing a marker the database
/// contradicts is [`clear_marker_the_store_contradicts`]'s job, on the
/// rejection path where that evidence actually arrives. `set` is still used
/// (rather than `write_marker_in`) so the racing-zero case does the right
/// thing instead of writing `{"models":0}`.
fn converge_enrollment_marker(config: &Config, user: &str, models: Option<u32>) {
    match models {
        Some(models) => super::enrollment_marker::set(config, user, models),
        None => debug!(user = %user, "model list unavailable; enrollment marker left unchanged"),
    }
}

/// Delete `user`'s enrollment marker when the authoritative store says they
/// have no models — the downward half of #137's convergence, and the only
/// marker work a pre-flight **rejection** is allowed to do.
///
/// # Why the one-shot path needs this at all
///
/// [`auth::pre_check_with_context`] short-circuits when `has_models` is false,
/// which is *above* [`converge_enrollment_marker`] in `run`. So on a daemonless
/// install — no daemon start, therefore no `reconcile_all`/`prune_markers_in`
/// — a marker that outlives its database rows can never be cleared: every
/// subsequent attempt hits the not-enrolled gate and returns before
/// convergence, and `facelock is-enrolled` reports enrolled forever. The
/// database being restored from a backup that predates the enrollment, or a row
/// removed out of band, is enough to get there. That is exactly #137's drift,
/// running in the opposite direction.
///
/// # Why this is admissible here when a marker *write* is not
///
/// The placement of [`converge_enrollment_marker`] below `pre_check_audited` is
/// load-bearing and is not disturbed: it keeps a marker **write** — temp file,
/// `chown`, `rename`, and a `mkdir` of the marker directory — off the path an
/// attempt rejected at a pre-flight gate can reach, because that is
/// attacker-drivable filesystem work from the wrong side of the rate limiter.
///
/// What happens here is a different operation with different properties:
///
/// - **It only ever removes.** One `unlink(2)` on a path that is a single,
///   validated component under the marker directory ([`marker_path`] rejects
///   `..`, `/` and empty names). Nothing is created, nothing is `chown`ed, no
///   directory is materialized — `remove_marker_in` does not call
///   `ensure_private_dir`.
/// - **It is bounded.** The first rejection removes the marker; every
///   subsequent one finds `ENOENT` and does nothing. Repetition buys an
///   attacker one failed `unlink` per attempt — nothing accumulates, on disk or
///   anywhere else. Said plainly, because the ordering matters: the enrollment
///   gate fires *before* the rate-limit check, so on that path this really does
///   run unmetered. What makes that acceptable is the bound above and the
///   property below, not the rate limiter.
/// - **It cannot be driven to a wrong answer.** It fires only when the
///   database — the authority — reports zero models for the user, in which case
///   any marker claiming otherwise is already false. There is no state an
///   attacker can steer it into: it can delete a stale marker, never a correct
///   one, so it is not a denial-of-face-unlock primitive either.
///
/// The `has_models` read here repeats the one the enrollment gate just did.
/// That is deliberate: asking the store directly keeps this independent of
/// which [`AuthOutcome`] variant a gate happens to return, so a future
/// rejection class cannot silently start or stop clearing markers. The cost is
/// one indexed `COUNT` on the rejection path only.
///
/// Best-effort like every other marker write: `forget` logs its own failures
/// and never propagates, so nothing here can fail an authentication.
///
/// [`marker_path`]: crate::commands::enrollment_marker::marker_path
fn clear_marker_the_store_contradicts(config: &Config, store: &FaceStore, user: &str) {
    match store.has_models(user) {
        // Authoritative "no": the marker, if any, is stale.
        Ok(false) => super::enrollment_marker::forget(config, user),
        Ok(true) => {}
        // An unreadable store is not evidence of anything — the same rule
        // `converge_enrollment_marker` applies to a `None` count. Guessing
        // "zero" from a transient database error would delete a correct marker.
        Err(e) => {
            debug!(user = %user, error = %e, "enrollment state unreadable; enrollment marker left unchanged")
        }
    }
}

/// Why the one-shot never got both of its resources up.
#[derive(Debug, PartialEq, Eq)]
enum StartupAbort {
    /// A signal arrived first. The camera was never opened.
    Cancelled,
    /// The ONNX models could not be loaded.
    Engine(String),
    /// The camera could not be opened.
    Camera(String),
}

/// Bring up the one-shot's two expensive resources **in this order**: models
/// first, camera second, with a cancellation check in between.
///
/// The order is the point. Loading ONNX takes a large fraction of a
/// one-shot's wall time; opening the camera first meant the IR emitter was
/// lit for all of it, which the user reads as a strobe that has nothing to do
/// with being looked at. Opening it last makes LED-on time equal to scan time
/// (ADR 008 §7). The check between them is what makes a SIGTERM during the
/// model load exit without ever touching the camera (§8).
///
/// Generic over the two resources purely so the ordering — the part that is
/// policy — is testable without ONNX models or a V4L2 device.
fn start_engine_then_camera<E, C>(
    cancel: &CancelToken,
    load_engine: impl FnOnce() -> Result<E, String>,
    open_camera: impl FnOnce() -> Result<C, String>,
) -> Result<(E, C), StartupAbort> {
    if cancel.is_cancelled() {
        return Err(StartupAbort::Cancelled);
    }
    let engine = load_engine().map_err(StartupAbort::Engine)?;
    if cancel.is_cancelled() {
        return Err(StartupAbort::Cancelled);
    }
    let camera = open_camera().map_err(StartupAbort::Camera)?;
    Ok((engine, camera))
}

/// Wire SIGTERM, SIGINT and SIGHUP to `cancel`.
///
/// `signal_hook::flag::register` writes into an `Arc<AtomicBool>` from the
/// handler — which is why [`CancelToken`] is one. The scan loop reads it once
/// per frame, unwinds normally, and `Camera::drop` runs STREAMOFF and turns
/// the IR emitter off. That is the whole point: the default disposition for
/// all three signals is to die immediately, skipping `Drop` and leaving an
/// XU-controlled emitter lit.
///
/// The three signals are the three ways a PAM host lets go: SIGTERM from the
/// module's own timeout (and from `PR_SET_PDEATHSIG` when the host is killed),
/// SIGINT from Ctrl-C at a `sudo` prompt, SIGHUP when the terminal goes away.
///
/// Best-effort: a registration failure is logged and the process keeps its
/// default disposition for that signal, which is exactly today's behavior.
fn register_cancel_signals(cancel: &CancelToken) {
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        if let Err(e) = signal_hook::flag::register(signal, cancel.flag()) {
            // Not fatal: this only costs the clean shutdown, never the auth.
            debug!(signal, "could not register cancel signal handler: {e}");
        }
    }
}

/// Exit code for [`AuthOutcome::Suppressed`] (#141). Not an [`ErrorKind`],
/// so it cannot have a row in [`oneshot_exit_code`]'s exhaustive match; this
/// constant is its entry in the same frozen table (docs/contracts.md,
/// "facelock auth Exit Codes"). A module that knows the code maps it to
/// PAM_AUTHINFO_UNAVAIL — the consequence the daemon transport's `-3`
/// sentinel already had — and an older module maps it to PAM_IGNORE, the
/// collapsed pre-#141 behavior.
const EXIT_SUPPRESSED: i32 = 4;

/// The process exit code for a pre-flight short-circuit outcome.
///
/// Split from its one call site so the non-`Error` arms are pinned by test
/// the same way [`oneshot_exit_code`]'s table is.
fn preflight_exit_code(resp: &AuthOutcome) -> i32 {
    match resp {
        AuthOutcome::Error { kind, .. } => oneshot_exit_code(*kind),
        AuthOutcome::Suppressed => EXIT_SUPPRESSED,
        // Not-enrolled without `suppress_unknown` (the daemon transport's
        // plain `-1` reply) and anything unforeseen: "no opinion". The camera
        // never opened on this path, so exit 1's "scanned and not matched" is
        // not available to it by construction.
        _ => 2,
    }
}

/// The process exit code for a rejection of class `kind`.
///
/// The oneshot binary is spawned by the PAM module, which turns this number
/// into a PAM code, so this table is auth policy — it used to be
/// `message.contains("all frames dark")`, one reword away from changing it.
///
/// Exhaustive on purpose: a new class must be assigned a code here.
///
/// The codes are a frozen contract (docs/contracts.md, "facelock auth Exit
/// Codes") with three invariants: exit 0, 1 and 2 keep their historical
/// meanings permanently; every newer code is allocated from the space an
/// older module already maps to PAM_IGNORE (so old module + new binary
/// degrades to the pre-split collapse and regresses nothing); and the
/// module's arm for unknown codes stays PAM_IGNORE (so new module + old
/// binary is unchanged too). Within that frame, each class whose
/// daemon-transport consequence is not PAM_IGNORE carries its own code, so
/// daemon unavailability no longer changes what PAM concludes (#141).
/// [`EXIT_SUPPRESSED`] is the companion code for the suppressed outcome,
/// which is not an `ErrorKind`.
fn oneshot_exit_code(kind: ErrorKind) -> i32 {
    match kind {
        // The one pre-flight class whose daemon consequence is a deliberate
        // failure (PAM_AUTH_ERR): an exhausted face-auth budget must not
        // soften to fall-through just because the daemon was unreachable.
        ErrorKind::RateLimited => 3,
        // The camera produced no usable image: not a non-match, and under
        // the daemon transport not a failure either — its rendered message
        // maps to PAM_IGNORE. Moved off exit 1 (which is frozen as "scanned
        // and not matched") to a code both module generations read as the
        // same abstention the daemon produces.
        ErrorKind::AllFramesDark => 5,
        // Every remaining class means "face auth cannot answer here": both
        // transports abstain (PAM_IGNORE), so they share the historical
        // catch-all code.
        ErrorKind::Disabled
        | ErrorKind::SshSession
        | ErrorKind::LidClosed
        | ErrorKind::LidUnavailable
        | ErrorKind::Storage
        | ErrorKind::RateLimitCheckFailed
        | ErrorKind::IrRequired
        | ErrorKind::Y16BitDepthRequired
        | ErrorKind::Internal => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EXIT_SUPPRESSED, StartupAbort, oneshot_exit_code, preflight_exit_code,
        start_engine_then_camera,
    };
    use facelock_core::config::Config;
    use facelock_core::types::MatchResult;
    use facelock_daemon::audit::AuditSource;
    use facelock_daemon::auth::pre_check_audited;
    use facelock_daemon::cancel::CancelToken;

    /// The ordering of ADR 008 §7, observed rather than asserted from the
    /// source: models load before the camera opens, so the IR emitter is lit
    /// for the scan and not for the model load.
    #[test]
    fn the_engine_loads_before_the_camera_opens() {
        let order = std::cell::RefCell::new(Vec::new());
        let started = start_engine_then_camera(
            &CancelToken::new(),
            || {
                order.borrow_mut().push("engine");
                Ok::<_, String>(())
            },
            || {
                order.borrow_mut().push("camera");
                Ok::<_, String>(())
            },
        );
        assert!(started.is_ok());
        assert_eq!(order.into_inner(), vec!["engine", "camera"]);
    }

    /// A signal that arrives before start-up costs nothing: neither resource
    /// is touched, and the camera in particular is never opened, so there is
    /// no LED to turn back off.
    #[test]
    fn a_token_set_before_startup_opens_nothing() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let order = std::cell::RefCell::new(Vec::new());
        let started = start_engine_then_camera(
            &cancel,
            || {
                order.borrow_mut().push("engine");
                Ok::<_, String>(())
            },
            || {
                order.borrow_mut().push("camera");
                Ok::<_, String>(())
            },
        );
        assert_eq!(started.unwrap_err(), StartupAbort::Cancelled);
        assert!(
            order.into_inner().is_empty(),
            "a cancelled start-up must not load models or open the camera"
        );
    }

    /// The check between the two steps: a signal during the model load
    /// (which is the long one) stops the start-up before the camera factory
    /// is ever called — ADR 008 §8's "SIGTERM during model load".
    #[test]
    fn a_signal_during_the_model_load_never_opens_the_camera() {
        let cancel = CancelToken::new();
        let opened = std::cell::Cell::new(false);
        let started = start_engine_then_camera(
            &cancel,
            || {
                // Stands in for a SIGTERM arriving mid-load.
                cancel.cancel();
                Ok::<_, String>(())
            },
            || {
                opened.set(true);
                Ok::<_, String>(())
            },
        );
        assert_eq!(started.unwrap_err(), StartupAbort::Cancelled);
        assert!(!opened.get(), "the camera factory must never have run");
    }

    /// Each resource's failure keeps its own identity, so the log line names
    /// the thing that actually broke.
    #[test]
    fn each_startup_failure_reports_its_own_resource() {
        let engine_failed = start_engine_then_camera::<(), ()>(
            &CancelToken::new(),
            || Err("no models".into()),
            || panic!("the camera must not open after a failed model load"),
        );
        assert_eq!(
            engine_failed.unwrap_err(),
            StartupAbort::Engine("no models".into())
        );

        let camera_failed = start_engine_then_camera::<(), ()>(
            &CancelToken::new(),
            || Ok(()),
            || Err("device busy".into()),
        );
        assert_eq!(
            camera_failed.unwrap_err(),
            StartupAbort::Camera("device busy".into())
        );
    }
    use facelock_daemon::auth::{AuthOutcome, ErrorKind};

    /// The coupling, in one place.
    ///
    /// A rejection class has four consequences, and they used to be derived
    /// from four independent substring matches on the same English sentence:
    /// the text the user and the wire see, the audit log's `result` label, the
    /// PAM return code, and this binary's exit code. Rewording a message
    /// silently changed PAM policy, and no test spanned the four.
    ///
    /// This table is that test. The first three columns are asserted directly;
    /// the fourth — PAM's code — is asserted where it can be: the two messages
    /// PAM substring-matches are pinned byte-exactly here, in
    /// `facelock-daemon`'s `frozen_protocol_strings_render_byte_exactly`, and
    /// at the wire in `server_authz.rs`. `pam-facelock` cannot be linked here
    /// (its dependency ceiling is libc/toml/serde/zbus), so its matcher is
    /// pinned by those strings rather than by calling it.
    ///
    /// Changing a value here is changing auth policy. Do it deliberately.
    #[test]
    fn every_rejection_class_pins_its_message_audit_label_and_exit_code() {
        // (class, rendered message with detail "boom", audit result, exit code)
        let table: &[(ErrorKind, &str, &str, i32)] = &[
            (ErrorKind::Disabled, "facelock is disabled", "error", 2),
            (ErrorKind::SshSession, "SSH session detected", "error", 2),
            (ErrorKind::LidClosed, "lid closed", "error", 2),
            // Daemon-only (issue #385): the one-shot helper reads the lid in
            // its own namespace and can always answer, so this class only
            // ever comes from the D-Bus transport's logind read failing.
            (
                ErrorKind::LidUnavailable,
                "lid state unavailable",
                "error",
                2,
            ),
            (ErrorKind::Storage, "storage error: boom", "error", 2),
            // Frozen: PAM reads this one as a deliberate lockout
            // (PAM_AUTH_ERR, no oneshot retry) — on the daemon transport via
            // the message, on the oneshot transport via exit 3 (#141). An
            // older module reads 3 as PAM_IGNORE, the pre-#141 collapse.
            (ErrorKind::RateLimited, "rate limited", "rate_limited", 3),
            (
                ErrorKind::RateLimitCheckFailed,
                "rate limit check failed: boom",
                "error",
                2,
            ),
            // Frozen: PAM reads this one as PAM_IGNORE.
            (
                ErrorKind::IrRequired,
                "IR camera required for authentication. Set security.require_ir = false to override (NOT RECOMMENDED).",
                "error",
                2,
            ),
            (
                ErrorKind::Y16BitDepthRequired,
                "Y16 IR texture scale is unverified; authentication requires a verified y16_bit_depth (8..=16) quirk",
                "error",
                2,
            ),
            // Exit 5, not 1: exit 1 is frozen as "scanned and not matched",
            // and the daemon transport abstains (PAM_IGNORE) on a dark scan,
            // so the oneshot side now does too — under every module
            // generation (#141).
            (ErrorKind::AllFramesDark, "all frames dark", "error", 5),
            (ErrorKind::Internal, "boom", "error", 2),
        ];

        for (kind, message, audit_result, exit_code) in table {
            assert_eq!(
                &kind.render("boom"),
                message,
                "{kind:?}: wire/user message changed — PAM and docs/contracts.md read this"
            );
            assert_eq!(
                kind.audit_result(),
                *audit_result,
                "{kind:?}: audit label changed — log consumers read this"
            );
            assert_eq!(
                oneshot_exit_code(*kind),
                *exit_code,
                "{kind:?}: exit code changed — PAM maps this to a return code"
            );
        }

        // A new class must be given a row, not silently inherit a neighbour's
        // policy. `render`, `audit_result` and `oneshot_exit_code` are all
        // exhaustive matches, so this is the only gap left to close.
        assert_eq!(
            table.len(),
            ErrorKind::ALL.len(),
            "every ErrorKind needs a row here"
        );
        for kind in ErrorKind::ALL {
            assert!(
                table.iter().any(|(k, ..)| k == kind),
                "{kind:?} has no row in the coupling table"
            );
        }
    }

    /// Invariants 1 and 2 of the exit-code contract (docs/contracts.md,
    /// "facelock auth Exit Codes"): exit 0 and 1 belong to the matched /
    /// scanned-and-not-matched outcomes permanently, so no rejection class
    /// may claim either — every class code comes from the space (2 and up)
    /// that a module predating it maps to PAM_IGNORE. This is what makes the
    /// upgrade window safe: a new binary under an old module can only ever
    /// degrade a rejection to the historical collapse, never turn one into a
    /// success or a non-match.
    #[test]
    fn rejection_classes_never_claim_the_match_codes() {
        for kind in ErrorKind::ALL {
            let code = oneshot_exit_code(*kind);
            assert!(
                code >= 2,
                "{kind:?} exits {code}: 0 and 1 are frozen for matched / no-match"
            );
        }
        // Compile-time: the suppressed code is a constant, so its half of
        // the invariant needs no runtime assert.
        const { assert!(EXIT_SUPPRESSED >= 2, "suppressed may not claim 0 or 1") }
    }

    /// `run()`'s failure exits, pinned at the source (the function needs a
    /// camera, so its literals cannot be pinned by calling it). Exit 1 is
    /// frozen as "scanned and not matched", so the only site allowed to
    /// produce it is the no-match arm of the auth-response match — a bare
    /// return of 1 is an ad-hoc failure exit that once turned a broken TPM
    /// unseal into PAM_AUTH_ERR on every attempt (a lockout under a
    /// `required` stack). And a failed model-list read must hard-error like
    /// the daemon handler (C3, #105), never degrade to an empty compare set
    /// that guarantees exit 1 and charges the rate limit.
    #[test]
    fn run_has_no_ad_hoc_failure_exits() {
        let src = include_str!("auth.rs");
        // Needles assembled at runtime so this test's own literals cannot
        // satisfy them (include_str! sees this file, tests included).
        let ad_hoc_exit_one = ["return ", "1;"].concat();
        assert!(
            !src.contains(&ad_hoc_exit_one),
            "exit 1 must only come from the no-match arm; storage-shaped \
             failures exit via oneshot_exit_code(ErrorKind::Storage)"
        );
        let folded_list_error = ["listed", ".unwrap_or_default()"].concat();
        assert!(
            !src.contains(&folded_list_error),
            "a failed model-list read must hard-error (C3/#105), not degrade \
             to an empty compare set"
        );
    }

    /// The PAM module's half of the exit-code contract, compiled from its
    /// actual source. `pam_facelock.so` cannot be linked here (its
    /// dependency ceiling is libc/toml/serde/zbus, and the crate builds only
    /// a cdylib), so the module's mapping table is `include!`d — the same
    /// bytes `pam-facelock` compiles — and pinned against
    /// [`oneshot_exit_code`]'s emission table below. Without this the two
    /// halves live in different crates and drift silently.
    mod pam_oneshot_map {
        #![allow(dead_code)]
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../pam-facelock/src/oneshot_exit.rs"
        ));
    }

    /// Transport parity, class for class (#141): the code the binary emits
    /// for a rejection class must map, in the module's oneshot table, to the
    /// PAM code the daemon transport gives the same class (docs/contracts.md,
    /// "PAM Semantics"). Daemon unavailability must not change what PAM
    /// concludes about a rate-limited, suppressed, or dark attempt.
    #[test]
    fn oneshot_exit_codes_map_to_the_daemon_transports_pam_codes() {
        use pam_oneshot_map as pam;
        for kind in ErrorKind::ALL {
            // The daemon transport's consequence for this class: the module
            // substring-matches the frozen "rate limited" message to
            // PAM_AUTH_ERR and maps every other recoverable error message to
            // PAM_IGNORE (`pam_code_for_daemon_error`).
            let daemon_code = match kind {
                ErrorKind::RateLimited => pam::PAM_AUTH_ERR,
                _ => pam::PAM_IGNORE,
            };
            assert_eq!(
                pam::classify(oneshot_exit_code(*kind)).pam_code,
                daemon_code,
                "{kind:?}: the transports disagree"
            );
        }
        // Suppressed: the daemon's `-3` sentinel maps to PAM_AUTHINFO_UNAVAIL.
        assert_eq!(
            pam::classify(EXIT_SUPPRESSED).pam_code,
            pam::PAM_AUTHINFO_UNAVAIL
        );
        // Invariant 1: the permanent codes.
        assert_eq!(pam::classify(0).pam_code, pam::PAM_SUCCESS);
        assert_eq!(pam::classify(1).pam_code, pam::PAM_AUTH_ERR);
        assert_eq!(pam::classify(2).pam_code, pam::PAM_IGNORE);
        // Invariant 3: a code the module does not know abstains, so a newer
        // binary (or a signal death, read as 2) never hardens the stack.
        for unknown in [6, 42, 101, 255] {
            assert_eq!(pam::classify(unknown).pam_code, pam::PAM_IGNORE);
        }
    }

    /// The suppressed outcome is not an `ErrorKind`, so it has no row in the
    /// coupling table; its exit code is pinned here instead. 4 is frozen
    /// protocol: the module maps it to PAM_AUTHINFO_UNAVAIL, the same
    /// consequence the daemon transport's `-3` sentinel produces.
    #[test]
    fn the_preflight_short_circuit_pins_its_non_error_codes() {
        assert_eq!(EXIT_SUPPRESSED, 4, "suppressed exit code is frozen");
        assert_eq!(preflight_exit_code(&AuthOutcome::Suppressed), 4);
        // A rejection class keeps its own code through the short-circuit.
        assert_eq!(
            preflight_exit_code(&AuthOutcome::error(ErrorKind::RateLimited)),
            3
        );
        // Not-enrolled without suppress_unknown: a plain "no opinion", never
        // exit 1 — the camera did not open, so nothing was scanned.
        assert_eq!(
            preflight_exit_code(&AuthOutcome::AuthResult(MatchResult {
                matched: false,
                model_id: None,
                label: None,
                similarity: 0.0,
                face_detected: false,
                failure_reason: None,
            })),
            2
        );
    }
    use facelock_daemon::rate_limit::RateLimiter;
    use facelock_store::FaceStore;
    use std::path::Path;

    /// Config for exercising the oneshot pre-flight gates: audit to a temp
    /// file, SSH/lid gates off so the environment cannot short-circuit first.
    fn gate_config(audit_path: &str, suppress_unknown: bool) -> Config {
        Config::parse(&format!(
            r#"
[security]
require_ir = false
abort_if_ssh = false
abort_if_lid_closed = false
suppress_unknown = {suppress_unknown}

[security.rate_limit]
max_attempts = 2
window_secs = 60

[audit]
enabled = true
path = "{audit_path}"
"#
        ))
        .unwrap()
    }

    fn audit_lines(path: &Path) -> Vec<serde_json::Value> {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        content
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn rate_limiter(config: &Config) -> RateLimiter {
        let rl = &config.security.rate_limit;
        RateLimiter::new(rl.max_attempts, rl.window_secs)
    }

    /// Caps of an IR-classified device (what these tests used to express as a
    /// bare `device_is_ir: true`).
    fn ir_caps() -> facelock_core::types::CameraCaps {
        facelock_core::types::CameraCaps {
            is_ir: true,
            ..Default::default()
        }
    }

    /// #95 symptom (a): a rate-limited oneshot attempt used to be rejected
    /// with no audit record. Unified on `pre_check_audited`, the rejection
    /// must produce the same `rate_limited` entry the daemon path writes,
    /// stamped with the oneshot source.
    #[test]
    fn rate_limit_rejection_on_oneshot_path_writes_audit_record() {
        let dir = tempfile::tempdir().unwrap();
        let audit_path = dir.path().join("audit.jsonl");
        let config = gate_config(audit_path.to_str().unwrap(), false);
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model("alice", "front", &[0.5f32; 512], "test-embedder")
            .unwrap();

        let limiter = rate_limiter(&config);
        limiter.record_failure(&store, "alice").unwrap();
        limiter.record_failure(&store, "alice").unwrap();

        let resp = pre_check_audited(
            &config,
            &store,
            "alice",
            &limiter,
            &ir_caps(),
            AuditSource::Oneshot,
        )
        .expect("rate-limited user must short-circuit");
        assert!(
            matches!(resp, AuthOutcome::Error { kind, .. } if kind == ErrorKind::RateLimited),
            "expected rate-limited error, got {resp:?}"
        );

        let lines = audit_lines(&audit_path);
        assert_eq!(lines.len(), 1, "exactly one audit record for the rejection");
        assert_eq!(lines[0]["result"], "rate_limited");
        assert_eq!(lines[0]["source"], "oneshot");
        assert_eq!(lines[0]["user"], "alice");
    }

    /// #95 symptom (b): `suppress_unknown` was ignored on the oneshot path.
    /// With it enabled, an un-enrolled user must yield `Suppressed` (audited
    /// as such), not a plain not-enrolled failure.
    #[test]
    fn suppress_unknown_honored_on_oneshot_path() {
        let dir = tempfile::tempdir().unwrap();
        let audit_path = dir.path().join("audit.jsonl");
        let config = gate_config(audit_path.to_str().unwrap(), true);
        let store = FaceStore::open_memory().unwrap();

        let resp = pre_check_audited(
            &config,
            &store,
            "nobody",
            &rate_limiter(&config),
            &ir_caps(),
            AuditSource::Oneshot,
        )
        .expect("un-enrolled user must short-circuit");
        assert!(matches!(resp, AuthOutcome::Suppressed));

        let lines = audit_lines(&audit_path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["result"], "suppressed");
        assert_eq!(lines[0]["source"], "oneshot");
    }

    /// Counterpart to the suppress test: with `suppress_unknown` off, the
    /// same un-enrolled user is a plain non-match, audited as `failure` —
    /// matching the daemon path.
    #[test]
    fn no_models_without_suppress_audits_failure() {
        let dir = tempfile::tempdir().unwrap();
        let audit_path = dir.path().join("audit.jsonl");
        let config = gate_config(audit_path.to_str().unwrap(), false);
        let store = FaceStore::open_memory().unwrap();

        let resp = pre_check_audited(
            &config,
            &store,
            "nobody",
            &rate_limiter(&config),
            &ir_caps(),
            AuditSource::Oneshot,
        )
        .expect("un-enrolled user must short-circuit");
        assert!(matches!(
            resp,
            AuthOutcome::AuthResult(MatchResult { matched: false, .. })
        ));

        let lines = audit_lines(&audit_path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["result"], "failure");
        assert_eq!(lines[0]["source"], "oneshot");
    }

    /// An enrolled, un-limited user passes the gates with no audit record —
    /// the auth loop itself owns success/failure auditing.
    #[test]
    fn passing_pre_check_writes_no_audit_record() {
        let dir = tempfile::tempdir().unwrap();
        let audit_path = dir.path().join("audit.jsonl");
        let config = gate_config(audit_path.to_str().unwrap(), false);
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model("alice", "front", &[0.5f32; 512], "test-embedder")
            .unwrap();

        let resp = pre_check_audited(
            &config,
            &store,
            "alice",
            &rate_limiter(&config),
            &ir_caps(),
            AuditSource::Oneshot,
        );
        assert!(resp.is_none(), "gates must pass, got {resp:?}");
        assert!(audit_lines(&audit_path).is_empty());
    }

    // -----------------------------------------------------------------------
    // Enrollment marker convergence (#137)
    // -----------------------------------------------------------------------

    use super::{clear_marker_the_store_contradicts, converge_enrollment_marker};
    use crate::commands::enrollment_marker::{
        MarkerState, marker_dir, read_marker_in, write_marker_in,
    };

    /// A config pointing an entire installation at `dir`: the marker directory
    /// is derived from `storage.db_path`. "alice" has no passwd entry, so the
    /// write skips its `chown` and the test needs no privileges.
    fn marker_config(dir: &std::path::Path) -> Config {
        let mut config = Config::parse("").expect("empty config parses to defaults");
        config.storage.db_path = dir.join("facelock.db").to_string_lossy().into_owned();
        config
    }

    /// #137 on a daemonless install: the marker the upgrade never wrote gets
    /// written the first time the user authenticates — whatever the
    /// authentication then decides, since this runs before it.
    #[test]
    fn oneshot_converges_an_absent_marker_from_the_model_count() {
        let tmp = tempfile::tempdir().unwrap();
        let config = marker_config(tmp.path());
        let base = marker_dir(&config);

        converge_enrollment_marker(&config, "alice", Some(2));

        match read_marker_in(&base, "alice") {
            MarkerState::Enrolled(m) => assert_eq!(m.models, 2),
            other => panic!("expected alice enrolled, got {other:?}"),
        }

        // Same idempotence the daemon path relies on.
        converge_enrollment_marker(&config, "alice", Some(2));
        assert!(matches!(
            read_marker_in(&base, "alice"),
            MarkerState::Enrolled(m) if m.models == 2
        ));
    }

    /// The downward half of #137 on a daemonless install, and the case the
    /// helper this replaces could never actually reach.
    ///
    /// The predecessor test called `converge_enrollment_marker(.., Some(0))`
    /// and asserted the marker went away. It did — but `run` cannot produce
    /// `Some(0)`: the enrollment gate in `pre_check_audited` short-circuits
    /// above that call whenever the store has no models, so the zero branch was
    /// dead on the one-shot path and the test read as coverage of a fix that
    /// was not there. The real evidence arrives on the *rejection* path, which
    /// is where this exercises it: a store that genuinely holds nothing for the
    /// user, and a marker on disk claiming otherwise.
    #[test]
    fn oneshot_rejection_clears_a_marker_the_store_contradicts() {
        let tmp = tempfile::tempdir().unwrap();
        let config = marker_config(tmp.path());
        let base = marker_dir(&config);
        let store = FaceStore::open_memory().unwrap();

        // What a database restored from a pre-enrollment backup leaves behind:
        // a marker with no rows behind it.
        write_marker_in(&base, "alice", 1, None).unwrap();

        clear_marker_the_store_contradicts(&config, &store, "alice");

        assert_eq!(
            read_marker_in(&base, "alice"),
            MarkerState::Absent,
            "a marker the database contradicts must not survive the attempt \
             that observed the contradiction"
        );

        // Idempotent, and the repeat costs one failed unlink — the bound the
        // helper's doc comment claims.
        clear_marker_the_store_contradicts(&config, &store, "alice");
        assert_eq!(read_marker_in(&base, "alice"), MarkerState::Absent);
    }

    /// The other side of the same gate: a rejection for any *other* reason
    /// (rate limited, non-IR, lid closed) must leave an enrolled user's marker
    /// exactly where it is. The store, not the rejection, is the authority.
    #[test]
    fn oneshot_rejection_leaves_an_enrolled_users_marker_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let config = marker_config(tmp.path());
        let base = marker_dir(&config);
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model("alice", "front", &[0.5f32; 512], "test-embedder")
            .unwrap();

        write_marker_in(&base, "alice", 1, None).unwrap();

        clear_marker_the_store_contradicts(&config, &store, "alice");

        assert!(
            matches!(read_marker_in(&base, "alice"), MarkerState::Enrolled(m) if m.models == 1),
            "an enrolled user's marker must survive a rejected attempt"
        );
    }

    /// The rejection path writes nothing — no marker, no marker directory. The
    /// whole reason it is allowed to run on this side of the rate limiter is
    /// that removal is all it can do.
    #[test]
    fn oneshot_rejection_never_creates_a_marker_or_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let config = marker_config(tmp.path());
        let base = marker_dir(&config);
        let store = FaceStore::open_memory().unwrap();

        clear_marker_the_store_contradicts(&config, &store, "alice");

        assert!(
            !base.exists(),
            "a rejected attempt must not materialize the marker directory"
        );
    }

    /// An unreadable store is not evidence of anything. Guessing "zero" from a
    /// failed `list_models` would delete a correct marker on a transient
    /// database error — the opposite of the bug being fixed.
    #[test]
    fn oneshot_convergence_leaves_the_marker_alone_when_the_count_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let config = marker_config(tmp.path());
        let base = marker_dir(&config);

        converge_enrollment_marker(&config, "alice", Some(3));
        converge_enrollment_marker(&config, "alice", None);

        assert!(
            matches!(read_marker_in(&base, "alice"), MarkerState::Enrolled(m) if m.models == 3),
            "an unknown count must not overwrite a known one"
        );
    }

    /// Structural pin, in the style of `resolved.rs`: where the convergence
    /// call sits in `run`'s straight-line code *is* its contract, and the
    /// window it has to sit in is bounded on both sides.
    ///
    /// **Below `pre_check_audited`.** Above it, a marker rewrite — temp file,
    /// `chown`, `rename` — would be reachable by an attempt the pre-flight
    /// gates reject, including a rate-limited one. That is attacker-drivable
    /// filesystem work from the wrong side of the rate limiter, and the whole
    /// reason the call is not simply hoisted to the top of `run`.
    ///
    /// **Above `start_engine_then_camera`.** Below it, convergence inherits
    /// every way the camera bring-up and the scan can end an attempt early —
    /// a cancel token set by a signal, a failed model load, a camera another
    /// process is holding, an embedding that will not decrypt, the no-face
    /// timeout on an empty chair. None of those are evidence about enrollment,
    /// so none of them may decide whether `is-enrolled` tells the truth. This
    /// is the half the camera-lifecycle work (ADR 008) silently broke: it moved
    /// the `list_models` line this call used to sit on to *below* the camera
    /// open, so a textual merge left convergence there.
    ///
    /// The authentication attempt itself is the outer bound, restated because
    /// it is the property #137 is about: convergence must not become
    /// conditional on a face being recognised.
    #[test]
    fn marker_convergence_sits_between_the_gates_and_the_camera() {
        let src = include_str!("auth.rs");
        // First occurrence of each is the one in `run`; the definitions and the
        // tests all live further down the file.
        let pre_check = src
            .find("pre_check_audited(")
            .expect("run() must run the pre-flight gates");
        let converge = src
            .find("converge_enrollment_marker(")
            .expect("run() must converge the marker");
        let camera = src
            .find("start_engine_then_camera(")
            .expect("run() must bring up the engine and camera");
        let authenticate = src
            .find("::authenticate_with_embeddings(")
            .expect("run() must attempt an authentication");

        assert!(
            pre_check < converge,
            "marker convergence must run *after* the pre-flight gates — no \
             filesystem work from the wrong side of the rate limiter"
        );
        assert!(
            converge < camera,
            "marker convergence must run *before* the camera bring-up — every \
             early exit below it is unrelated to whether the user is enrolled"
        );
        assert!(
            converge < authenticate,
            "marker convergence must run before the authentication attempt"
        );

        // Exactly one call site, so no path converges twice (a second write
        // would be harmless but would mean the placement above is no longer
        // the whole story). Everything before the definition is `run` and its
        // doc comment.
        let definition = src
            .find("fn converge_enrollment_marker(")
            .expect("the helper must be defined in this file");
        assert_eq!(
            src[..definition]
                .matches("converge_enrollment_marker(")
                .count(),
            1,
            "run() must converge exactly once"
        );
    }

    /// The companion pin for the downward half. The clearing call has one legal
    /// home: **inside** the pre-flight short-circuit, between the gates and the
    /// `return` that ends the rejected attempt.
    ///
    /// Above `pre_check_audited` it would be a store read (and a marker unlink)
    /// on every attempt regardless of the gates, which is the hoist the #144
    /// review rejected. Below the short-circuit's `return` it would be
    /// unreachable on exactly the path that has the evidence — the not-enrolled
    /// rejection — which is the dead-code shape this change exists to fix.
    #[test]
    fn the_clearing_call_sits_inside_the_pre_flight_short_circuit() {
        let src = include_str!("auth.rs");
        let pre_check = src
            .find("pre_check_audited(")
            .expect("run() must run the pre-flight gates");
        let clear = src
            .find("clear_marker_the_store_contradicts(")
            .expect("run() must clear a marker the store contradicts");
        let converge = src
            .find("converge_enrollment_marker(")
            .expect("run() must converge the marker");

        assert!(
            pre_check < clear,
            "the clearing call must run *after* the gates, not before them"
        );
        assert!(
            clear < converge,
            "the clearing call belongs inside the short-circuit block, which \
             returns before the upward convergence below it"
        );

        // Exactly one call site, for the same reason convergence has one.
        let definition = src
            .find("fn clear_marker_the_store_contradicts(")
            .expect("the helper must be defined in this file");
        assert_eq!(
            src[..definition]
                .matches("clear_marker_the_store_contradicts(")
                .count(),
            1,
            "run() must clear at most once, on the rejection path"
        );
    }
}
