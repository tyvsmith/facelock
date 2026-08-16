//! The `org.facelock.Daemon` D-Bus server: per-method authorization,
//! caller-identity resolution, capture-slot contention control, live config
//! reload, idle timeout, and the serve loop.
//!
//! This lives in the daemon library (not the `facelock` binary) so the
//! authorization layer is reachable from integration tests (D6). Process
//! concerns stay with the binary: the root check, tracing init, and
//! constructing the production handler — the server receives a built handler
//! plus an injected rebuild recipe ([`HandlerRebuild`]) for the live reload.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use facelock_camera::Camera;
use facelock_core::dbus_interface::{
    AuthResult, BUS_NAME, DeviceInfo, ModelInfo, OBJECT_PATH, PreviewFaceInfo,
};
use facelock_core::notify::{Notifier, NotifierFactory, NotifyEvent, notify_desktop_if_enabled};
use facelock_core::traits::{CameraSource, FaceProcessor};
use facelock_face::FaceEngine;
use futures_util::StreamExt;
use nix::unistd::{Uid, User};
use tracing::{error, info, warn};
use zbus::{fdo, interface, object_server::SignalEmitter};

use crate::cancel::CancelToken;
use crate::handler::{AuthIntent, CAMERA_POLL_INTERVAL, DaemonRequest, DaemonResponse, Handler};

/// Production type alias for the handler with real Camera and FaceEngine.
pub type ProductionHandler = Handler<Camera<'static>, FaceEngine>;

/// Rebuilds the handler from the on-disk config. Injected by the binary
/// (which owns config parsing and handler construction) and invoked by the
/// live config reload when the config file's mtime advances. `None` disables
/// live reload (tests).
pub type HandlerRebuild<C, E> = Box<dyn Fn() -> Result<Handler<C, E>, String> + Send + Sync>;

/// [`HandlerRebuild`] with the production camera and engine.
pub type ProductionRebuild = HandlerRebuild<Camera<'static>, FaceEngine>;

/// Failures bringing up or running the D-Bus server.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Bus(#[from] zbus::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The daemon is still holding a capability it promised to drop before
    /// serving anyone. See [`drop_capabilities_or_refuse`].
    #[error("refusing to serve authentications: {0}")]
    Capabilities(String),
}

/// Maximum time to wait for the handler mutex before returning a "busy" error.
/// This prevents D-Bus clients from hanging indefinitely if a previous auth
/// call is stuck (e.g., camera blocking on DQBUF).
const HANDLER_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the suspend path waits for the handler mutex after cancelling.
/// A cancelled request exits within one frame, so this is generous; when it
/// runs out the camera is left to close on the request's own return rather
/// than blocking the suspend transition any longer.
const SUSPEND_RELEASE_WAIT: Duration = Duration::from_secs(1);

/// Try to acquire the handler mutex with a timeout.
/// Uses try_lock in a polling loop to avoid blocking the thread indefinitely.
fn lock_handler_with_timeout<H>(
    handler: &Mutex<H>,
) -> std::result::Result<MutexGuard<'_, H>, fdo::Error> {
    let deadline = Instant::now() + HANDLER_LOCK_TIMEOUT;
    let mut waited = false;
    loop {
        match handler.try_lock() {
            Ok(guard) => {
                if waited {
                    warn!("handler lock acquired after waiting");
                }
                return Ok(guard);
            }
            Err(TryLockError::Poisoned(e)) => {
                error!("handler mutex poisoned (previous operation panicked), recovering");
                return Ok(e.into_inner());
            }
            Err(TryLockError::WouldBlock) => {
                if !waited {
                    warn!("handler lock contention — waiting for previous operation");
                    waited = true;
                }
                if Instant::now() >= deadline {
                    error!(
                        "handler lock timeout after {HANDLER_LOCK_TIMEOUT:?} — previous operation is stuck"
                    );
                    return Err(fdo::Error::Failed(
                        "daemon busy: previous operation timed out".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Tracks whether a camera-capture operation is currently in flight.
///
/// Camera captures serialize on the handler mutex; without this guard a
/// second caller would queue on that mutex for up to `HANDLER_LOCK_TIMEOUT`
/// (10s), letting any authorized caller stall others (local DoS). The slot
/// lets capture methods reject concurrent requests immediately with a
/// "daemon busy" error instead. Callers (PAM, CLI) treat that like any other
/// daemon error and degrade to password auth — never a lockout. Per-user
/// rate limiting is unaffected; this is orthogonal contention control.
#[derive(Debug, Default)]
struct CaptureSlot {
    busy: AtomicBool,
}

impl CaptureSlot {
    /// Try to claim the capture slot. Returns a RAII guard on success, or an
    /// immediate "daemon busy" error if another capture is already in flight.
    fn try_acquire(self: &Arc<Self>, operation: &str) -> fdo::Result<CaptureGuard> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Ok(CaptureGuard(Arc::clone(self)))
        } else {
            warn!(
                operation = operation,
                "capture already in flight — rejecting immediately with busy"
            );
            Err(fdo::Error::Failed(format!(
                "daemon busy: another capture operation is in progress ({operation} rejected)"
            )))
        }
    }
}

/// RAII guard for [`CaptureSlot`]; releases the slot when dropped
/// (including on panic unwind inside a blocking task).
#[derive(Debug)]
struct CaptureGuard(Arc<CaptureSlot>);

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

/// The cancel token of the request currently holding the capture slot, if
/// any — the one handle suspend, `ReleaseCamera` and shutdown have on
/// whatever is in flight.
///
/// They cannot reach it any other way: cancelling means setting a flag the
/// running request reads, and the running request is holding the handler
/// mutex, so anything that has to take that mutex first is already too late.
/// Hence a slot of its own, guarded by a mutex held only long enough to clone
/// a token out of it — never across a capture, never nested inside the
/// handler lock.
///
/// The generation counter is what makes a *stale* token harmless. Requests
/// overlap at the edges (one is delivering its notification while the next
/// has already claimed the capture slot), so "clear the slot when my request
/// ends" must mean "clear it only if it is still mine". Without that, a
/// finishing request would clear its successor's entry and the next
/// `ReleaseCamera` would find an empty slot and cancel nothing.
#[derive(Clone, Debug, Default)]
pub struct CurrentRequest(Arc<CurrentRequestInner>);

#[derive(Debug, Default)]
struct CurrentRequestInner {
    slot: Mutex<Option<(u64, CancelToken)>>,
    generation: AtomicU64,
}

impl CurrentRequest {
    /// Publish `token` as the in-flight request's, until the returned guard
    /// drops. Called once the request is certain to run — after
    /// authorization, after the capture slot is claimed — so a rejected call
    /// never displaces the request it was rejected in favour of.
    fn install(&self, token: CancelToken) -> CurrentRequestGuard {
        let generation = self.0.generation.fetch_add(1, Ordering::Relaxed) + 1;
        *self.lock() = Some((generation, token));
        CurrentRequestGuard {
            current: self.clone(),
            generation,
        }
    }

    /// Cancel whatever is in flight. Lock-free from the request's point of
    /// view (this mutex is not the handler's), and a no-op when the slot is
    /// empty — nothing is running, so there is nothing to stop.
    pub fn cancel(&self) {
        if let Some((_, token)) = self.lock().as_ref() {
            token.cancel();
        }
    }

    /// A poisoned slot is recovered rather than propagated: the only code
    /// that holds this lock stores or clones, so a poisoned mutex means some
    /// *other* thread panicked, and refusing to cancel over it would leave
    /// the camera streaming into a suspend.
    fn lock(&self) -> MutexGuard<'_, Option<(u64, CancelToken)>> {
        match self.0.slot.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Clears the [`CurrentRequest`] slot when its request ends — but only if the
/// slot still names that request (see the generation counter above).
struct CurrentRequestGuard {
    current: CurrentRequest,
    generation: u64,
}

impl Drop for CurrentRequestGuard {
    fn drop(&mut self) {
        let mut slot = self.current.lock();
        if slot
            .as_ref()
            .is_some_and(|(installed, _)| *installed == self.generation)
        {
            *slot = None;
        }
    }
}

/// A running watch on the caller's bus name, aborted when the request that
/// registered it ends.
///
/// The guard matters as much as the watch: without it a watch would outlive
/// its request and cancel *the next one* when the previous caller finally
/// exited.
struct CallerWatch(tokio::task::JoinHandle<()>);

impl Drop for CallerWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Cancel the in-flight request when the caller's D-Bus connection
/// disappears.
///
/// This is what makes an abandoned authentication *visible* to the daemon
/// (ADR 008 §5). A screen locker that aborts PAM because the password was
/// typed first, a killed `sudo`, a crashed client — all of them drop their
/// bus connection, and the bus broadcasts `NameOwnerChanged` with an empty
/// new owner. Without this, the daemon cannot tell that from a slow user and
/// keeps the camera (and the IR emitter) running to `timeout_secs`.
///
/// Best-effort by design: if the subscription cannot be set up, this logs and
/// returns `None`, and that request is bounded by its timeout exactly as
/// every request was before. A client that dies *before* the subscription is
/// established is missed the same way — accepted, since the alternative is
/// synchronizing every method call with the bus.
async fn watch_caller_departure(
    connection: &zbus::Connection,
    sender: Option<&zbus::names::UniqueName<'_>>,
    cancel: CancelToken,
) -> Option<CallerWatch> {
    let sender = sender?.to_string();

    let proxy = match fdo::DBusProxy::new(connection).await {
        Ok(proxy) => proxy,
        Err(e) => {
            warn!(
                sender,
                "cannot watch caller for departure ({e}); request is timeout-bounded"
            );
            return None;
        }
    };
    // Arg-0 match: the bus filters to this name, so every event on the
    // stream is about our caller and nothing else.
    let mut stream = match proxy
        .receive_name_owner_changed_with_args(&[(0, sender.as_str())])
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            warn!(
                sender,
                "cannot watch caller for departure ({e}); request is timeout-bounded"
            );
            return None;
        }
    };

    let watched = sender.clone();
    Some(CallerWatch(tokio::spawn(async move {
        while let Some(signal) = stream.next().await {
            let Ok(args) = signal.args() else { continue };
            let name = args.name().to_string();
            let new_owner = args.new_owner().as_ref().map(|owner| owner.to_string());
            if caller_departed(&watched, &name, new_owner.as_deref()) {
                info!(
                    sender = watched,
                    "caller disconnected, cancelling in-flight request"
                );
                cancel.cancel();
                return;
            }
        }
    })))
}

/// Does this `NameOwnerChanged` event mean the watched caller is gone?
///
/// Two conditions, both required. The name must be the one we registered
/// for — the bus's arg-0 match already guarantees that, and checking it again
/// here is what makes "one request's watch never cancels another's" a
/// property of this crate rather than of a match rule. And the new owner must
/// be absent: an empty new owner is a departure, a different one is a
/// handover of a well-known name, which is not our caller leaving.
fn caller_departed(watched: &str, name: &str, new_owner: Option<&str>) -> bool {
    name == watched && new_owner.is_none_or(str::is_empty)
}

/// Raw camera frames require privilege: only root gets them. When frames are
/// not allowed the bytes are stripped — the caller gets detection and
/// recognition metadata only, never raw camera/IR imagery. Per-method
/// authorization already confines the preview methods to root; this strip is
/// the regression hedge that keeps imagery out of a non-root reply anyway.
fn sanitize_preview_jpeg(jpeg_data: Vec<u8>, allow_frames: bool) -> Vec<u8> {
    if allow_frames { jpeg_data } else { Vec::new() }
}

/// A resolved D-Bus caller: the UID the bus daemon vouches for
/// (`GetConnectionUnixUser`) and its resolved username. Public (with public
/// fields) so integration tests can drive the method-level entry points with
/// synthetic identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerIdentity {
    pub uid: u32,
    pub username: Option<String>,
}

impl CallerIdentity {
    fn display_name(&self) -> String {
        self.username
            .clone()
            .unwrap_or_else(|| format!("UID {}", self.uid))
    }
}

async fn resolve_caller_identity(
    hdr: &zbus::message::Header<'_>,
    connection: &zbus::Connection,
) -> fdo::Result<CallerIdentity> {
    let sender = hdr
        .sender()
        .ok_or_else(|| fdo::Error::Failed("no sender in D-Bus message".into()))?;

    let dbus_proxy = fdo::DBusProxy::new(connection)
        .await
        .map_err(|e| fdo::Error::Failed(format!("failed to create DBus proxy: {e}")))?;
    let uid = dbus_proxy
        .get_connection_unix_user(sender.as_ref().into())
        .await
        .map_err(|e| fdo::Error::Failed(format!("failed to get caller UID: {e}")))?;

    let username = uid_to_username(uid);
    Ok(CallerIdentity { uid, username })
}

/// Declare the D-Bus method vocabulary once: the variants, their wire names,
/// and [`Method::ALL`] all come from this single list.
///
/// The matrix tests iterate `ALL`, so `ALL` being *complete* is what makes
/// them mean anything — and a hand-written second copy of the variant list is
/// exactly the thing that drifts. Generating it removes the possibility: a
/// method added here lands in `ALL` and in `name()` or does not exist. (Drift
/// could only ever under-test, since [`Method::scope`]'s catch-all keeps an
/// unlisted method root-only, but a test that claims completeness should have
/// it.)
macro_rules! declare_methods {
    ($($variant:ident => $wire:literal,)+) => {
        /// Every method on the `org.facelock.Daemon` D-Bus interface. Keep in
        /// sync with the `#[interface]` block below — the one direction no
        /// type can enforce, and what
        /// `interface_methods_and_the_authz_matrix_are_the_same_set` pins by
        /// scanning this file. This enum plus [`Method::scope`] is the
        /// authorization matrix; the in-module unit tests pin the table
        /// itself, and tests/server_authz.rs exercises it through the
        /// method-level entry points (D6).
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        enum Method {
            $($variant,)+
        }

        impl Method {
            /// Every variant, complete by construction — see
            /// [`declare_methods`].
            #[cfg(test)]
            const ALL: &'static [Method] = &[$(Method::$variant,)+];

            /// The wire name, which is what denial messages and capture-slot
            /// contention errors quote.
            fn name(self) -> &'static str {
                match self {
                    $(Method::$variant => $wire,)+
                }
            }
        }
    };
}

declare_methods! {
    Authenticate => "Authenticate",
    TestAuthenticate => "TestAuthenticate",
    Enroll => "Enroll",
    ListModels => "ListModels",
    RemoveModel => "RemoveModel",
    ClearModels => "ClearModels",
    PreviewFrame => "PreviewFrame",
    PreviewDetectFrame => "PreviewDetectFrame",
    ListDevices => "ListDevices",
    ReleaseCamera => "ReleaseCamera",
    Ping => "Ping",
    Shutdown => "Shutdown",
}

impl Method {
    /// Authorization target for each method.
    ///
    /// `Authenticate` is the only user-scoped method: screen lockers run
    /// their PAM stack as the user, so a user must be able to request
    /// authentication for themselves — that is architecture, not policy.
    /// Everything else is root-only. In particular `PreviewDetectFrame`,
    /// which runs per-frame with no rate limit, must never be reachable by
    /// an unprivileged caller: together with score redaction this closes the
    /// similarity hill-climbing oracle by construction. The catch-all arm
    /// makes any future method root-only until it is deliberately opened up.
    ///
    /// `TestAuthenticate` is listed explicitly rather than left to the
    /// catch-all: it is the entry point that does *not* charge the rate
    /// limit, so its root-only scope is the whole reason it is safe to
    /// offer, not an incidental default.
    fn scope(self) -> Scope {
        match self {
            Method::Authenticate => Scope::UserScoped,
            Method::TestAuthenticate => Scope::Root,
            _ => Scope::Root,
        }
    }
}

/// Who may call a D-Bus method. The bus policy admits root and the facelock
/// group to the whole interface; this in-daemon check (keyed on the caller
/// UID from `GetConnectionUnixUser`) is the per-method decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    /// Root only.
    Root,
    /// Root, or a non-root caller acting on their own username.
    UserScoped,
}

