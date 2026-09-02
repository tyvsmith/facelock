//! Backend selection and the transport seam (D1).
//!
//! Every user-facing operation is answered either by the daemon over D-Bus or
//! by direct (in-process) store/camera access. That used to be a boolean
//! (`should_use_direct`) consulted independently at 13 call sites, each with
//! its own failure policy and its own fresh bus probe — real TOCTOU windows
//! (`clear` probed, prompted for an unbounded time, then probed again) and an
//! activation asymmetry (`name_has_owner` does not start the daemon, but a
//! `send_request` on the "wrong" branch would, silently flipping a later
//! operation from direct to daemon mode — issue #89 validation fallout).
//!
//! Now selection is made **once per invocation**, at the top of each
//! backend-using command, with **at most one** deliberate, non-activating bus
//! probe. The outcome is a [`BackendKind`], not a boolean, because "direct"
//! means three different things that deserve different messages and log levels;
//! and every operation that used to fork per call site forks in exactly one
//! method here.
//!
//! What stays outside this seam, on the merits:
//! - **PAM** performs one operation and needs things the CLI must not have
//!   (pinned-peer verification, a hard deadline, an env-cleared subprocess
//!   fallback). It shares test vectors, not code.
//! - **`is-enrolled`** is dispatched in `main` before any config parse and
//!   must never probe the bus; `hyprlock` and `config` touch no backend.
//! - **`status`** consumes facts, not transport: its daemon probe is
//!   [`probe_daemon`] here in the seam (H8), so `ipc_client` stays
//!   single-sited.
//! - Maintenance commands (`bench`, `tpm`, `audit`, `auth`, `daemon`) are
//!   direct-by-nature and never select.

use anyhow::bail;
use zbus::blocking::Connection;

use facelock_core::Config;
use facelock_core::config::DaemonMode;
use facelock_core::dbus_interface::BUS_NAME;
use facelock_core::ipc::IpcDeviceInfo;
use facelock_core::types::{FaceModelInfo, MatchResult};
use facelock_daemon::auth::AuthOutcome;
use facelock_store::StoreError;

use crate::message::{AccessMessage, Terminal};
use crate::resolved::Fact;
use crate::{direct, ipc_client};

/// Which implementation answers this invocation's questions — and, for the
/// direct answers, *why* it is direct. The distinction carries the failure
/// policy: an expected configuration is not a degraded state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// `daemon.mode = "daemon"` and the bus name has an owner.
    Daemon,
    /// `daemon.mode = "oneshot"` — direct access is the configuration.
    /// Expected; no warning.
    DirectByConfig,
    /// `daemon.mode = "daemon"` but the bus name has no owner — degraded.
    /// WARNed once, at selection.
    DirectByFallback,
    /// `daemon.mode = "daemon"` under a non-default `--config` (#314). The
    /// packaged unit runs bare `facelock daemon` and reads only the default
    /// file, so whatever owns the bus is configured by a file this process is
    /// not reading: its store, camera, models and security policy cannot be
    /// proven to match. Expected, told once at INFO; the bus is never asked.
    DirectByOverride,
}

impl BackendKind {
    pub fn is_direct(self) -> bool {
        match self {
            BackendKind::Daemon => false,
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => true,
        }
    }
}

/// What the one probe observed, recorded as a [`Fact`] beside the config
/// facts in `resolved` (D7) and with the same discipline: `NotProbed` is a
/// distinct value, never a guessed `Unreachable`. `status` reports it
/// through [`DaemonPing`], which additionally carries the failure detail a
/// report wants and a human does not need at selection time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonReachability {
    /// `mode = "oneshot"`, or a non-default `--config`: the bus is
    /// deliberately never asked.
    NotProbed,
    Reachable,
    Unreachable,
}

