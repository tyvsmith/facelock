//! The `facelock daemon` command: process setup for the D-Bus server.
//!
//! The server itself — the `org.facelock.Daemon` service, per-method
//! authorization, capture slot, idle timeout — lives in
//! `facelock_daemon::server` so its authorization layer is integration-
//! testable (D6). This command keeps the process-level concerns: the root
//! check, tracing init, and building the production handler from config
//! (camera resolution, model/EP probes, state layout — all of which lean on
//! CLI-shared modules like `crate::resolved` and `crate::state_layout`).

use std::path::Path;

use facelock_camera::quirks::QuirksDb;
use facelock_camera::{
    Camera, ResolvedCamera, auto_detect_device_with, is_ir_camera_resolved, validate_device,
};
use facelock_core::config::Config;
use facelock_core::notify::NotifierFactory;
use facelock_daemon::rate_limit::RateLimiter;
use facelock_daemon::server::{ProductionHandler, ProductionRebuild};
use facelock_face::FaceEngine;
use facelock_store::FaceStore;
use tracing::{error, info, warn};

/// Type alias for the camera factory closure.
type CameraFactory = Box<dyn Fn(&Config) -> Result<Camera<'static>, String> + Send + Sync>;

/// Build a new handler from config. Used at startup and for live config reload.
/// Returns the handler and idle_timeout_secs from the loaded config.
///
/// Which file that is comes from `Config::load()` — i.e. from the
/// process-level config override `run` installs — and from nowhere else.
/// This used to take an explicit path for the startup call while the reload
/// recipe passed `None`; the two agreed only because `main` sets the
/// override too. A caller that set one without the other would have started
/// on one file and reloaded from another, silently. One source of truth
/// makes that unrepresentable, and it is the same one `server`'s mtime watch
/// stats (`paths::config_path()`).
fn build_handler() -> Result<(ProductionHandler, u64), String> {
    build_handler_from(load_config_and_ensure_layout()?)
}

/// The privileged prologue: parse the config and converge the on-disk layout.
///
/// Split out of [`build_handler`] because it is the part that needs
/// `CAP_CHOWN`, and the daemon narrows its capabilities between this and the
/// rest of startup (see [`run`]). `build_handler` still runs both halves back
/// to back, because it is also the live-reload recipe — a reload re-reads the
/// config and re-converges the layout exactly as before. What a reload no
/// longer has is `CAP_CHOWN`, which is the documented model rather than a
/// regression: the steady state chowns nothing, and a reload that genuinely
/// needed one now fails the rebuild (the daemon logs it and keeps serving with
/// the handler it already has) instead of quietly succeeding on a thread that
/// happened to still hold the capability.
fn load_config_and_ensure_layout() -> Result<Config, String> {
    // Deliberate re-read (D7): this is the daemon's config lifecycle — one
    // parse at startup, one per mtime-triggered reload (maybe_reload_handler).
    // Everything downstream consumes the Config held by the handler.
    let config = Config::load().map_err(|e| format!("failed to load config: {e}"))?;

    // Before anything opens the store: the daemon runs as root, so this is
    // where an upgraded install converges on the documented modes. A handful
    // of stat calls in the steady state.
    crate::state_layout::ensure_state_layout(&config).map_err(|e| format!("{e:#}"))?;

    Ok(config)
}