/// The single per-method authorization decision point. `target_user` is the
/// username a user-scoped method acts on; root-scoped methods ignore it.
/// Fails closed: a user-scoped call without a target user is denied.
fn authorize_method(
    caller: &CallerIdentity,
    method: Method,
    target_user: Option<&str>,
) -> fdo::Result<()> {
    match method.scope() {
        Scope::Root => require_root(caller, method.name()),
        Scope::UserScoped => {
            let user = target_user.ok_or_else(|| {
                fdo::Error::Failed(format!("{} requires a target user", method.name()))
            })?;
            require_user_authorized(caller, user, method.name())
        }
    }
}

fn require_root(caller: &CallerIdentity, operation: &str) -> fdo::Result<()> {
    if caller.uid == 0 {
        return Ok(());
    }

    let caller_name = caller.display_name();
    warn!(
        operation = operation,
        caller_uid = caller.uid,
        caller_name = %caller_name,
        "D-Bus caller not authorized for privileged operation"
    );
    Err(fdo::Error::AccessDenied(format!(
        "{operation} requires root (caller: '{caller_name}', UID {})",
        caller.uid
    )))
}

fn require_user_authorized(
    caller: &CallerIdentity,
    user: &str,
    operation: &str,
) -> fdo::Result<()> {
    if caller.uid == 0 {
        return Ok(());
    }

    let caller_name = caller.username.clone().ok_or_else(|| {
        fdo::Error::Failed(format!("failed to resolve UID {} to username", caller.uid))
    })?;

    if caller_name == user {
        return Ok(());
    }

    warn!(
        operation = operation,
        caller_uid = caller.uid,
        caller_name = %caller_name,
        requested_user = %user,
        "D-Bus caller not authorized to act on behalf of requested user"
    );
    Err(fdo::Error::AccessDenied(format!(
        "{operation} not authorized: caller '{caller_name}' (UID {}) cannot act as '{user}'",
        caller.uid
    )))
}

fn uid_to_username(uid: u32) -> Option<String> {
    User::from_uid(Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|user| user.name)
}

/// Current time as seconds since an arbitrary epoch (Instant-based).
/// Used for idle timeout tracking without wall-clock dependency.
fn now_secs() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_secs()
}

/// Encode a recoverable authentication error into the `AuthResult` wire
/// format (`model_id == -2`, `label` = error message) instead of a D-Bus
/// error.
///
/// A D-Bus error reply makes clients treat the daemon as broken: the PAM
/// module would fall back to a fresh root oneshot attempt, silently
/// escalating past daemon-side state such as rate limiting. In-band encoding
/// lets the PAM client classify the error (rate limited → PAM_AUTH_ERR,
/// everything else → PAM_IGNORE) without retrying.
/// See docs/contracts.md ("Authenticate error encoding").
fn recoverable_auth_error(message: String) -> AuthResult {
    AuthResult {
        matched: false,
        model_id: -2,
        label: message,
        similarity: 0.0,
    }
}

/// `model_id` for an unmatched attempt in which the detector saw nobody (and
/// for the pre-camera gates, which reject before a face could be seen).
const NO_MATCH_NO_FACE: i32 = -1;

/// `model_id` for an unmatched attempt in which the detector *did* see a face.
///
/// PAM needs this distinction to choose `PAM_AUTH_ERR` (we looked at you and
/// said no) over `PAM_IGNORE` (we have no opinion), and it cannot read it off
/// `similarity`, which is redacted to `0.0` for every non-root caller — so a
/// hyprlock user's genuine no-match used to be indistinguishable from an empty
/// frame (#108's N12, deferred to #109 and never carried).
///
/// A pre-`-4` PAM module decodes this as a plain no-match (its `match` falls
/// through to the same arm as `-1`), so the sentinel is safe to emit at a
/// daemon that is newer than the installed module.
const NO_MATCH_FACE_SEEN: i32 = -4;

/// The `model_id` field for a [`MatchResult`] on the wire.
///
/// A matched attempt carries the winning model's id; an unmatched one carries
/// the sentinel that says whether a face was there at all.
fn wire_model_id(result: &facelock_core::types::MatchResult) -> i32 {
    match result.model_id {
        Some(id) => id as i32,
        None if result.face_detected && !result.matched => NO_MATCH_FACE_SEEN,
        None => NO_MATCH_NO_FACE,
    }
}

/// The `org.facelock.Daemon` service.
///
/// Generic over the handler's camera and engine so integration tests can
/// construct it around mocks ([`FacelockService::new`]) and drive the
/// method-level entry points (`*_as`) with synthetic caller identities (D6).
/// The `#[interface]` block below binds the production types and owns only
/// the zbus glue: caller-identity resolution and signal emission.
pub struct FacelockService<C, E>
where
    C: CameraSource + Send + 'static,
    E: FaceProcessor + Send + 'static,
{
    handler: Arc<Mutex<Handler<C, E>>>,
    /// Timestamp of last D-Bus method call (seconds since daemon start).
    last_activity: Arc<AtomicU64>,
    /// Config file mtime when the handler was last built.
    /// Used to detect config changes and reload on next request.
    config_mtime: Arc<Mutex<Option<std::time::SystemTime>>>,
    /// In-flight guard for camera-capture operations (DoS control).
    capture_slot: Arc<CaptureSlot>,
    /// The cancel token of the request currently holding that capture slot,
    /// so suspend, `ReleaseCamera` and shutdown can stop it **without taking
    /// the handler mutex** — which is exactly what the request being
    /// cancelled is holding (ADR 008 §5).
    current: CurrentRequest,
    /// Builds per-user notifiers for auth outcomes. Injected from `main` so
    /// the server never names the delivery implementation (D9) — a
    /// prerequisite for moving this server out of facelock-cli.
    notifier_factory: NotifierFactory,
    /// Rebuilds the handler from on-disk config for the live reload. `None`
    /// disables reload (tests).
    rebuild: Option<HandlerRebuild<C, E>>,
}