/// What a backend cannot do, stated once at selection instead of discovered
/// at the failure site.
///
/// Deliberately not carried here: `facelock test`'s remaining posture
/// asymmetries. Both transports now run the same `pre_check` gates (N11), so
/// the old `enforces_auth_gates` flag would state a falsehood; what still
/// differs — the direct path skips the SSH/lid *physical-presence* aborts via
/// `PreCheckContext::test` and stamps `AuditSource::Test` instead of
/// `AuditSource::Daemon` — is documented at [`Backend::recognize`], because no
/// caller branches on it.
pub struct BackendCaps {
    /// Streaming JPEG preview frames come from the daemon; a direct backend
    /// has only the local text preview.
    pub graphical_preview: bool,
    /// Per-device format/resolution detail is a V4L2 interrogation; the D-Bus
    /// `DeviceInfo` type does not carry it.
    pub device_formats: bool,
}

/// The transport seam. Holds the selection made by [`Backend::select`]; every
/// method forks on it in exactly one place, with one failure policy per
/// operation: daemon replies and store failures propagate — they never fold
/// into "no models" (C4) — and on direct reads a provably absent database is
/// a benign answer that must not materialize one (C7).
pub struct Backend<'a> {
    kind: BackendKind,
    config: &'a Config,
}

impl<'a> Backend<'a> {
    /// Select the backend for this invocation. Called once, at the top of
    /// each backend-using command; probes the bus at most once (never for
    /// `mode = "oneshot"`), non-activating by construction.
    pub fn select(config: &'a Config) -> Backend<'a> {
        let config_override = facelock_core::paths::non_default_config_override();
        let (kind, reachability) = classify(
            &config.daemon.mode,
            config_override.is_some(),
            daemon_bus_reachable,
        );
        let (level, notice) = announcement(kind);
        // The one machine-facing record of the selection and the probed
        // reachability fact. `tracing::event!` needs a const level, hence the
        // match.
        match level {
            tracing::Level::WARN => tracing::warn!(
                target: "facelock::backend",
                ?kind,
                ?reachability,
                ?config_override,
                "backend selected"
            ),
            tracing::Level::INFO => tracing::info!(
                target: "facelock::backend",
                ?kind,
                ?reachability,
                ?config_override,
                "backend selected"
            ),
            _ => tracing::debug!(
                target: "facelock::backend",
                ?kind,
                ?reachability,
                ?config_override,
                "backend selected"
            ),
        }
        if let Some(msg) = notice {
            Terminal.error(&msg);
        }
        Backend { kind, config }
    }

    pub fn kind(&self) -> BackendKind {
        self.kind
    }

    pub fn caps(&self) -> BackendCaps {
        match self.kind {
            BackendKind::Daemon => BackendCaps {
                graphical_preview: true,
                device_formats: false,
            },
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => BackendCaps {
                graphical_preview: false,
                device_formats: true,
            },
        }
    }

    /// Does `user` have any enrolled models?
    pub fn has_models(&self, user: &str) -> anyhow::Result<bool> {
        match self.kind {
            BackendKind::Daemon => Ok(!self.daemon_list_models(user)?.is_empty()),
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => self.direct_read(false, |store| {
                store
                    .has_models(user)
                    .map_err(|e| anyhow::anyhow!("storage error: {e}"))
            }),
        }
    }

    /// Does `user` have models produced by `embedder`?
    pub fn has_models_for_embedder(&self, user: &str, embedder: &str) -> anyhow::Result<bool> {
        match self.kind {
            BackendKind::Daemon => Ok(self
                .daemon_list_models(user)?
                .iter()
                .any(|model| model.embedder_model == embedder)),
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => self.direct_read(false, |store| {
                store
                    .has_models_for_embedder(user, embedder)
                    .map_err(|e| anyhow::anyhow!("storage error: {e}"))
            }),
        }
    }

    /// All of `user`'s models.
    pub fn list_models(&self, user: &str) -> anyhow::Result<Vec<FaceModelInfo>> {
        match self.kind {
            BackendKind::Daemon => self.daemon_list_models(user),
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => self.direct_read(Vec::new(), |store| {
                store
                    .list_models(user)
                    .map_err(|e| anyhow::anyhow!("storage error: {e}"))
            }),
        }
    }

    /// Remove one model. `Some(removed)` when this backend can tell whether
    /// the model existed; `None` on the daemon path, whose wire reply does not
    /// carry it (a P1-3 wire change deferred to keep this PR wire-stable).
    pub fn remove_model(&self, user: &str, model_id: u32) -> anyhow::Result<Option<bool>> {
        match self.kind {
            BackendKind::Daemon => {
                ipc_client::remove_model(user, model_id)?;
                Ok(None)
            }
            // An absent store provably holds nothing to remove: report "not
            // found" without materializing a database (the per-site version
            // used the creating opener here).
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => self.direct_read(Some(false), |store| {
                store
                    .remove_model(user, model_id)
                    .map(Some)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }),
        }
    }

    /// Remove all of `user`'s models. `Some(count)` when this backend can
    /// count them; `None` on the daemon path (same wire limitation as
    /// [`Backend::remove_model`]).
    pub fn clear_models(&self, user: &str) -> anyhow::Result<Option<usize>> {
        match self.kind {
            BackendKind::Daemon => {
                ipc_client::clear_models(user)?;
                Ok(None)
            }
            // An absent store provably holds nothing to clear, so the honest
            // answer is "cleared 0" — same as [`Backend::remove_model`]'s
            // `Some(false)` for the same state, and without materializing the
            // database (C7). This used to error on the grounds that callers
            // check `has_models` first, which made the answer depend on call
            // ordering the type could not express.
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => self.direct_read(Some(0), |store| {
                store
                    .clear_user(user)
                    .map(|count| Some(count as usize))
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }),
        }
    }

    /// Enroll a face for `user`, returning `(model_id, embedding_count)`.
    /// The daemon call runs on a dedicated connection whose timeout is
    /// derived from the daemon's own enrollment deadline (issue #89).
    pub fn enroll(&self, user: &str, label: &str) -> anyhow::Result<(u32, u32)> {
        match self.kind {
            BackendKind::Daemon => ipc_client::send_enroll(user, label, self.config),
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => direct::enroll(self.config, user, label),
        }
    }

    /// Recognition only — backs `facelock test` (root-only, N11). NEVER the
    /// PAM/auth path: the daemon transport calls the root-only
    /// `TestAuthenticate` method, not `Authenticate`.
    ///
    /// The two transports now have the same posture: both run every
    /// `pre_check` gate except the SSH/lid physical-presence aborts
    /// (`PreCheckContext::test` — they exist to stop an attacker's shortcuts,
    /// not a root operator diagnosing over SSH), both audit as `Test`, and
    /// neither charges the rate limit for a failed test — the direct path
    /// because it never records failures, the daemon path because
    /// `TestAuthenticate` is the entry point that does not.
    pub fn recognize(&self, user: &str) -> anyhow::Result<MatchResult> {
        match self.kind {
            BackendKind::Daemon => match ipc_client::test_authenticate(user)? {
                AuthOutcome::AuthResult(result) => Ok(result),
                // `suppress_unknown` short-circuit: same "no result"
                // shape the direct path reports.
                AuthOutcome::Suppressed => Ok(MatchResult {
                    matched: false,
                    model_id: None,
                    label: None,
                    similarity: 0.0,
                    face_detected: false,
                    failure_reason: None,
                }),
                AuthOutcome::Error { message, .. } => bail!("daemon error: {message}"),
                AuthOutcome::Cancelled => bail!("authentication cancelled"),
            },
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => direct::authenticate(self.config, user),
        }
    }

    /// Enumerate camera devices. Formats are present only when
    /// [`BackendCaps::device_formats`] says so.
    pub fn list_devices(&self) -> anyhow::Result<Vec<IpcDeviceInfo>> {
        match self.kind {
            BackendKind::Daemon => {
                // Only blame a missing daemon when the request wasn't
                // refused: an AccessDenied already carries its own hint as
                // the headline.
                ipc_client::list_devices().map_err(|e| {
                    if ipc_client::is_access_denied(&e) {
                        e
                    } else {
                        e.context("failed to query daemon — is facelock-daemon running?")
                    }
                })
            }
            BackendKind::DirectByConfig
            | BackendKind::DirectByFallback
            | BackendKind::DirectByOverride => direct::list_devices_info(),
        }
    }

    /// The daemon's `ListModels`, typed. One failure policy (C4): transport
    /// failures — including the daemon's own error reply, which arrives as a
    /// D-Bus error — propagate; they must never read as "no models enrolled".
    fn daemon_list_models(&self, user: &str) -> anyhow::Result<Vec<FaceModelInfo>> {
        ipc_client::list_models(user)
    }

    /// Direct-transport read with the C7 discrimination carried by
    /// [`StoreError`]: [`StoreError::Absent`] (fresh install) answers
    /// `absent_value` without creating the database the probe would otherwise
    /// materialize; every other failure class propagates.
    fn direct_read<T>(
        &self,
        absent_value: T,
        read: impl FnOnce(&facelock_store::FaceStore) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let store = match direct::open_store_existing(self.config) {
            Ok(store) => store,
            Err(StoreError::Absent { .. }) => return Ok(absent_value),
            Err(e) => return Err(e.into()),
        };
        read(&store)
    }
}

/// What `status`'s deliberate daemon probe found (H8).
///
/// Distinct from [`DaemonReachability`] on purpose. Selection's probe
/// (`name_has_owner`) must never activate the daemon, because selection runs
/// on every backend-using command. `status` exists to diagnose, so its probe
/// is the full `Ping` round-trip — allowed to trigger D-Bus activation,
/// because "responding" in a health report means *the daemon the next auth
/// would get answered a method call*, not merely that the name has an owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonPing {
    /// `daemon.mode = "oneshot"`: the bus is deliberately never asked; there
    /// is no daemon to report on.
    NotConfigured,
    /// A `Ping` method call completed.
    Responding,
    /// The round-trip failed; `error` is the transport's own account.
    NotResponding { error: String },
}