/// Everything after the privileged prologue: device resolution, the ONNX
/// engine, the store, the handler. None of it needs a capability, which is why
/// [`run`] can drop them before calling this — and must, because
/// `FaceEngine::load` is where ONNX Runtime brings up its intra-op thread
/// pools, and those threads keep whatever credentials they are born with.
fn build_handler_from(mut config: Config) -> Result<(ProductionHandler, u64), String> {
    let quirks = QuirksDb::load();

    if config.device.path.is_none() {
        // The DB loaded above, not a second load: the quirk set that picks the
        // device must be the one that then classifies it.
        let info = auto_detect_device_with(&quirks)
            .map_err(|e| format!("no camera device specified and auto-detection failed: {e}"))?;
        let is_ir = is_ir_camera_resolved(&info, Some(&quirks));
        info!(device = %info.path, name = %info.name, ir = is_ir, "auto-detected camera device");
        config.device.path = Some(info.path);
    }

    let device_path = config.device.path.clone().unwrap();

    // Resolve and interrogate the device once, up front. The resulting caps
    // gate `pre_check` before any camera is opened, and every camera the
    // factory opens carries the same interrogation. Tolerant of a device
    // that cannot be queried: the daemon still starts (auth keeps falling
    // through to password), with non-IR caps and whatever identity sysfs
    // still offers.
    let resolved = match validate_device(&device_path) {
        Ok(info) => {
            let resolved = ResolvedCamera::interrogate(info, &quirks);
            info!(
                device = %device_path,
                ir = resolved.caps.is_ir,
                name = %resolved.info.name,
                "camera device"
            );
            Some(resolved)
        }
        Err(e) => {
            warn!("failed to query device {device_path}: {e}");
            None
        }
    };
    let device_caps = resolved
        .as_ref()
        .map(|r| r.caps.clone())
        .unwrap_or_else(|| {
            facelock_core::types::CameraCaps::unqueryable(facelock_camera::device_fingerprint(
                &device_path,
            ))
        });

    // Explicit resolution before construction (D7): name what is missing or
    // misconfigured up front — the engine's load error is opaque about
    // missing model files, and ORT falls back to CPU silently when the
    // configured provider isn't compiled into the installed runtime.
    let models = crate::resolved::ModelFiles::probe(&config);
    if !models.all_present() {
        let missing: Vec<String> = models
            .missing()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        return Err(format!(
            "model files missing: {} — run 'sudo facelock setup' to download them",
            missing.join(", ")
        ));
    }
    let ep = crate::resolved::ExecutionProviderFact::probe(&config);
    match &ep.status {
        crate::resolved::EpStatus::Available => {
            info!(provider = %ep.configured, "execution provider resolved");
        }
        crate::resolved::EpStatus::NotBuiltIn => {
            warn!(
                provider = %ep.configured,
                "configured execution provider is not built into the installed \
                 ONNX Runtime; inference will fall back to CPU"
            );
        }
        crate::resolved::EpStatus::Unqueryable(e) => {
            warn!("could not query ONNX Runtime for provider availability: {e}");
        }
        // The engine load below fails with the provider registry's error,
        // which names the valid values.
        crate::resolved::EpStatus::UnknownName => {}
    }

    let engine = FaceEngine::load(&config.recognition, Path::new(&config.daemon.model_dir))
        .map_err(|e| format!("failed to load face engine: {e}"))?;

    // `create`: a fresh install with nobody enrolled still needs a store for
    // rate limiting.
    let store = FaceStore::create(Path::new(&config.storage.db_path))
        .map_err(|e| format!("failed to open database: {e}"))?;

    let rate_limiter = RateLimiter::new(
        config.security.rate_limit.max_attempts,
        config.security.rate_limit.window_secs,
    );

    // Device coupling (Plan 02): the interrogated fingerprint rides on the
    // caps, so enroll can record it and auth can require it. Advisory only
    // (model-granularity, spoofable) — see facelock_core::types::DeviceFingerprint.
    info!(
        device = %device_path,
        device_id = %device_caps.fingerprint.canonical(),
        "camera device fingerprint"
    );

    let idle_timeout_secs = config.daemon.idle_timeout_secs;
    let warmup_override = resolved
        .as_ref()
        .and_then(|r| r.quirk.as_ref())
        .and_then(|q| q.warmup_frames);
    // Resolve and interrogate afresh on every (re)open rather than replaying
    // the startup interrogation. Replaying it would let a device swapped in at
    // the same /dev/videoN after startup inherit the startup device's `is_ir`
    // and fingerprint — caps it never demonstrated — which is exactly the
    // invariant `ResolvedCamera` exists to hold: the caps a camera carries are
    // the caps that camera showed. Reopens happen at idle-timeout frequency,
    // not per frame, so the extra interrogation is not on the hot path. A
    // device that is unqueryable at startup gets the same treatment, and so
    // still surfaces its own open error (or works) if it appears later.
    let camera_factory: CameraFactory = Box::new(move |config: &Config| {
        Camera::open(&config.device, &QuirksDb::load()).map_err(|e| e.to_string())
    });
    let handler = facelock_daemon::handler::Handler::new(
        config,
        engine,
        store,
        rate_limiter,
        device_caps,
        Some(camera_factory),
        warmup_override,
    )?;

    Ok((handler, idle_timeout_secs))
}