impl<C, E> FacelockService<C, E>
where
    C: CameraSource + Send + 'static,
    E: FaceProcessor + Send + 'static,
{
    /// Construct the service around a built handler. [`run_dbus_server`]
    /// does this with production types; integration tests with mocks.
    pub fn new(
        handler: Handler<C, E>,
        startup_config_mtime: Option<std::time::SystemTime>,
        rebuild: Option<HandlerRebuild<C, E>>,
        notifier_factory: NotifierFactory,
    ) -> Self {
        Self {
            handler: Arc::new(Mutex::new(handler)),
            last_activity: Arc::new(AtomicU64::new(now_secs())),
            config_mtime: Arc::new(Mutex::new(startup_config_mtime)),
            capture_slot: Arc::new(CaptureSlot::default()),
            // The slot starts empty: nothing is in flight, so there is
            // nothing to cancel. It survives a config reload untouched
            // because it holds a *request's* token, not the handler's — and
            // reload only runs between requests anyway.
            current: CurrentRequest::default(),
            notifier_factory,
            rebuild,
        }
    }

    /// The in-flight request's cancel token, for the suspend watcher
    /// `run_dbus_server` spawns and for the tests that stand in for it.
    pub fn current_request(&self) -> CurrentRequest {
        self.current.clone()
    }

    /// Check if the config file has been modified since the handler was built.
    /// If so, reload config, rebuild the engine/store/handler, and swap it in.
    /// Called at the start of authenticate and enroll — the two methods that
    /// depend on cached ONNX models and config values.
    fn maybe_reload_handler(&self) {
        // No rebuild recipe injected (tests): live reload is disabled.
        let Some(rebuild) = self.rebuild.as_ref() else {
            return;
        };
        let config_path = facelock_core::paths::config_path();
        let current_mtime = std::fs::metadata(&config_path)
            .and_then(|m| m.modified())
            .ok();

        // A poisoned lock is not a reason to reload: keep serving with the
        // handler already built, exactly as the two swap sites below do.
        let needs_reload = match self.config_mtime.lock() {
            Ok(stored) => matches!((*stored, current_mtime), (Some(old), Some(new)) if new > old),
            Err(_) => false,
        };

        if !needs_reload {
            return;
        }

        info!("config file changed, reloading");

        let new_handler = match rebuild() {
            Ok(handler) => handler,
            Err(e) => {
                warn!("failed to reload config: {e} — continuing with old config");
                return;
            }
        };

        // Swap in the new handler
        if let Ok(mut guard) = self.handler.lock() {
            *guard = new_handler;
        }

        // Update stored mtime
        if let Ok(mut stored) = self.config_mtime.lock() {
            *stored = current_mtime;
        }

        info!("handler reloaded with new config");
    }

    // ------------------------------------------------------------------
    // Method-level entry points (D6): everything each wire method does
    // except zbus mechanics — activity/reload bookkeeping, authorization,
    // capture-slot contention, handler dispatch, response mapping, and
    // similarity redaction. `caller` arrives resolved, so integration tests
    // exercise the full path with synthetic identities; production resolves
    // it from the message header in the `#[interface]` glue below.
    // ------------------------------------------------------------------

    /// The real-authentication entry point: every PAM stack, every locker,
    /// the polkit agent. A failed attempt ALWAYS charges the rate-limit
    /// budget, whatever the caller's UID.
    ///
    /// It used to charge only non-root callers, on the theory that a root
    /// caller must be root-only `facelock test` (N11). That inference was
    /// wrong in the direction that matters: `sudo` is setuid-root, and
    /// `login`, `su` and root-run greeters run their PAM stack as root too,
    /// so real failed authentications arrived here as UID 0 and were never
    /// charged. The diagnostic carve-out now lives in
    /// [`FacelockService::test_authenticate_as`], where it is asked for
    /// explicitly instead of inferred.
    ///
    /// `cancel` is this request's token and nobody else's — the glue mints it
    /// per call and subscribes the caller-departure watch to it, so a second
    /// caller (even one about to be denied) can neither clear it nor have its
    /// own departure land on this request (ADR 008 §5).
    pub async fn authenticate_as(
        &self,
        caller: CallerIdentity,
        user: &str,
        cancel: CancelToken,
    ) -> fdo::Result<AuthResult> {
        self.run_authentication(
            caller,
            user,
            Method::Authenticate,
            AuthIntent::Authenticate,
            cancel,
        )
        .await
    }

    /// The root-only diagnostic entry point behind `facelock test` (N11,
    /// issue #96). Same authentication, same reply shape, two deliberate
    /// differences: a failed attempt charges no rate-limit budget, and the
    /// SSH/lid physical-presence gates are skipped — an admin who is already
    /// root may legitimately diagnose recognition over SSH or with the lid
    /// closed on a docked laptop. Everything else (`disabled`, enrollment,
    /// the rate-limit *check*, `require_ir`) still applies.
    ///
    /// Root-only is what makes that safe, and it is enforced by the same
    /// table-driven [`authorize_method`] as every other privileged method.
    pub async fn test_authenticate_as(
        &self,
        caller: CallerIdentity,
        user: &str,
        cancel: CancelToken,
    ) -> fdo::Result<AuthResult> {
        self.run_authentication(
            caller,
            user,
            Method::TestAuthenticate,
            AuthIntent::Test,
            cancel,
        )
        .await
    }

    /// The body both authentication entry points share, so the diagnostic
    /// method cannot drift from the real one: same authorization table, same
    /// capture slot, same handler call, same in-band error encoding, same
    /// notification, same redaction. Only `method` (which authorization
    /// applies) and `intent` (what the attempt costs and which gates run)
    /// differ.
    async fn run_authentication(
        &self,
        caller: CallerIdentity,
        user: &str,
        method: Method,
        intent: AuthIntent,
        cancel: CancelToken,
    ) -> fdo::Result<AuthResult> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        self.maybe_reload_handler();
        authorize_method(&caller, method, Some(user))?;
        let caller_is_root = caller.uid == 0;
        let capture_guard = self.capture_slot.try_acquire(method.name())?;
        // Publish the token only now. Both `?` above return without ever
        // touching the slot, which is the point: the capture slot is what
        // decides which request is *the* request, so a denied or busy-rejected
        // call must not be able to displace the entry belonging to the one
        // actually running — nor to leave its own dead token behind for the
        // next suspend to cancel instead. Dropped at the end of this method.
        let _current = self.current.install(cancel.clone());
        let handler = self.handler.clone();
        let notifier_factory = self.notifier_factory.clone();
        let user = user.to_string();
        let result = tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let response = handler.handle_authenticate(user.clone(), intent, &cancel);
            // Notification settings come from the handler's config — the
            // freshest parse, since maybe_reload_handler ran at method entry.
            // No mid-request file re-read (D7).
            let notify_config = handler.config.notification.clone();
            drop(handler);
            // Capture finished — free the slot before slower follow-up work
            // (notifications) so the next auth isn't rejected needlessly.
            drop(capture_guard);
            match response {
                DaemonResponse::AuthResult(result) => {
                    // Desktop notification, delivered as root via setpriv. NOT
                    // fire-and-forget: the delivery path runs the helper with
                    // `Command::output()`, which waits for the child, and this
                    // runs before the reply below is built — so an auth reply
                    // waits on it. The `Notifier` contract is that delivery
                    // must not FAIL an authentication (errors are logged and
                    // swallowed); staying cheap enough not to delay one is an
                    // obligation of the implementation, not a guarantee of
                    // this call site.
                    notify_auth_outcome(&notify_config, notifier_factory(&user).as_ref(), &result);

                    Ok(AuthResult {
                        matched: result.matched,
                        model_id: wire_model_id(&result),
                        label: result.label.unwrap_or_default(),
                        similarity: result.similarity as f64,
                    })
                }
                DaemonResponse::Suppressed => {
                    // No enrolled models + suppress_unknown enabled.
                    // Return model_id=-3 as a marker so the PAM module
                    // can map this to PAM_AUTHINFO_UNAVAIL.
                    Ok(AuthResult {
                        matched: false,
                        model_id: -3,
                        label: String::new(),
                        similarity: 0.0,
                    })
                }
                DaemonResponse::Error { message } => Ok(recoverable_auth_error(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?;

        // The similarity score is root-only (a hill-climbing oracle
        // otherwise); the score has already reached the audit log unredacted
        // inside the handler.
        result.map(|auth| auth.redact_similarity_unless_root(caller_is_root))
    }

    /// `cancel` is this request's own token; see [`Self::authenticate_as`].
    pub async fn enroll_as(
        &self,
        caller: CallerIdentity,
        user: &str,
        label: &str,
        cancel: CancelToken,
    ) -> fdo::Result<(u32, u32)> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        self.maybe_reload_handler();
        authorize_method(&caller, Method::Enroll, None)?;
        let capture_guard = self.capture_slot.try_acquire("Enroll")?;
        // Same ordering rule as `run_authentication`: authorized and holding
        // the capture slot, therefore this is the request in flight.
        let _current = self.current.install(cancel.clone());
        let handler = self.handler.clone();
        let user = user.to_string();
        let label = label.to_string();
        tokio::task::spawn_blocking(move || {
            let _capture_guard = capture_guard;
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::Enroll { user, label };
            let response = handler.handle_with_cancel(request, &cancel);
            match response {
                DaemonResponse::Enrolled {
                    model_id,
                    embedding_count,
                } => Ok((model_id, embedding_count)),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    pub async fn list_models_as(
        &self,
        caller: CallerIdentity,
        user: &str,
    ) -> fdo::Result<Vec<ModelInfo>> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        authorize_method(&caller, Method::ListModels, None)?;
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ListModels { user };
            let response = handler.handle(request);
            match response {
                DaemonResponse::Models(models) => Ok(models
                    .into_iter()
                    .map(|m| ModelInfo {
                        id: m.id,
                        user: m.user,
                        label: m.label,
                        created_at: m.created_at,
                        embedder_model: m.embedder_model,
                        // D-Bus has no Option: empty string == NULL/legacy.
                        device_id: m.device_id.unwrap_or_default(),
                    })
                    .collect()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    pub async fn remove_model_as(
        &self,
        caller: CallerIdentity,
        user: &str,
        model_id: u32,
    ) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        authorize_method(&caller, Method::RemoveModel, None)?;
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::RemoveModel { user, model_id };
            let response = handler.handle(request);
            match response {
                // C8 (Phase E): the wire reply is unit, so "removed" and
                // "nothing to remove" are indistinguishable to the caller.
                DaemonResponse::Removed => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    pub async fn clear_models_as(&self, caller: CallerIdentity, user: &str) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        authorize_method(&caller, Method::ClearModels, None)?;
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ClearModels { user };
            let response = handler.handle(request);
            match response {
                DaemonResponse::Removed => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    pub async fn preview_frame_as(&self, caller: CallerIdentity) -> fdo::Result<Vec<u8>> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        authorize_method(&caller, Method::PreviewFrame, None)?;
        let capture_guard = self.capture_slot.try_acquire("PreviewFrame")?;
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let _capture_guard = capture_guard;
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::PreviewFrame;
            let response = handler.handle(request);
            match response {
                DaemonResponse::Frame { jpeg_data } => Ok(jpeg_data),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    pub async fn preview_detect_frame_as(
        &self,
        caller: CallerIdentity,
        user: &str,
    ) -> fdo::Result<(Vec<u8>, Vec<PreviewFaceInfo>)> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        // Root-only: preview runs per-frame with neither pre_check nor the
        // rate limiter, so for any weaker caller this method would be a
        // continuous similarity feed at camera framerate.
        authorize_method(&caller, Method::PreviewDetectFrame, None)?;
        let caller_is_root = caller.uid == 0;

        let capture_guard = self.capture_slot.try_acquire("PreviewDetectFrame")?;
        let handler = self.handler.clone();
        let user = user.to_string();
        tokio::task::spawn_blocking(move || {
            let _capture_guard = capture_guard;
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::PreviewDetectFrame { user };
            let response = handler.handle(request);
            match response {
                DaemonResponse::DetectFrame { jpeg_data, faces } => {
                    let jpeg_data = sanitize_preview_jpeg(jpeg_data, caller_is_root);
                    let face_infos: Vec<PreviewFaceInfo> = faces
                        .into_iter()
                        .map(|f| {
                            PreviewFaceInfo {
                                x: f.x as f64,
                                y: f.y as f64,
                                width: f.width as f64,
                                height: f.height as f64,
                                confidence: f.confidence as f64,
                                similarity: f.similarity as f64,
                                recognized: f.recognized,
                            }
                            // Defense in depth: authorization above already
                            // restricts this method to root, but the score
                            // must stay redacted even if that ever regresses.
                            .redact_similarity_unless_root(caller_is_root)
                        })
                        .collect();
                    Ok((jpeg_data, face_infos))
                }
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    pub async fn list_devices_as(&self, caller: CallerIdentity) -> fdo::Result<Vec<DeviceInfo>> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        authorize_method(&caller, Method::ListDevices, None)?;
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ListDevices;
            let response = handler.handle(request);
            match response {
                // C9 (Phase E): the wire DeviceInfo carries no formats, so
                // per-device format/resolution detail is dropped here.
                DaemonResponse::Devices(devices) => Ok(devices
                    .into_iter()
                    .map(|d| DeviceInfo {
                        path: d.path,
                        name: d.name,
                        driver: d.driver,
                        is_ir: d.is_ir,
                    })
                    .collect()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    pub async fn release_camera_as(&self, caller: CallerIdentity) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        authorize_method(&caller, Method::ReleaseCamera, None)?;
        // Cancel the in-flight request before queuing for the handler mutex.
        // If a capture is in flight it holds that mutex, so the store below is
        // the only thing that reaches it — and it is what lets the lock be
        // acquired at all, within one frame instead of at `timeout_secs`.
        self.current.cancel();
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            let request = DaemonRequest::ReleaseCamera;
            let response = handler.handle(request);
            match response {
                DaemonResponse::Ok => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }

    pub async fn ping_as(&self, caller: CallerIdentity) -> fdo::Result<String> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        authorize_method(&caller, Method::Ping, None)?;
        Ok("pong".to_string())
    }

    pub async fn shutdown_as(&self, caller: CallerIdentity) -> fdo::Result<()> {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
        authorize_method(&caller, Method::Shutdown, None)?;
        // Same reason as `ReleaseCamera`: stop the capture before waiting on
        // the mutex it is holding.
        self.current.cancel();
        let handler = self.handler.clone();
        tokio::task::spawn_blocking(move || {
            let mut handler = lock_handler_with_timeout(&handler)?;
            match handler.handle(DaemonRequest::Shutdown) {
                DaemonResponse::Ok => Ok(()),
                DaemonResponse::Error { message } => Err(fdo::Error::Failed(message)),
                other => Err(fdo::Error::Failed(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        })
        .await
        .map_err(|e| fdo::Error::Failed(format!("task join error: {e}")))?
    }
}

#[interface(name = "org.facelock.Daemon")]
impl FacelockService<Camera<'static>, FaceEngine> {
    async fn authenticate(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        user: &str,
    ) -> fdo::Result<AuthResult> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        // One token for this call and nothing else. zbus dispatches each
        // method in its own task, so anything shared between calls is shared
        // between *concurrent* calls: a token owned by the service could be
        // cleared out from under an in-flight request by a second caller, and
        // a departure watch subscribed against it would cancel whoever
        // happened to be running when some other client exited. Minted fresh
        // here, watched here, and passed down — nobody else can reach it
        // (ADR 008 §5).
        let cancel = CancelToken::new();
        let _watch = watch_caller_departure(connection, hdr.sender(), cancel.clone()).await;
        let result = self.authenticate_as(caller, user, cancel).await;

        // Emit auth_attempted signal (best-effort, don't fail auth if signal
        // fails). The payload deliberately carries no similarity score — the
        // raw biometric score is a spoof-tuning oracle for anyone able to
        // receive the broadcast; `matched` + user is enough for consumers.
        if let Ok(ref auth_result) = result {
            let _ = Self::auth_attempted(&ctxt, user, auth_result.matched).await;
        }

        result
    }

    /// The root-only diagnostic counterpart of `Authenticate`, behind
    /// `facelock test`. Identical wire shape (`s` in, `AuthResult` out,
    /// same `-1`/`-2`/`-3` sentinels) — see
    /// [`FacelockService::test_authenticate_as`] for the two behavioral
    /// differences.
    async fn test_authenticate(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        user: &str,
    ) -> fdo::Result<AuthResult> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        let cancel = CancelToken::new();
        let _watch = watch_caller_departure(connection, hdr.sender(), cancel.clone()).await;
        let result = self.test_authenticate_as(caller, user, cancel).await;

        // Emitted for the same reason and with the same payload as
        // `Authenticate`'s: a camera-backed attempt happened for `user`.
        if let Ok(ref auth_result) = result {
            let _ = Self::auth_attempted(&ctxt, user, auth_result.matched).await;
        }

        result
    }

    async fn enroll(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
        label: &str,
    ) -> fdo::Result<(u32, u32)> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        // `facelock enroll` is a long capture loop; Ctrl-C on the CLI drops
        // its bus connection and must end it, not leave the camera running
        // to the enrollment deadline.
        let cancel = CancelToken::new();
        let _watch = watch_caller_departure(connection, hdr.sender(), cancel.clone()).await;
        self.enroll_as(caller, user, label, cancel).await
    }

    async fn list_models(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
    ) -> fdo::Result<Vec<ModelInfo>> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.list_models_as(caller, user).await
    }

    async fn remove_model(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
        model_id: u32,
    ) -> fdo::Result<()> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.remove_model_as(caller, user, model_id).await
    }

    async fn clear_models(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
    ) -> fdo::Result<()> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.clear_models_as(caller, user).await
    }

    async fn preview_frame(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<Vec<u8>> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.preview_frame_as(caller).await
    }

    async fn preview_detect_frame(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        user: &str,
    ) -> fdo::Result<(Vec<u8>, Vec<PreviewFaceInfo>)> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.preview_detect_frame_as(caller, user).await
    }

    async fn list_devices(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<Vec<DeviceInfo>> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.list_devices_as(caller).await
    }

    async fn release_camera(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<()> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.release_camera_as(caller).await
    }

    async fn ping(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<String> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.ping_as(caller).await
    }

    async fn shutdown(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<()> {
        let caller = resolve_caller_identity(&hdr, connection).await?;
        self.shutdown_as(caller).await
    }

    /// Signal emitted after each authentication attempt.
    ///
    /// Carries only the user and the match outcome — never the raw
    /// similarity score (an information leak / spoof-tuning oracle).
    /// The bus policy additionally restricts who may receive this signal.
    #[zbus(signal)]
    async fn auth_attempted(
        emitter: &SignalEmitter<'_>,
        user: &str,
        matched: bool,
    ) -> zbus::Result<()>;
}

/// Map an auth outcome to its desktop notification and deliver it through
/// the injected notifier, honoring the notification config.
///
/// Pure decision + injected delivery: the tests below assert emit/no-emit
/// with a recording notifier; production passes the per-user desktop
/// notifier built by the injected [`NotifierFactory`].
fn notify_auth_outcome(
    config: &facelock_core::config::NotificationConfig,
    notifier: &dyn Notifier,
    result: &facelock_core::types::MatchResult,
) {
    let event = if result.matched {
        NotifyEvent::Success {
            label: result.label.clone(),
            similarity: result.similarity,
        }
    } else {
        NotifyEvent::Failure {
            reason: "no match".into(),
        }
    };
    notify_desktop_if_enabled(config, notifier, &event);
}

/// Run the daemon's D-Bus server until shutdown (signal, D-Bus `Shutdown`,
/// or idle timeout). Blocking: builds its own multi-threaded tokio runtime.
///
/// Takes a [`CapabilitiesDropped`] because building that runtime is the point
/// of no return for the capability narrowing: `Builder::build()` spawns the
/// worker threads, each worker's blocking-pool threads descend from a worker,
/// and every D-Bus method body runs its real work on one of those. A thread
/// keeps the credentials it was created with, so a narrowing that happens
/// after this line reaches none of them. The token is the caller's proof it
/// happened before.
pub fn run(
    handler: ProductionHandler,
    idle_timeout_secs: u64,
    startup_config_mtime: Option<std::time::SystemTime>,
    rebuild: Option<ProductionRebuild>,
    notifier_factory: NotifierFactory,
    _narrowed: &CapabilitiesDropped,
) -> Result<(), ServerError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(run_dbus_server(
        handler,
        idle_timeout_secs,
        startup_config_mtime,
        rebuild,
        notifier_factory,
    ))
}

/// Bitmask (low 32-bit word, caps 0-31) of the capabilities the daemon keeps
/// after startup: CAP_SETUID (bit 7) and CAP_SETGID (bit 6).
///
/// These two are required for the desktop-notification privilege-drop: the
/// daemon runs as root and execs `runuser`/`su` to `setgroups()` + `setuid()`
/// into the user's session bus (see `notifications.rs::send_as_user`). Under
/// `NoNewPrivileges` that exec cannot regain privilege, so the caps must be
/// retained — and held in the inheritable set so systemd `AmbientCapabilities`
/// survives the exec into the non-setuid `runuser`. Every other capability is
/// dropped. Factored into a pure `const fn` so the mask can be unit-tested
/// without calling `capset` (which needs privilege and may fail in CI).
const fn retained_capability_mask() -> u32 {
    // CAP_SETGID = 6, CAP_SETUID = 7.
    (1 << 7) | (1 << 6)
}

/// The part of [`retained_capability_mask`] this process can actually keep,
/// given what it currently holds as `permitted`.
///
/// `capset(2)` requires the new permitted set to be a subset of the old one,
/// and enforces it *wholesale*: a request naming one capability the process
/// does not have is rejected in its entirety and drops nothing at all. So the
/// retained mask cannot be requested absolutely. An operator who wants no
/// desktop notifications and writes a drop-in with
/// `CapabilityBoundingSet=CAP_CHOWN` + `AmbientCapabilities=` starts the
/// daemon with `permitted == {CAP_CHOWN}`; an absolute request for
/// `{CAP_SETUID, CAP_SETGID}` returns `EPERM`, `CAP_CHOWN` survives, the
/// read-back below calls that a violation, and `Restart=on-failure` turns a
/// legitimate narrower configuration into a permanent 3-second restart loop —
/// with the journal blaming the wrong thing.
///
/// Intersecting first makes the call only ever *remove*, so it cannot fail for
/// this reason, and a narrower-than-retained start lands in the "warn and
/// serve" branch the asymmetric policy describes instead of the fatal one.
/// Whatever comes back, `CAP_CHOWN` is not in it: the mask has no bit 0.
const fn capabilities_to_keep(permitted: u64) -> u32 {
    (permitted & retained_capability_mask() as u64) as u32
}

/// Proof that the capability narrowing has already run, on a thread that is an
/// ancestor of every thread the process goes on to create.
///
/// Linux capabilities and `PR_SET_NO_NEW_PRIVS` are per-*thread* attributes:
/// `capset(2)`/`prctl(2)` with `hdr.pid == 0` change the calling thread and
/// nothing else, and a thread that already exists keeps what it had for as
/// long as it lives. (This is why libcap ships `libpsx` to broadcast a
/// capability change across a process's threads.) Narrowing therefore has to
/// happen while the daemon is still single-threaded — before
/// `FaceEngine::load` brings up ONNX Runtime's intra-op pools and before
/// [`run`] builds the tokio runtime.
///
/// That ordering is not something [`run`] can verify for itself, and a
/// read-back cannot catch a mistake either: `capget(pid = 0)` inspects the one
/// thread that *did* drop, so the verification would be self-confirming. A
/// token only [`drop_capabilities_or_refuse`] can mint moves the requirement
/// to where the compiler checks it.
#[derive(Debug)]
pub struct CapabilitiesDropped(());

/// CAP_CHOWN's capability number.
///
/// Named because docs/security.md makes a promise about this one specifically:
/// the shipped unit puts CAP_CHOWN in the daemon's *bounding* set (never the
/// ambient set) for two startup-only chowns — `ensure_state_layout` and the
/// enrollment-marker reconcile (#137) — and the drop below is the whole of
/// what takes it away again before the first authentication.
const CAP_CHOWN: u32 = 0;

/// The capability sets a process actually holds, as read back from `capget`.
///
/// Each field is a mask over capability numbers 0-63: the two 32-bit words
/// `_LINUX_CAPABILITY_VERSION_3` uses, low word in the low half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct HeldCapabilities {
    effective: u64,
    permitted: u64,
    inheritable: u64,
}

/// Capabilities held beyond [`retained_capability_mask`], across all three sets.
///
/// Zero means the daemon holds exactly what it promised to hold — nothing
/// wider. Deliberately one-sided: it answers "is anything *extra* still here?"
/// and never "are the retained caps present?". A daemon started under a
/// narrower capability set than the shipped unit grants (an operator edit, an
/// unusual container) holds *fewer* caps than the mask, which costs
/// notifications and nothing else — it is not a security failure and must not
/// stop the daemon.
///
/// The ambient set needs no separate check: the kernel clears a capability
/// from ambient the moment it leaves permitted or inheritable, so an ambient
/// cap that survived is a permitted cap that survived.
///
/// Pure so the policy is testable without privilege or syscalls — the same
/// reason [`retained_capability_mask`] is a `const fn`.
const fn capabilities_beyond_retained(held: HeldCapabilities) -> u64 {
    let want = retained_capability_mask() as u64;
    (held.effective | held.permitted | held.inheritable) & !want
}

/// Render a capability mask for a log line: the hex `capsh --decode=` takes,
/// with CAP_CHOWN called out by name when present because that is the one
/// docs/security.md names.
fn describe_capability_mask(mask: u64) -> String {
    if mask & (1u64 << CAP_CHOWN) != 0 {
        format!("{mask:#018x} (includes CAP_CHOWN)")
    } else {
        format!("{mask:#018x}")
    }
}

// capget/capset use syscall numbers directly since libc doesn't expose the cap
// structs on all platforms. Shared by the drop and by the read-back that
// verifies it.
#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// `_LINUX_CAPABILITY_VERSION_3`. Two [`CapData`] words: caps 0-31, then 32-63.
const LINUX_CAP_V3: u32 = 0x2008_0522;

/// Drop every Linux capability except `keep`, and set PR_SET_NO_NEW_PRIVS.
///
/// By the time this runs the daemon has converged the state layout and the
/// enrollment markers — the only two things it ever needed `CAP_CHOWN` for —
/// so it no longer needs any elevated capability EXCEPT the two required to
/// drop privilege for desktop notifications (`runuser` →
/// `setgroups`/`setuid`). `keep` comes from [`capabilities_to_keep`] and is
/// therefore always a subset of what is currently permitted; it goes in the
/// effective, permitted, AND inheritable sets, and everything else is cleared.
///
/// **Per-thread.** `hdr.pid = 0` means the calling thread, so this narrows one
/// thread and every thread created from it afterwards — see
/// [`CapabilitiesDropped`] for why that constrains where it may be called.
///
/// Returns `Ok(())` on success. Callers must not treat an error as merely
/// advisory: [`drop_capabilities_or_refuse`] is the entry point, and it decides
/// what a failure costs by *reading back* what is actually held.
fn drop_capabilities(keep: u32) -> std::result::Result<(), String> {
    unsafe {
        // Prevent the process (and children) from ever gaining new privileges
        let ret = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        if ret != 0 {
            return Err(format!(
                "prctl(PR_SET_NO_NEW_PRIVS) failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        // Retain `keep` — normally CAP_SETUID + CAP_SETGID, the two the
        // runuser/su notification privilege-drop needs; clear every other
        // capability. The retained bits go in effective, permitted, AND
        // inheritable — the inheritable set is what lets systemd
        // AmbientCapabilities keep these caps across the exec into the
        // non-setuid `runuser` under NoNewPrivileges. V3 uses two CapData
        // structs (caps 0-31 and 32-63); the retained caps (6, 7) live in the
        // low word, so the high word stays fully zeroed — which is also what
        // clears anything above 31.
        let mut header = CapHeader {
            version: LINUX_CAP_V3,
            pid: 0,
        };
        let mut data = [
            CapData {
                effective: keep,
                permitted: keep,
                inheritable: keep,
            },
            CapData {
                effective: 0,
                permitted: 0,
                inheritable: 0,
            },
        ];
        let ret = libc::syscall(
            libc::SYS_capset,
            &mut header as *mut CapHeader,
            data.as_mut_ptr(),
        );
        if ret != 0 {
            return Err(format!(
                "capset syscall failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

/// Ask the kernel which capabilities this process actually holds.
///
/// `capget` is in `@system-service`, so the unit's seccomp allowlist permits
/// it (docs/security.md, Phase 3).
fn read_capabilities() -> std::result::Result<HeldCapabilities, String> {
    let mut header = CapHeader {
        version: LINUX_CAP_V3,
        pid: 0,
    };
    let mut data = [CapData::default(), CapData::default()];
    // SAFETY: `header` and `data` are correctly sized and aligned for
    // _LINUX_CAPABILITY_VERSION_3, which requires exactly two CapData words.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_capget,
            &mut header as *mut CapHeader,
            data.as_mut_ptr(),
        )
    };
    if ret != 0 {
        return Err(format!(
            "capget syscall failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let word = |lo: u32, hi: u32| u64::from(lo) | (u64::from(hi) << 32);
    Ok(HeldCapabilities {
        effective: word(data[0].effective, data[1].effective),
        permitted: word(data[0].permitted, data[1].permitted),
        inheritable: word(data[0].inheritable, data[1].inheritable),
    })
}

/// Drop capabilities, verify the drop actually happened, and refuse to serve if
/// it did not.
///
/// **Call this from the daemon's main thread, before anything spawns a
/// thread.** The narrowing is per-thread and is inherited only forwards; see
/// [`CapabilitiesDropped`], the token this returns, which [`run`] demands.
///
/// This used to log `failed to drop capabilities (continuing)` and carry on.
/// That was tolerable while the dropped set held nothing the security model had
/// promised to remove — the two retained caps were the whole story, and a
/// failed drop cost only the "narrowed after init" nicety. It is not tolerable
/// now: the shipped unit's bounding set includes **CAP_CHOWN** for two
/// startup-only chowns (#137), and docs/security.md tells the reader it is
/// "never held while authenticating anyone". A warning cannot keep that
/// promise — a failed drop would leave the daemon serving every authentication
/// with `chown(2)` in reach, announced only in the journal.
///
/// So the guarantee is *checked* rather than assumed: `capget` reports what the
/// kernel really left behind, and anything beyond
/// [`retained_capability_mask`] ends startup. Verifying beats trusting
/// `capset`'s return code — it also catches a mask that stopped being what this
/// code thinks it is.
///
/// The refusal is deliberately narrow, and asymmetric:
///
/// - **Extra capabilities held → fatal.** The daemon exits before the first
///   authentication. PAM degrades to the password, exactly as it does when the
///   daemon is not running at all, so this is never a lockout — the same
///   trade `state_layout::ensure_state_layout` already makes a few lines
///   earlier in startup, and the same "fail closed" convention as the model
///   SHA-256 check.
/// - **Drop failed but nothing extra is held → warn and continue.** Nothing
///   about the security model is violated, and refusing here would turn an
///   operator's hardening edit into a daemon that will not run.
/// - **The read-back itself failed → fatal.** An unverifiable guarantee is not
///   a guarantee.
///
/// A daemon started under a narrower capability set than the shipped unit
/// grants reaches none of those branches any more: [`capabilities_to_keep`]
/// asks only for capabilities the process already has, so the drop succeeds
/// and simply keeps less.
pub fn drop_capabilities_or_refuse() -> std::result::Result<CapabilitiesDropped, ServerError> {
    let refuse = |m: String| ServerError::Capabilities(m);

    // Read before dropping: the request has to be a subset of what is already
    // permitted or the kernel rejects it wholesale and nothing is dropped.
    let before = read_capabilities()
        .map_err(|e| refuse(format!("could not read the capabilities to drop: {e}")))?;
    let keep = capabilities_to_keep(before.permitted);
    let dropped = drop_capabilities(keep);

    let held = read_capabilities().map_err(|e| {
        refuse(format!(
            "could not verify that capabilities were dropped: {e}"
        ))
    })?;
    let extra = capabilities_beyond_retained(held);
    if extra != 0 {
        let why = match &dropped {
            Ok(()) => "the drop reported success".to_string(),
            Err(e) => format!("the drop failed: {e}"),
        };
        return Err(refuse(format!(
            "still holding capabilities that must be dropped before serving: {} ({why})",
            describe_capability_mask(extra)
        )));
    }

    match dropped {
        Ok(()) if keep == retained_capability_mask() => info!(
            "narrowed to CAP_SETUID+CAP_SETGID for the notification privilege-drop before any \
             other thread exists; verified all others dropped (including CAP_CHOWN) and set \
             no-new-privs"
        ),
        Ok(()) => warn!(
            "started under a narrower capability set than the shipped unit grants: kept {keep:#010x} \
             of the retained mask, so desktop notifications may not work. Everything else, \
             CAP_CHOWN included, is dropped and verified."
        ),
        Err(e) => warn!(
            "capability drop reported a failure, but nothing beyond the retained set is held — \
             desktop notifications may not work: {e}"
        ),
    }
    Ok(CapabilitiesDropped(()))
}

async fn run_dbus_server(
    handler: ProductionHandler,
    idle_timeout_secs: u64,
    startup_config_mtime: Option<std::time::SystemTime>,
    rebuild: Option<ProductionRebuild>,
    notifier_factory: NotifierFactory,
) -> Result<(), ServerError> {
    // Production builds the service through the same constructor the tests
    // use, so an invariant added to `new` cannot silently skip the only
    // instance that authenticates anyone. The struct literal this replaces
    // existed to keep the two handles below; cloning them back off the
    // service is what that cost.
    let service = FacelockService::new(handler, startup_config_mtime, rebuild, notifier_factory);
    let handler = service.handler.clone();
    let last_activity = service.last_activity.clone();
    let current_request = service.current_request();

    let _connection = zbus::connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service)?
        .build()
        .await?;

    info!("facelock daemon running on D-Bus system bus as {BUS_NAME}");

    // The capability narrowing does NOT happen here. It used to, on the
    // reasoning that a failure after the bus name is claimed is one clients
    // pay for — but by this line the process is long since multi-threaded
    // (ONNX Runtime's intra-op pools, then every tokio worker), and the
    // narrowing only ever reaches the calling thread and its descendants. The
    // threads that actually serve `Authenticate` would have kept CAP_CHOWN for
    // the daemon's whole life, and the `capget` read-back — also per-thread —
    // would have inspected the one thread that did drop and confirmed itself.
    //
    // So it moved to the top of `commands::daemon::run`, before anything
    // spawns a thread, and `run` demands the `CapabilitiesDropped` token as
    // proof. The bus-name argument gets simpler rather than weaker: refusing
    // before the name is claimed means no client ever sees a half-privileged
    // daemon at all.

    // Spawn a background task to release the camera on system suspend.
    // Best-effort: if logind is unavailable, log a warning and continue.
    let handler_for_sleep = handler.clone();
    let current_for_sleep = current_request.clone();
    tokio::spawn(async move {
        if let Err(e) = watch_sleep_signals(handler_for_sleep, current_for_sleep).await {
            tracing::warn!("failed to watch logind sleep signals: {e}");
        }
    });

    // Wait for shutdown signal (SIGTERM or SIGINT)
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received SIGINT, shutting down");
        }
        _ = sigterm.recv() => {
            info!("received SIGTERM, shutting down");
        }
        _ = poll_shutdown(handler, last_activity, idle_timeout_secs) => {
            info!("shutdown requested via D-Bus or idle timeout, shutting down");
        }
    }

    info!("goodbye");
    Ok(())
}

/// Watch for logind `PrepareForSleep` signals.
///
/// On suspend (arg=true), release the camera so V4L2 handles don't go stale.
/// On resume (arg=false), just log — the camera will be re-acquired on demand.
///
/// Manual testing:
/// ```bash
/// # Start daemon, then:
/// sudo systemctl suspend
/// # After resume, check: journalctl -u facelock-daemon --since "5 min ago"
/// ```
async fn watch_sleep_signals(
    handler: Arc<Mutex<ProductionHandler>>,
    current: CurrentRequest,
) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;

    let mut stream = proxy.receive_signal("PrepareForSleep").await?;
    info!("watching logind PrepareForSleep signals for camera suspend/resume");

    while let Some(signal) = stream.next().await {
        let suspending: bool = signal.body().deserialize().unwrap_or(false);
        if suspending {
            // Lock-free and first: a capture in flight is holding the handler
            // mutex, so its token is the only thing that reaches it. This
            // replaces the old single `try_lock` that gave up with a "handler
            // busy" warning and left the camera streaming into suspend.
            current.cancel();
            let handler = handler.clone();
            let _ = tokio::task::spawn_blocking(move || {
                // The cancelled request exits within one frame and drops the
                // lock; wait about that long for it rather than giving up
                // immediately (ADR 008 §8).
                let deadline = Instant::now() + SUSPEND_RELEASE_WAIT;
                loop {
                    match handler.try_lock() {
                        Ok(mut h) => {
                            h.handle(DaemonRequest::ReleaseCamera);
                            info!("released camera for suspend");
                            return;
                        }
                        Err(TryLockError::Poisoned(e)) => {
                            e.into_inner().handle(DaemonRequest::ReleaseCamera);
                            info!("released camera for suspend (recovered poisoned lock)");
                            return;
                        }
                        Err(TryLockError::WouldBlock) => {
                            if Instant::now() >= deadline {
                                warn!(
                                    "could not release camera for suspend within {SUSPEND_RELEASE_WAIT:?}: \
                                     handler still busy (the camera closes when the request returns)"
                                );
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                    }
                }
            })
            .await;
        } else {
            info!("resumed from suspend, camera will reacquire on demand");
        }
    }
    Ok(())
}

/// Poll the handler's shutdown_requested flag, idle camera release, and idle timeout.
/// All mutex access goes through spawn_blocking to avoid blocking the
/// tokio runtime (which would deadlock D-Bus method dispatch).
async fn poll_shutdown(
    handler: Arc<Mutex<ProductionHandler>>,
    last_activity: Arc<AtomicU64>,
    idle_timeout_secs: u64,
) {
    loop {
        tokio::time::sleep(CAMERA_POLL_INTERVAL).await;

        // Check idle timeout (0 = disabled)
        if idle_timeout_secs > 0 {
            let last = last_activity.load(Ordering::Relaxed);
            let now = now_secs();
            if now.saturating_sub(last) >= idle_timeout_secs {
                info!(
                    idle_secs = now.saturating_sub(last),
                    timeout = idle_timeout_secs,
                    "idle timeout reached, initiating shutdown"
                );
                return;
            }
        }

        let handler = handler.clone();
        let should_shutdown = tokio::task::spawn_blocking(move || {
            if let Ok(mut h) = handler.try_lock() {
                if h.shutdown_requested {
                    return true;
                }
                h.expire_camera(Instant::now());
            }
            false
        })
        .await
        .unwrap_or(false);

        if should_shutdown {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(uid: u32, username: Option<&str>) -> CallerIdentity {
        CallerIdentity {
            uid,
            username: username.map(str::to_string),
        }
    }

    #[test]
    fn bus_name_constants() {
        assert_eq!(BUS_NAME, "org.facelock.Daemon");
        assert_eq!(OBJECT_PATH, "/org/facelock/Daemon");
    }

    #[test]
    fn recoverable_auth_errors_are_encoded_in_band() {
        // Recoverable errors (rate limited, IR required, camera/storage
        // failures) must travel in the AuthResult wire format with
        // model_id == -2, not as D-Bus errors: a D-Bus error would make the
        // PAM client fall back to a fresh root oneshot attempt, silently
        // bypassing daemon-side state such as rate limiting.
        let result = recoverable_auth_error("rate limited".to_string());
        assert!(!result.matched);
        assert_eq!(result.model_id, -2);
        assert_eq!(result.label, "rate limited");
        assert_eq!(result.similarity, 0.0);
    }

    use facelock_core::config::{NotificationConfig, NotificationMode};
    use facelock_core::types::MatchResult;
    use facelock_test_support::RecordingNotifier;

    fn match_result(matched: bool) -> MatchResult {
        MatchResult {
            matched,
            model_id: matched.then_some(1),
            label: matched.then(|| "front".to_string()),
            similarity: 0.42,
            face_detected: true,
            failure_reason: None,
        }
    }

    fn desktop_config() -> NotificationConfig {
        NotificationConfig {
            mode: NotificationMode::Both,
            notify_prompt: true,
            notify_on_success: true,
            notify_on_failure: true,
        }
    }

    /// D9: a failed auth emits a Failure notification through the injected
    /// notifier when the config enables desktop failure notifications.
    #[test]
    fn failed_auth_emits_failure_notification_when_enabled() {
        let recorder = RecordingNotifier::new();
        notify_auth_outcome(&desktop_config(), &recorder, &match_result(false));
        assert_eq!(
            recorder.events(),
            vec![NotifyEvent::Failure {
                reason: "no match".into()
            }]
        );
    }

    /// D9: under the default config (terminal-only mode, and
    /// notify_on_failure = false) the same failed auth emits nothing.
    #[test]
    fn failed_auth_emits_nothing_under_default_config() {
        let recorder = RecordingNotifier::new();
        notify_auth_outcome(
            &NotificationConfig::default(),
            &recorder,
            &match_result(false),
        );
        assert_eq!(recorder.events(), vec![]);
    }

    /// A successful auth carries the label and similarity into the event.
    #[test]
    fn successful_auth_emits_success_event_with_match_data() {
        let recorder = RecordingNotifier::new();
        notify_auth_outcome(&desktop_config(), &recorder, &match_result(true));
        assert_eq!(
            recorder.events(),
            vec![NotifyEvent::Success {
                label: Some("front".into()),
                similarity: 0.42
            }]
        );
    }

    // --- Authorization matrix (N13) ---
    //
    // Authenticate is the only user-scoped method; everything else is
    // root-only. These tests iterate Method::ALL, which `declare_methods!`
    // generates from the same list as the variants — so a new method really
    // cannot be added without landing in the matrix.

    /// The wire method set and the authorization matrix must be the same
    /// set. `Method` is what [`authorize_method`] keys on, and zbus derives
    /// the wire name from the `#[interface]` function name — nothing in the
    /// type system ties the two together, so a method added to the interface
    /// without a `Method` variant would be a wire method the matrix tests
    /// above never see. Scanning the source is how the repo pins structural
    /// facts a type cannot (same idiom as the CLI's backend-seam pins); the
    /// live introspection XML is unavailable here because `#[interface]` is
    /// implemented only for the production `Camera`/`FaceEngine` handler.
    #[test]
    fn interface_methods_and_the_authz_matrix_are_the_same_set() {
        // Assembled at runtime so this literal doesn't match itself.
        let marker = format!("#[{}(name = \"org.facelock.Daemon\")]", "interface");
        let after_marker = include_str!("server.rs")
            .split_once(&marker)
            .expect("the #[interface] block")
            .1;
        // The impl ends at the first `}` in column 0; every brace inside it
        // is indented.
        let block = after_marker
            .split_once("\n}\n")
            .expect("the #[interface] block's closing brace")
            .0;

        let mut on_wire: Vec<String> = Vec::new();
        let mut previous = "";
        for line in block.lines() {
            let line = line.trim();
            // Signals are declared in the same block but are not methods.
            if let Some(rest) = line.strip_prefix("async fn ") {
                if !previous.contains("(signal)") {
                    on_wire.push(rest.split('(').next().unwrap().to_string());
                }
            }
            if !line.is_empty() {
                previous = line;
            }
        }

        let mut in_matrix: Vec<String> = Method::ALL.iter().map(|m| snake_case(m.name())).collect();
        on_wire.sort();
        in_matrix.sort();
        assert_eq!(
            on_wire, in_matrix,
            "every #[interface] method needs a Method variant (and vice versa) — \
             the authorization matrix is keyed on that enum"
        );
    }

    /// The wire name in the snake_case form zbus derives for the
    /// `#[interface]` function, which is what the scan above compares
    /// against.
    fn snake_case(name: &str) -> String {
        let mut out = String::with_capacity(name.len() + 3);
        for (i, ch) in name.char_indices() {
            if ch.is_ascii_uppercase() {
                if i > 0 {
                    out.push('_');
                }
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn authz_matrix_root_is_allowed_everywhere() {
        let root = caller(0, Some("root"));
        for method in Method::ALL.iter().copied() {
            assert!(
                authorize_method(&root, method, Some("alice")).is_ok(),
                "root must be allowed to call {method:?}"
            );
        }
    }

    #[test]
    fn authz_matrix_every_method_is_root_only_except_authenticate() {
        for method in Method::ALL.iter().copied() {
            let expected = if method == Method::Authenticate {
                Scope::UserScoped
            } else {
                Scope::Root
            };
            assert_eq!(method.scope(), expected, "{method:?}");
        }
    }

    #[test]
    fn authz_matrix_non_root_is_denied_every_root_scoped_method() {
        let alice = caller(1000, Some("alice"));
        for method in Method::ALL.iter().copied() {
            if method == Method::Authenticate {
                continue;
            }
            let err = authorize_method(&alice, method, Some("alice")).unwrap_err();
            assert!(
                matches!(err, fdo::Error::AccessDenied(_)),
                "{method:?} must deny a non-root caller, got: {err:?}"
            );
        }
    }

    #[test]
    fn oracle_and_metadata_methods_deny_non_root() {
        // The methods N13 retargeted, pinned by name: PreviewDetectFrame is
        // the continuous score feed (no pre_check, no rate limit); the rest
        // were group- or user-reachable metadata surfaces.
        let alice = caller(1000, Some("alice"));
        for method in [
            Method::PreviewDetectFrame,
            Method::ListModels,
            Method::ListDevices,
            Method::Ping,
            Method::ReleaseCamera,
        ] {
            let err = authorize_method(&alice, method, Some("alice")).unwrap_err();
            assert!(
                matches!(err, fdo::Error::AccessDenied(_)),
                "{method:?} must deny a non-root caller"
            );
        }
    }

    #[test]
    fn authenticate_allows_non_root_caller_for_themselves() {
        assert!(
            authorize_method(
                &caller(1000, Some("alice")),
                Method::Authenticate,
                Some("alice")
            )
            .is_ok()
        );
    }

    #[test]
    fn authenticate_denies_non_root_caller_for_another_user() {
        let err = authorize_method(
            &caller(1000, Some("alice")),
            Method::Authenticate,
            Some("bob"),
        )
        .unwrap_err();
        assert!(matches!(err, fdo::Error::AccessDenied(_)));
    }

    #[test]
    fn authenticate_fails_closed_for_unresolvable_caller_username() {
        // A non-root caller whose UID cannot be resolved to a username can
        // never match the target user.
        assert!(
            authorize_method(&caller(1000, None), Method::Authenticate, Some("alice")).is_err()
        );
    }

    #[test]
    fn user_scoped_method_without_target_user_fails_closed() {
        assert!(authorize_method(&caller(0, Some("root")), Method::Authenticate, None).is_err());
        assert!(
            authorize_method(&caller(1000, Some("alice")), Method::Authenticate, None).is_err()
        );
    }

    #[test]
    fn capture_slot_grants_when_free() {
        let slot = Arc::new(CaptureSlot::default());
        assert!(slot.try_acquire("Authenticate").is_ok());
    }

    #[test]
    fn capture_slot_rejects_concurrent_capture_immediately() {
        let slot = Arc::new(CaptureSlot::default());
        let _guard = slot.try_acquire("Authenticate").expect("first acquire");
        let err = slot.try_acquire("Authenticate").unwrap_err();
        // Busy must surface as a plain daemon error so PAM degrades to
        // password (never a lockout), and the message must say "busy".
        match err {
            fdo::Error::Failed(msg) => assert!(msg.contains("busy"), "message: {msg}"),
            other => panic!("expected fdo::Error::Failed, got {other:?}"),
        }
    }

    #[test]
    fn capture_slot_frees_on_guard_drop() {
        let slot = Arc::new(CaptureSlot::default());
        let guard = slot.try_acquire("Authenticate").expect("first acquire");
        drop(guard);
        assert!(
            slot.try_acquire("Authenticate").is_ok(),
            "slot must be reusable after the previous capture finishes"
        );
    }

    #[test]
    fn preview_jpeg_stripped_when_frames_not_allowed() {
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        assert!(sanitize_preview_jpeg(jpeg, false).is_empty());
    }

    #[test]
    fn preview_jpeg_kept_when_frames_allowed() {
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        assert_eq!(sanitize_preview_jpeg(jpeg.clone(), true), jpeg);
    }

    // --- ADR 008 §5: caller departure ---

    /// The event that means "cancel": our caller's name lost its owner.
    #[test]
    fn an_empty_new_owner_for_the_watched_name_is_a_departure() {
        assert!(caller_departed(":1.42", ":1.42", None));
        assert!(caller_departed(":1.42", ":1.42", Some("")));
    }

    /// The one thing a per-request watch must never do: cancel somebody
    /// else's request. A `NameOwnerChanged` for any other name is ignored
    /// even if it reaches this stream.
    #[test]
    fn a_departure_of_another_name_cancels_nothing() {
        assert!(!caller_departed(":1.42", ":1.43", None));
        assert!(!caller_departed(":1.42", "org.freedesktop.login1", None));
    }

    /// A well-known name changing hands is not our caller leaving.
    #[test]
    fn a_handover_to_a_new_owner_is_not_a_departure() {
        assert!(!caller_departed(":1.42", ":1.42", Some(":1.99")));
    }

    /// End to end over the decision the watch task runs, with the token it
    /// would set: only the registered sender's departure cancels.
    #[test]
    fn only_the_registered_senders_departure_sets_the_token() {
        let cancel = CancelToken::new();
        for (name, new_owner) in [(":1.43", None), (":1.42", Some(":1.99"))] {
            if caller_departed(":1.42", name, new_owner) {
                cancel.cancel();
            }
        }
        assert!(!cancel.is_cancelled(), "an unrelated event cancelled us");

        if caller_departed(":1.42", ":1.42", None) {
            cancel.cancel();
        }
        assert!(cancel.is_cancelled());
    }

    /// Each caller's watch is subscribed against that caller's own token, so
    /// B leaving cannot end A's authentication. This is the shape of the
    /// glue, replayed: two requests, two tokens, one departure.
    #[test]
    fn one_callers_departure_never_cancels_anothers_request() {
        let alice = CancelToken::new();
        let bob = CancelToken::new();

        // Bob's watch fires. It holds only Bob's token.
        if caller_departed(":1.43", ":1.43", None) {
            bob.cancel();
        }

        assert!(bob.is_cancelled(), "Bob's own request must end");
        assert!(
            !alice.is_cancelled(),
            "Bob's departure reached Alice's in-flight authentication"
        );
    }

    // --- ADR 008 §5: the in-flight request slot ---

    /// Nothing running, nothing to stop. `ReleaseCamera` on an idle daemon
    /// must not leave a set flag lying around for the next request to find.
    #[test]
    fn cancelling_an_empty_slot_is_a_no_op() {
        let current = CurrentRequest::default();
        current.cancel();
        let next = CancelToken::new();
        let _guard = current.install(next.clone());
        assert!(
            !next.is_cancelled(),
            "a cancel with nothing in flight reached the next request"
        );
    }

    /// What suspend, `ReleaseCamera` and shutdown do: reach exactly the
    /// request that is running.
    #[test]
    fn cancel_reaches_the_installed_request() {
        let current = CurrentRequest::default();
        let in_flight = CancelToken::new();
        let _guard = current.install(in_flight.clone());
        current.cancel();
        assert!(in_flight.is_cancelled());
    }

    /// The generation check. Requests overlap at the edges — one is still
    /// finishing while the next has claimed the capture slot — so the older
    /// guard's drop must not clear the newer entry, or the next cancellation
    /// would find the slot empty and stop nothing.
    #[test]
    fn a_finishing_request_never_clears_its_successors_slot() {
        let current = CurrentRequest::default();
        let first = CancelToken::new();
        let second = CancelToken::new();

        let first_guard = current.install(first.clone());
        let _second_guard = current.install(second.clone());
        drop(first_guard);

        current.cancel();
        assert!(
            second.is_cancelled(),
            "the request in flight was not reached"
        );
        assert!(
            !first.is_cancelled(),
            "a cancellation reached a request that had already finished"
        );
    }

    /// And the converse: once a request's guard has dropped with the slot
    /// still its own, the slot is empty — a stale token can never be
    /// cancelled in place of the next request's.
    #[test]
    fn a_finished_requests_token_is_not_left_in_the_slot() {
        let current = CurrentRequest::default();
        let finished = CancelToken::new();
        drop(current.install(finished.clone()));

        current.cancel();
        assert!(
            !finished.is_cancelled(),
            "a finished request stayed cancellable"
        );
    }

    #[test]
    fn retained_capability_mask_is_exactly_setuid_and_setgid() {
        // Cap bit numbers per <linux/capability.h>.
        const CAP_SETGID: u32 = 6;
        const CAP_SETUID: u32 = 7;
        const CAP_DAC_OVERRIDE: u32 = 1;
        const CAP_NET_RAW: u32 = 13;
        const CAP_SYS_ADMIN: u32 = 21;

        let mask = retained_capability_mask();

        // Exactly the two caps required for the runuser/su notification
        // privilege-drop are retained.
        assert_eq!(mask, (1 << CAP_SETUID) | (1 << CAP_SETGID));
        assert_eq!(mask, 0b1100_0000);

        // The two we want are present.
        assert_ne!(mask & (1 << CAP_SETUID), 0, "CAP_SETUID must be retained");
        assert_ne!(mask & (1 << CAP_SETGID), 0, "CAP_SETGID must be retained");

        // Dangerous caps are NOT retained.
        assert_eq!(
            mask & (1 << CAP_SYS_ADMIN),
            0,
            "CAP_SYS_ADMIN must be dropped"
        );
        assert_eq!(mask & (1 << CAP_NET_RAW), 0, "CAP_NET_RAW must be dropped");
        assert_eq!(
            mask & (1 << CAP_DAC_OVERRIDE),
            0,
            "CAP_DAC_OVERRIDE must be dropped"
        );

        // Exactly two bits set, and none in the high word (caps 32-63).
        assert_eq!(mask.count_ones(), 2);
    }

    // -----------------------------------------------------------------------
    // The capability drop is verified, not assumed (#137)
    // -----------------------------------------------------------------------

    /// Every set holding exactly the retained mask is the intended steady
    /// state: nothing extra, so nothing to refuse.
    #[test]
    fn a_clean_drop_leaves_nothing_beyond_the_retained_set() {
        let want = u64::from(retained_capability_mask());
        let held = HeldCapabilities {
            effective: want,
            permitted: want,
            inheritable: want,
        };
        assert_eq!(capabilities_beyond_retained(held), 0);
    }

    /// The deliberate asymmetry: holding *fewer* caps than the mask is not a
    /// violation. A daemon started under a narrower bounding set than the
    /// shipped unit grants (an operator edit, an unusual container) keeps less
    /// than the retained mask — that costs desktop notifications and nothing
    /// the security model promised, and must not stop it from serving.
    #[test]
    fn holding_fewer_caps_than_retained_is_not_a_violation() {
        // CAP_SETUID only: CAP_SETGID was never in the bounding set.
        let held = HeldCapabilities {
            effective: 1 << 7,
            permitted: 1 << 7,
            inheritable: 0,
        };
        assert_eq!(capabilities_beyond_retained(held), 0);
    }

    /// The realistic narrower shape, which is *not* a subset of the retained
    /// mask and so is not covered by the test above: an operator who wants no
    /// desktop notifications writes a drop-in with
    /// `CapabilityBoundingSet=CAP_CHOWN` and `AmbientCapabilities=`, and the
    /// daemon starts with `permitted == {CAP_CHOWN}` — narrower than the mask
    /// in one direction and wider in the other.
    ///
    /// Requesting the retained mask absolutely is `EPERM` here (the kernel
    /// requires the new permitted set to be a subset of the old), and `capset`
    /// rejects wholesale, so *nothing* would be dropped: CAP_CHOWN survives,
    /// the read-back calls it a violation, and `Restart=on-failure` flaps the
    /// daemon every `RestartSec` forever over a legitimate configuration.
    /// Intersecting first asks for nothing the process lacks.
    #[test]
    fn a_bounding_set_of_cap_chown_alone_narrows_to_nothing() {
        let permitted = 1u64 << CAP_CHOWN;

        // Pre-drop, this state *is* a violation — which is what makes the
        // wholesale-rejection failure mode fatal rather than merely untidy.
        let before = HeldCapabilities {
            effective: permitted,
            permitted,
            inheritable: 0,
        };
        assert_eq!(capabilities_beyond_retained(before), 1 << CAP_CHOWN);

        // The drop asks for nothing, so it cannot be refused, and clears
        // CAP_CHOWN on the way.
        let keep = capabilities_to_keep(permitted);
        assert_eq!(keep, 0, "must not request a capability the process lacks");

        let after = HeldCapabilities {
            effective: u64::from(keep),
            permitted: u64::from(keep),
            inheritable: u64::from(keep),
        };
        assert_eq!(capabilities_beyond_retained(after), 0);
    }

    /// `capset` requires the new permitted set to be a subset of the old one.
    /// The mask handed to it must satisfy that for *every* starting set, not
    /// just the shipped one — this is the property that keeps the drop from
    /// failing wholesale and dropping nothing.
    #[test]
    fn the_kept_mask_is_always_a_subset_of_what_is_already_permitted() {
        for permitted in [
            0,
            1 << CAP_CHOWN,
            u64::from(retained_capability_mask()),
            u64::from(retained_capability_mask()) | (1 << CAP_CHOWN), // the shipped unit
            1 << 7,                                                   // CAP_SETUID alone
            u64::MAX,                                                 // unconfined root
        ] {
            let keep = u64::from(capabilities_to_keep(permitted));
            assert_eq!(
                keep & !permitted,
                0,
                "kept {keep:#x} is not a subset of permitted {permitted:#x}"
            );
            assert_eq!(
                keep & !u64::from(retained_capability_mask()),
                0,
                "kept {keep:#x} reaches beyond the retained mask"
            );
            assert_eq!(
                keep & (1 << CAP_CHOWN),
                0,
                "CAP_CHOWN must never be kept, whatever the process started with"
            );
        }
    }

    /// The shipped configuration is unchanged by the intersection: bounding
    /// `CAP_SETUID CAP_SETGID CAP_CHOWN` with the first two ambient still keeps
    /// exactly the two the notification privilege-drop needs.
    #[test]
    fn the_shipped_unit_still_keeps_both_notification_caps() {
        let permitted = u64::from(retained_capability_mask()) | (1 << CAP_CHOWN);
        assert_eq!(capabilities_to_keep(permitted), retained_capability_mask());
    }

    /// **The load-bearing test.** CAP_CHOWN surviving in *any* of the three
    /// sets is the failure docs/security.md's "never held while authenticating
    /// anyone" claim is about. Checked per-set because they are not
    /// interchangeable: `permitted` is what `chown(2)` needs here, and
    /// `inheritable` is what an exec'd child could carry away.
    #[test]
    fn cap_chown_surviving_in_any_set_is_refused() {
        let want = u64::from(retained_capability_mask());
        let chown = 1u64 << CAP_CHOWN;
        for (label, held) in [
            (
                "effective",
                HeldCapabilities {
                    effective: want | chown,
                    permitted: want,
                    inheritable: want,
                },
            ),
            (
                "permitted",
                HeldCapabilities {
                    effective: want,
                    permitted: want | chown,
                    inheritable: want,
                },
            ),
            (
                "inheritable",
                HeldCapabilities {
                    effective: want,
                    permitted: want,
                    inheritable: want | chown,
                },
            ),
        ] {
            assert_eq!(
                capabilities_beyond_retained(held),
                chown,
                "CAP_CHOWN left in the {label} set was not detected"
            );
        }
    }

    /// A failed drop leaves *everything* — the case the old
    /// `warn!("failed to drop capabilities (continuing)")` waved through. Full
    /// root under the shipped bounding set is CAP_SETUID + CAP_SETGID +
    /// CAP_CHOWN, so the extra is exactly the capability the unit added.
    #[test]
    fn a_drop_that_did_not_happen_is_refused() {
        let bounding = u64::from(retained_capability_mask()) | (1 << CAP_CHOWN);
        let held = HeldCapabilities {
            effective: bounding,
            permitted: bounding,
            inheritable: bounding,
        };
        assert_eq!(capabilities_beyond_retained(held), 1 << CAP_CHOWN);
    }

    /// Capabilities above 31 live in the second `capget` word. Reading only
    /// the low word would silently ignore half the capability space.
    #[test]
    fn capabilities_in_the_high_word_are_not_missed() {
        // CAP_CHECKPOINT_RESTORE = 40.
        let high = 1u64 << 40;
        let held = HeldCapabilities {
            effective: 0,
            permitted: high,
            inheritable: 0,
        };
        assert_eq!(capabilities_beyond_retained(held), high);
    }

    /// The refusal has to be readable in a journal at 3am: a hex mask
    /// `capsh --decode=` accepts, and CAP_CHOWN spelled out because that is
    /// the capability the docs make a promise about.
    #[test]
    fn the_refusal_names_cap_chown() {
        let described = describe_capability_mask(1 << CAP_CHOWN);
        assert!(described.contains("CAP_CHOWN"), "got {described}");
        assert!(described.contains("0x"), "got {described}");
        assert!(
            !describe_capability_mask(1 << 21).contains("CAP_CHOWN"),
            "CAP_SYS_ADMIN must not be reported as CAP_CHOWN"
        );
    }

    /// Smoke test for the `capget` wiring itself — the struct layout and the
    /// two-word V3 read are the parts a unit test on masks cannot cover. Runs
    /// unprivileged: every process can read its own capability sets, and the
    /// kernel guarantees effective is a subset of permitted whatever they are.
    #[test]
    fn this_process_can_read_back_its_own_capabilities() {
        let held = read_capabilities().expect("capget on self must succeed");
        assert_eq!(
            held.effective & !held.permitted,
            0,
            "effective must be a subset of permitted; got {held:?}"
        );
    }

    /// This thread's `NoNewPrivs` bit, straight from `/proc`. `None` when
    /// `/proc` is not mounted or the field is absent.
    ///
    /// `/proc/thread-self` is the calling *thread*, not the process — which is
    /// the whole point of the test below, and the same distinction that makes
    /// `/proc/<pid>/status` (the main thread) the wrong thing to assert on a
    /// running daemon.
    fn no_new_privs() -> Option<bool> {
        let status = std::fs::read_to_string("/proc/thread-self/status").ok()?;
        let value = status.lines().find_map(|l| l.strip_prefix("NoNewPrivs:"))?;
        Some(value.trim() == "1")
    }

    fn set_no_new_privs() -> std::io::Result<()> {
        // SAFETY: PR_SET_NO_NEW_PRIVS takes no pointer arguments, and setting
        // it is unprivileged and always permitted.
        let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// **The regression pin for the ordering.** Narrowing a process is a
    /// per-thread operation inherited only forwards, so *when* it happens
    /// decides *which* threads it reaches. This runs both orders against a
    /// real `tokio` multi-threaded runtime and shows the difference.
    ///
    /// Demonstrated with `PR_SET_NO_NEW_PRIVS` rather than `capset` because
    /// the two travel by the same mechanism — a `task_struct` field copied at
    /// `clone(2)` and never broadcast to siblings — and this is the half an
    /// unprivileged test can actually set. `/proc/<tid>/status` reports it per
    /// thread, exactly as it reports `CapPrm`, which is what
    /// `test/pkg-validate.sh` walks across `/proc/<pid>/task/*` on the running
    /// daemon.
    ///
    /// The whole experiment runs on a thread this test spawns: the bit is
    /// irreversible, so it must land somewhere the test owns rather than on a
    /// harness thread.
    #[test]
    fn only_threads_created_after_the_narrowing_inherit_it() {
        std::thread::spawn(|| {
            if no_new_privs() != Some(false) {
                // No /proc, or something already set it for this process tree.
                // Nothing left to demonstrate either way.
                return;
            }

            let build = || {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("multi-threaded runtime")
            };
            let worker_bit = |rt: &tokio::runtime::Runtime| {
                rt.block_on(async { tokio::spawn(async { no_new_privs() }).await.unwrap() })
            };

            // A runtime built BEFORE the narrowing: the order this code used
            // to have, where the drop sat after the bus name was claimed.
            let early = build();
            // Force its workers into existence now, as production does long
            // before that old drop site (models loaded, bus connected).
            early.block_on(async { tokio::spawn(async {}).await.unwrap() });

            set_no_new_privs().expect("PR_SET_NO_NEW_PRIVS is unprivileged");
            assert_eq!(no_new_privs(), Some(true), "the calling thread narrows");

            // A runtime built AFTER it: the order `commands::daemon::run` now
            // guarantees by demanding a `CapabilitiesDropped`.
            let late = build();

            assert_eq!(
                worker_bit(&early),
                Some(false),
                "a worker that existed before the narrowing kept the wider credentials. \
                 This is the defect: narrowing once the runtime is up reaches none of the \
                 threads that serve Authenticate, and a per-thread read-back cannot see it."
            );
            assert_eq!(
                worker_bit(&late),
                Some(true),
                "a worker created after the narrowing must inherit it"
            );
        })
        .join()
        .expect("experiment thread");
    }
}