/// `status`'s daemon probe. Lives in the seam so `ipc_client::ping` stays
/// single-sited (D1) — the health model consumes the fact, not the transport.
pub fn probe_daemon(mode: &DaemonMode) -> Fact<DaemonPing> {
    probe_daemon_with(mode, ipc_client::ping)
}

/// The probe rule, split from the transport so it is testable without a bus
/// (same shape as [`classify`]).
fn probe_daemon_with(
    mode: &DaemonMode,
    ping: impl FnOnce() -> anyhow::Result<()>,
) -> Fact<DaemonPing> {
    match mode {
        DaemonMode::Oneshot => Fact::claimed(DaemonPing::NotConfigured),
        DaemonMode::Daemon => Fact::probed(match ping() {
            Ok(()) => DaemonPing::Responding,
            Err(e) => DaemonPing::NotResponding {
                error: e.to_string(),
            },
        }),
    }
}

/// The selection rule, split from the probe so it is testable without a bus.
///
/// `config_override` is [`facelock_core::paths::non_default_config_override`]'s
/// verdict. It outranks the bus in daemon mode: a daemon that answers is
/// reading the default file, and an answer from it would be an answer about
/// a configuration this invocation did not select. The bus is not probed on
/// that path, so no reachability fact is claimed that was never observed.
fn classify(
    mode: &DaemonMode,
    config_override: bool,
    probe: impl FnOnce() -> bool,
) -> (BackendKind, Fact<DaemonReachability>) {
    if config_override && matches!(mode, DaemonMode::Daemon) {
        return (
            BackendKind::DirectByOverride,
            Fact::claimed(DaemonReachability::NotProbed),
        );
    }
    match mode {
        DaemonMode::Oneshot => (
            BackendKind::DirectByConfig,
            Fact::claimed(DaemonReachability::NotProbed),
        ),
        DaemonMode::Daemon => {
            if probe() {
                (
                    BackendKind::Daemon,
                    Fact::probed(DaemonReachability::Reachable),
                )
            } else {
                (
                    BackendKind::DirectByFallback,
                    Fact::probed(DaemonReachability::Unreachable),
                )
            }
        }
    }
}