/// Converge every enrollment marker with the database, once, at startup (#137).
///
/// Markers are newer than the databases they describe, so an install upgraded
/// from a release without them has enrolled users and an empty `enrolled/` —
/// and `facelock is-enrolled` answers "not enrolled" for a user whose face
/// authentication works. This is the same shape as
/// [`crate::state_layout::ensure_state_layout`] a few lines above in
/// `build_handler`: it does not ask "has this run before?", it re-derives the
/// right answer from the authoritative source, so it needs no ledger and
/// cannot drift from one.
///
/// Deliberately in `run` and not in `build_handler`: the daemon's config
/// reload goes through the builder, and reconciling is a startup concern, not
/// something to repeat on every edit of the config file.
///
/// Best-effort by contract: a marker is a hint for `is-enrolled`, so failing
/// to write one must never stop the daemon from serving authentications.
fn reconcile_enrollment_markers(config: &Config) {
    match crate::commands::enrollment_marker::reconcile_all(config) {
        Ok(()) => info!("enrollment markers reconciled"),
        Err(e) => warn!("enrollment markers not reconciled: {e:#}"),
    }
}

/// `--config` is honored through the process-level override `main` installs
/// before dispatch, which is what `Config::load()` and
/// `paths::config_path()` both read — so startup, the live reload and the
/// mtime watch cannot disagree about which file is the config.
pub fn run(notifier_factory: NotifierFactory) -> anyhow::Result<()> {
    crate::ipc_client::require_root("sudo facelock daemon")?;

    // Init tracing (daemon uses its own tracing setup with target=true).
    // See crate::logging's module doc for why this must not build its own
    // fallback filter by hand.
    tracing_subscriber::fmt()
        .with_env_filter(crate::logging::default_env_filter())
        .with_target(true)
        .init();

    info!("facelock daemon starting");

    // Startup is ordered around one hinge: everything that needs CAP_CHOWN
    // runs first, then the process narrows itself, then everything that
    // creates a thread runs.
    //
    // The narrowing has to land in that gap because capabilities and
    // PR_SET_NO_NEW_PRIVS are per-*thread* and inherited only forwards. Doing
    // it later — as this did, after the bus name was claimed — leaves every
    // thread already in flight holding CAP_CHOWN for the daemon's lifetime:
    // ONNX Runtime's intra-op pools (six threads, spawned inside
    // `FaceEngine::load`) and every tokio worker and blocking thread, which is
    // to say precisely the threads that go on to serve `Authenticate`.
    // `capget` would not have caught it either, being per-thread itself.
    //
    // Here the process is still single-threaded, so the narrowing reaches the
    // whole daemon, and refusing costs nothing anyone can observe: no bus name
    // is claimed yet, so a client sees the same thing it sees when the daemon
    // is not running at all.
    let config = match load_config_and_ensure_layout() {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            std::process::exit(1);
        }
    };

    // The other CAP_CHOWN user, and the last one: markers are chowned to the
    // user each describes. Moved ahead of the handler (it used to read
    // `handler.config`) so it stays on the privileged side of the hinge; it
    // takes the same parsed Config, so this still adds no second read of the
    // config file (resolved::config_load_call_sites_are_pinned).
    reconcile_enrollment_markers(&config);

    let narrowed = facelock_daemon::server::drop_capabilities_or_refuse()?;

    let (handler, idle_timeout_secs) = match build_handler_from(config) {
        Ok(r) => r,
        Err(e) => {
            error!("{e}");
            std::process::exit(1);
        }
    };

    let config_mtime = std::fs::metadata(facelock_core::paths::config_path())
        .and_then(|m| m.modified())
        .ok();

    // The reload recipe: the same builder reading the same file.
    let rebuild: ProductionRebuild = Box::new(|| build_handler().map(|(handler, _idle)| handler));

    facelock_daemon::server::run(
        handler,
        idle_timeout_secs,
        config_mtime,
        Some(rebuild),
        notifier_factory,
        &narrowed,
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::commands::enrollment_marker::{MarkerState, marker_dir, read_marker_in};

    /// A config pointing an entire installation at `dir` — the marker
    /// directory is derived from `storage.db_path`, so this is all it takes.
    fn config_rooted_at(dir: &Path) -> Config {
        let mut config = Config::parse("").expect("empty config parses to defaults");
        config.storage.db_path = dir.join("facelock.db").to_string_lossy().into_owned();
        config
    }

    /// The account the test runs as: `reconcile_all` skips users with no
    /// passwd entry, and `chown`ing to your own uid/gid is a no-op an
    /// unprivileged test may perform.
    fn current_account() -> Option<String> {
        nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .ok()
            .flatten()
            .map(|u| u.name)
    }

    /// #137: a database enrolled before markers existed, plus an empty
    /// `enrolled/`, is exactly the state an upgrade from v0.1.4 leaves behind.
    /// Daemon startup is what fixes it on a daemon-mode install — and fixes it
    /// again, unchanged, on every restart after that.
    #[test]
    fn startup_reconcile_backfills_an_upgraded_install_and_repeats_cleanly() {
        let Some(me) = current_account() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let config = config_rooted_at(tmp.path());

        {
            let store = FaceStore::create(Path::new(&config.storage.db_path)).unwrap();
            store.add_model(&me, "front", &[0.0f32; 512], "").unwrap();
        }
        let base = marker_dir(&config);
        assert!(!base.exists(), "the upgrade leaves no markers behind");

        reconcile_enrollment_markers(&config);
        match read_marker_in(&base, &me) {
            MarkerState::Enrolled(m) => assert_eq!(m.models, 1, "count comes from the database"),
            other => panic!("expected {me} enrolled after startup reconcile, got {other:?}"),
        }

        // Restart: same derivation, same answer, no ledger consulted.
        reconcile_enrollment_markers(&config);
        assert!(matches!(
            read_marker_in(&base, &me),
            MarkerState::Enrolled(m) if m.models == 1
        ));
    }

    /// Failure isolation: reconcile is best-effort, so a database it cannot
    /// open is logged and stepped over. If this ever propagates, a corrupt
    /// state directory stops the daemon from serving authentications — which
    /// is a far worse outcome than a stale `is-enrolled` answer.
    #[test]
    fn startup_reconcile_swallows_a_database_it_cannot_open() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config_rooted_at(tmp.path());
        // Not a database. Stands in for any reconcile failure; asserted to
        // actually fail first so this test cannot go vacuous.
        std::fs::write(&config.storage.db_path, b"not a sqlite file").unwrap();
        assert!(
            crate::commands::enrollment_marker::reconcile_all(&config).is_err(),
            "test setup must produce a reconcile failure to swallow"
        );

        reconcile_enrollment_markers(&config);
    }

    /// Nothing about reconciling may bring a database into existence — the
    /// daemon's own `FaceStore::create` owns that decision, and it has already
    /// run by the time this is called.
    #[test]
    fn startup_reconcile_creates_no_database() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config_rooted_at(tmp.path());

        reconcile_enrollment_markers(&config);

        assert!(
            !Path::new(&config.storage.db_path).exists(),
            "reconcile must not create a database as a side effect"
        );
        assert!(
            marker_dir(&config).is_dir(),
            "the marker directory is still created"
        );
    }
}