/// What selection says, per kind: the log level for the machine line, and
/// the user-facing stderr message, if any. Four kinds, three treatments —
/// an expected configuration is silent, a degraded fallback warns once, and
/// an override is explained once without a warning, because the operator
/// chose it and the daemon it bypasses is not at fault.
fn announcement(kind: BackendKind) -> (tracing::Level, Option<AccessMessage>) {
    match kind {
        BackendKind::Daemon => (tracing::Level::DEBUG, None),
        BackendKind::DirectByConfig => (tracing::Level::DEBUG, None),
        BackendKind::DirectByFallback => (
            tracing::Level::WARN,
            Some(AccessMessage::DaemonUnreachableFallback),
        ),
        BackendKind::DirectByOverride => (
            tracing::Level::INFO,
            Some(AccessMessage::DirectByConfigOverride),
        ),
    }
}

/// The one deliberate bus probe. `name_has_owner` asks the bus about the
/// daemon's name without triggering D-Bus activation — the asymmetry that
/// made per-site probing hazardous (`send_request` on a freshly booted
/// system would *start* the daemon). Any failure to ask reads as
/// unreachable.
fn daemon_bus_reachable() -> bool {
    let Ok(conn) = Connection::system() else {
        return false;
    };
    let Ok(proxy) = zbus::blocking::fdo::DBusProxy::new(&conn) else {
        return false;
    };
    let Ok(name) = BUS_NAME.try_into() else {
        return false;
    };
    proxy.name_has_owner(name).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // -----------------------------------------------------------------------
    // Selection: config mode x reachability -> kind, message, log level.
    // The probe is a closure, so no test touches a bus.
    // -----------------------------------------------------------------------

    #[test]
    fn oneshot_config_selects_direct_without_probing() {
        let (kind, fact) = classify(&DaemonMode::Oneshot, false, || {
            panic!("oneshot must never probe the bus")
        });
        assert_eq!(kind, BackendKind::DirectByConfig);
        assert_eq!(fact, Fact::claimed(DaemonReachability::NotProbed));
    }

    #[test]
    fn daemon_mode_with_owner_selects_daemon() {
        let (kind, fact) = classify(&DaemonMode::Daemon, false, || true);
        assert_eq!(kind, BackendKind::Daemon);
        assert_eq!(fact, Fact::probed(DaemonReachability::Reachable));
    }

    #[test]
    fn daemon_mode_without_owner_is_fallback_not_config() {
        let (kind, fact) = classify(&DaemonMode::Daemon, false, || false);
        assert_eq!(kind, BackendKind::DirectByFallback);
        assert_eq!(fact, Fact::probed(DaemonReachability::Unreachable));
    }

    /// #314: under a non-default `--config` the daemon on the bus reads a
    /// different file, so daemon mode never selects it, whatever the bus
    /// says. The bus is not even asked: its answer could not change the
    /// selection, and a probe would be a fact `status` might then misreport.
    #[test]
    fn config_override_selects_direct_without_probing_whoever_owns_the_bus() {
        let (kind, fact) = classify(&DaemonMode::Daemon, true, || {
            panic!("a config override must never probe the bus")
        });
        assert_eq!(kind, BackendKind::DirectByOverride);
        assert_eq!(fact, Fact::claimed(DaemonReachability::NotProbed));
        assert!(kind.is_direct());
    }

    /// The override changes nothing for oneshot: direct access is already
    /// the configuration, and there is no daemon whose identity is in doubt,
    /// so the override note would explain a choice nothing made.
    #[test]
    fn config_override_leaves_oneshot_as_direct_by_config() {
        let (kind, fact) = classify(&DaemonMode::Oneshot, true, || {
            panic!("oneshot must never probe the bus")
        });
        assert_eq!(kind, BackendKind::DirectByConfig);
        assert_eq!(fact, Fact::claimed(DaemonReachability::NotProbed));
    }

    // -----------------------------------------------------------------------
    // status's daemon probe (H8): mode x ping outcome -> DaemonPing fact.
    // -----------------------------------------------------------------------

    #[test]
    fn oneshot_daemon_probe_never_pings() {
        let fact = probe_daemon_with(&DaemonMode::Oneshot, || {
            panic!("oneshot must never ping the daemon")
        });
        assert_eq!(fact, Fact::claimed(DaemonPing::NotConfigured));
    }

    #[test]
    fn daemon_probe_reports_a_completed_round_trip() {
        let fact = probe_daemon_with(&DaemonMode::Daemon, || Ok(()));
        assert_eq!(fact, Fact::probed(DaemonPing::Responding));
    }

    #[test]
    fn daemon_probe_carries_the_failure_detail() {
        let fact = probe_daemon_with(&DaemonMode::Daemon, || {
            Err(anyhow::anyhow!("D-Bus Ping call failed"))
        });
        assert_eq!(
            fact,
            Fact::probed(DaemonPing::NotResponding {
                error: "D-Bus Ping call failed".into()
            })
        );
    }

    /// Three kinds, three treatments: normal operation and an expected
    /// configuration stay quiet at DEBUG; only the degraded fallback WARNs
    /// and tells the human.
    #[test]
    fn announcement_levels_and_messages() {
        let (level, msg) = announcement(BackendKind::Daemon);
        assert_eq!(level, tracing::Level::DEBUG);
        assert_eq!(msg, None);

        let (level, msg) = announcement(BackendKind::DirectByConfig);
        assert_eq!(level, tracing::Level::DEBUG);
        assert_eq!(msg, None);

        let (level, msg) = announcement(BackendKind::DirectByFallback);
        assert_eq!(level, tracing::Level::WARN);
        assert_eq!(msg, Some(AccessMessage::DaemonUnreachableFallback));

        // An override is a choice the operator made, not a degraded state:
        // told once, at INFO, never with the fallback's warning.
        let (level, msg) = announcement(BackendKind::DirectByOverride);
        assert_eq!(level, tracing::Level::INFO);
        assert_eq!(msg, Some(AccessMessage::DirectByConfigOverride));
    }

    #[test]
    fn caps_state_the_transport_differences_up_front() {
        let config = oneshot_config("/nonexistent/facelock.db");
        let backend = Backend::select(&config);
        assert!(backend.kind().is_direct());
        let caps = backend.caps();
        assert!(!caps.graphical_preview);
        assert!(caps.device_formats);

        // The daemon caps are the mirror image (constructed directly — no
        // bus in unit tests).
        let daemon = Backend {
            kind: BackendKind::Daemon,
            config: &config,
        };
        assert!(!daemon.kind().is_direct());
        let caps = daemon.caps();
        assert!(caps.graphical_preview);
        assert!(!caps.device_formats);
    }

    // -----------------------------------------------------------------------
    // Direct reads: the C4/C7 failure policy, uniform across operations.
    // `mode = "oneshot"` pins DirectByConfig without touching D-Bus.
    // -----------------------------------------------------------------------

    fn oneshot_config(db_path: &str) -> Config {
        let mut config = Config::parse("[daemon]\nmode = \"oneshot\"\n").expect("config parses");
        config.storage.db_path = db_path.to_string();
        config
    }

    fn oneshot_config_at(db_path: &Path) -> Config {
        oneshot_config(&db_path.to_string_lossy())
    }

    /// C4/C7, direct: a fresh (absent) store answers benign reads as values
    /// — and the probe must not create the database it reports empty.
    #[test]
    fn absent_store_reads_are_benign_and_create_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        let config = oneshot_config_at(&db_path);
        let backend = Backend::select(&config);

        assert!(!backend.has_models("alice").unwrap());
        assert!(
            !backend
                .has_models_for_embedder("alice", "embedder")
                .unwrap()
        );
        assert!(backend.list_models("alice").unwrap().is_empty());
        assert_eq!(backend.remove_model("alice", 3).unwrap(), Some(false));
        // Nothing enrolled anywhere is nothing to clear — not an error, and
        // not a different answer from `remove_model`'s for the same state.
        assert_eq!(backend.clear_models("alice").unwrap(), Some(0));
        assert!(
            !db_path.exists(),
            "benign reads must not create the database they report empty"
        );
    }

    /// C4, direct: an unreadable (present) store is an error on every read —
    /// never "no models", never "assume yes".
    #[test]
    fn unreadable_store_propagates_on_every_read() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        std::fs::write(&db_path, b"this is not a sqlite database").unwrap();
        let config = oneshot_config_at(&db_path);
        let backend = Backend::select(&config);

        assert!(backend.has_models("alice").is_err());
        assert!(backend.has_models_for_embedder("alice", "e").is_err());
        assert!(backend.list_models("alice").is_err());
        assert!(backend.remove_model("alice", 1).is_err());
        assert!(backend.clear_models("alice").is_err());
    }

    /// The direct fork, end to end against a real store: one representative
    /// per-operation behavioral pin.
    #[test]
    fn direct_operations_answer_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        {
            let store = facelock_store::FaceStore::create(&db_path).unwrap();
            store
                .add_model("alice", "front", &[0.5f32; 512], "embedder-a")
                .unwrap();
            store
                .add_model("alice", "side", &[0.4f32; 512], "embedder-b")
                .unwrap();
        }
        let config = oneshot_config_at(&db_path);
        let backend = Backend::select(&config);

        assert!(backend.has_models("alice").unwrap());
        assert!(!backend.has_models("bob").unwrap());
        assert!(
            backend
                .has_models_for_embedder("alice", "embedder-a")
                .unwrap()
        );
        assert!(
            !backend
                .has_models_for_embedder("alice", "embedder-c")
                .unwrap()
        );
        assert_eq!(backend.list_models("alice").unwrap().len(), 2);

        let removed = backend.remove_model("alice", 1).unwrap();
        assert_eq!(removed, Some(true));
        assert_eq!(backend.remove_model("alice", 99).unwrap(), Some(false));

        assert_eq!(backend.clear_models("alice").unwrap(), Some(1));
        assert!(!backend.has_models("alice").unwrap());
    }

    // -----------------------------------------------------------------------
    // Structural pins (source scan, same discipline as resolved.rs): the
    // boolean fork stays deleted, and the daemon transport entry points stay
    // single-sited.
    // -----------------------------------------------------------------------

    fn source_files() -> Vec<(std::path::PathBuf, String)> {
        fn walk(dir: &Path, out: &mut Vec<(std::path::PathBuf, String)>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let content = std::fs::read_to_string(&path).unwrap();
                    out.push((path, content));
                }
            }
        }
        let mut out = Vec::new();
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
        out
    }

    fn code_contains(content: &str, needle: &str) -> bool {
        content
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.contains(needle))
    }

    /// The 13-site boolean fork (`should_use_direct`) is gone and must stay
    /// gone: transport is selected once, here.
    #[test]
    fn the_boolean_fork_stays_deleted() {
        // Assembled at runtime so this test's own literal doesn't count.
        let needle = format!("should_use_{}", "direct");
        for (path, content) in source_files() {
            assert!(
                !code_contains(&content, &needle),
                "{}: `{needle}` reappeared — transport is selected once via \
                 backend::Backend::select, not per call site (D1)",
                path.display()
            );
        }
    }

    /// Daemon transport entry points appear only at the allowed sites. A new
    /// transport call site outside the seam is a per-site fork trying to
    /// come back (D1). The needles are the typed `ipc_client` transport
    /// functions (D5) — `require_root`/`resolve_user`/`confirm` are not
    /// transport and stay unrestricted.
    #[test]
    fn daemon_transport_entry_points_are_single_sited() {
        // (function, allowed files). Needles are assembled at runtime so this
        // test's own table doesn't match itself; ipc_client.rs (the
        // definitions) is always allowed.
        let backend_only: &[&str] = &["backend.rs"];
        let preview: &[&str] = &[
            "commands/preview/text_only.rs",       // daemon frame loop
            "commands/preview/wayland_preview.rs", // daemon frame loop
        ];
        let pins: &[(&str, &[&str])] = &[
            ("test_authenticate", backend_only),
            ("list_models", backend_only),
            ("remove_model", backend_only),
            ("clear_models", backend_only),
            ("send_enroll", backend_only),
            ("list_devices", backend_only),
            ("ping", backend_only), // status's probe routes through probe_daemon (H8)
            ("preview_detect_frame", preview),
            ("release_camera", preview),
        ];

        for (path, content) in source_files() {
            let rel = path
                .to_string_lossy()
                .split("/src/")
                .last()
                .unwrap()
                .to_string();
            if rel == "ipc_client.rs" {
                continue;
            }
            for (function, allowed) in pins {
                let needle = format!("ipc_client::{function}(");
                if code_contains(&content, &needle) {
                    assert!(
                        allowed.iter().any(|a| rel == *a),
                        "{rel}: {needle} outside the backend seam — route the \
                         operation through backend::Backend (D1)"
                    );
                }
            }
        }
    }

    /// The CLI must never call the daemon's `Authenticate` method. That
    /// method is real authentication: it always charges the shared
    /// rate-limit budget, and its only legitimate callers are the
    /// authenticators (PAM, the polkit agent). `facelock test` is a
    /// diagnostic and goes to the root-only `TestAuthenticate` instead —
    /// keeping the two apart on the wire is what lets the daemon stop
    /// guessing an attempt's purpose from the caller's UID, which is how a
    /// failed face auth at a `sudo` prompt used to escape the limiter.
    #[test]
    fn the_cli_never_calls_the_real_authenticate_method() {
        // Assembled at runtime so this test's own literal doesn't count.
        let needle = format!("ipc_client::{}(", "authenticate");
        for (path, content) in source_files() {
            assert!(
                !code_contains(&content, &needle),
                "{}: `{needle}` is the always-charging real-auth method — \
                 `facelock test` must call ipc_client::test_authenticate",
                path.display()
            );
        }
    }

    /// D5: the handler's request/response enums are internal to
    /// facelock-daemon. The CLI talks through the typed `ipc_client`
    /// transport and the AuthOutcome/EnrollOutcome vocabulary; these type
    /// names reappearing here means the double translation is leaking back
    /// out of the daemon crate.
    #[test]
    fn handler_enums_never_appear_in_the_cli() {
        // Assembled at runtime so this test's own literals don't count.
        let request = format!("Daemon{}", "Request");
        let response = format!("Daemon{}", "Response");
        for (path, content) in source_files() {
            for needle in [&request, &response] {
                assert!(
                    !code_contains(&content, needle),
                    "{}: `{needle}` is facelock-daemon-internal (D5) — use the \
                     typed ipc_client transport or the outcome vocabulary",
                    path.display()
                );
            }
        }
    }
}
