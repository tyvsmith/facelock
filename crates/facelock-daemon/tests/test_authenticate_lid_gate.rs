//! The closed-lid physical-presence gate across the real service (issue #385).
//!
//! `pam_facelock.so` reads `/proc/acpi/button/lid` in the calling process.
//! The packaged daemon cannot: `systemd/facelock-daemon.service` sets
//! `ProcSubset=pid`, so `/proc/acpi` is not in its mount namespace and the
//! procfs resolver reports "no lid device" on every laptop. The daemon
//! therefore resolves the lid over the system bus instead
//! (`org.freedesktop.login1.Manager.LidClosed`), and this file pins the three
//! answers that resolution can give, plus who is exempt from asking.
//!
//! The lid check is injected rather than read off a live bus: these tests
//! must fail when the *gate* regresses, not when the runner has no logind.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use facelock_core::config::Config;
use facelock_core::notify::{Notifier, NullNotifier};
use facelock_core::types::CameraCaps;
use facelock_daemon::cancel::CancelToken;
use facelock_daemon::handler::Handler;
use facelock_daemon::rate_limit::RateLimiter;
use facelock_daemon::server::{CallerIdentity, FacelockService};
use facelock_store::FaceStore;
use facelock_test_support::fixtures;
use facelock_test_support::{MockCamera, MockFaceEngine};

/// The camera factory `Handler::new` takes. Named because the spelled-out
/// type trips `clippy::type_complexity`, which the `--all-targets` lint gate
/// makes a hard failure. Each integration test file is its own crate, so this
/// cannot be shared without exporting a test-only type from production code.
type MockCameraFactory = Box<dyn Fn(&Config) -> Result<MockCamera, String> + Send + Sync>;

/// `model_id` of an in-band recoverable rejection (the wire shape every
/// `pre_check` refusal uses, so PAM abstains instead of retrying oneshot).
const RECOVERABLE: i32 = -2;

/// A service whose SSH gate is off, so the lid is the only environment gate
/// under test.
fn service(abort_if_lid_closed: bool) -> FacelockService<MockCamera, MockFaceEngine> {
    let config = Config::parse(&format!(
        r#"
[recognition]
threshold = 0.45
timeout_secs = 2

[security]
require_ir = false
require_frame_variance = false
require_landmark_liveness = false
abort_if_ssh = false
abort_if_lid_closed = {abort_if_lid_closed}

[encryption]
method = "none"

[audit]
enabled = false
"#,
    ))
    .unwrap();

    let emb = fixtures::known_embedding(1);
    let store = FaceStore::open_memory().unwrap();
    store
        .add_model("alice", "front", &emb, "test-embedder")
        .unwrap();
    let rate_limiter = RateLimiter::new(
        config.security.rate_limit.max_attempts,
        config.security.rate_limit.window_secs,
    );
    let factory: MockCameraFactory = Box::new(|_| Ok(MockCamera::bright(64, 64, 60)));
    let handler = Handler::new(
        config,
        MockFaceEngine::one_face(emb),
        store,
        rate_limiter,
        CameraCaps::default(),
        Some(factory),
        None,
    )
    .unwrap();
    FacelockService::new(
        handler,
        None,
        None,
        Arc::new(|_user: &str| Box::new(NullNotifier) as Box<dyn Notifier>),
    )
}

fn caller(uid: u32, username: &str) -> CallerIdentity {
    CallerIdentity {
        uid,
        username: Some(username.into()),
    }
}

/// The session check the lid tests never exercise: `abort_if_ssh = false`
/// means it is dropped without being invoked.
async fn unused_session_check() -> Result<bool, String> {
    Err("the SSH gate is disabled in these fixtures".to_string())
}

#[track_caller]
fn assert_refused_in_band(
    result: zbus::fdo::Result<facelock_core::dbus_interface::AuthResult>,
    message: &str,
) {
    let auth = result.expect("a lid refusal travels in band, not as a D-Bus error");
    assert!(!auth.matched);
    assert_eq!(auth.model_id, RECOVERABLE);
    assert_eq!(auth.label, message);
}

#[tokio::test]
async fn a_closed_lid_refuses_before_the_camera_opens() {
    let refused = service(true)
        .authenticate_as_with_checks(
            caller(1000, "alice"),
            "alice",
            CancelToken::new(),
            unused_session_check,
            || async { Ok(true) },
        )
        .await;
    assert_refused_in_band(refused, "lid closed");
}

#[tokio::test]
async fn an_open_lid_reaches_recognition() {
    let allowed = service(true)
        .authenticate_as_with_checks(
            caller(1000, "alice"),
            "alice",
            CancelToken::new(),
            unused_session_check,
            || async { Ok(false) },
        )
        .await
        .unwrap();
    assert!(allowed.matched, "an open lid must reach recognition");
}

/// The posture choice this fix makes explicit: with the gate enabled, a lid
/// the daemon cannot resolve refuses rather than counting as open. The
/// refusal is in band, so a PAM stack still falls through to the password.
#[tokio::test]
async fn an_unresolvable_lid_refuses_rather_than_counting_as_open() {
    for reason in [
        "logind is not on the system bus",
        "read logind LidClosed property: timed out",
    ] {
        let refused = service(true)
            .authenticate_as_with_checks(
                caller(1000, "alice"),
                "alice",
                CancelToken::new(),
                unused_session_check,
                move || async move { Err(reason.to_string()) },
            )
            .await;
        assert_refused_in_band(refused, "lid state unavailable");
    }
}

/// Suspend, `ReleaseCamera`, shutdown and caller departure all cancel a
/// pending lid read. That is not a lid fault, and the audit trail must not
/// record it as one: `cancelled` is the frozen class for it, and the whole
/// reason `lid state unavailable` exists is so the log says which thing
/// actually happened.
#[tokio::test]
async fn a_cancelled_lid_read_reports_cancellation_not_a_lid_fault() {
    let cancel = CancelToken::new();
    cancel.cancel();
    let refused = service(true)
        .authenticate_as_with_checks(
            caller(1000, "alice"),
            "alice",
            cancel,
            unused_session_check,
            std::future::pending::<Result<bool, String>>,
        )
        .await;
    assert_refused_in_band(refused, "cancelled");
}

#[tokio::test]
async fn a_disabled_lid_gate_performs_no_lid_lookup() {
    let calls = Arc::new(AtomicUsize::new(0));
    let checked = Arc::clone(&calls);

    let result = service(false)
        .authenticate_as_with_checks(
            caller(1000, "alice"),
            "alice",
            CancelToken::new(),
            unused_session_check,
            move || async move {
                checked.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            },
        )
        .await
        .unwrap();

    assert!(result.matched);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// `sudo`, `login`, `su` and root-run greeters run their PAM stack as root,
/// so a real authentication reaches the daemon as UID 0. Exempting root here
/// would leave the gate inert on the primary documented PAM target — the same
/// mistake the rate limiter made before ADR 008.
#[tokio::test]
async fn a_root_caller_is_lid_gated_too() {
    let refused = service(true)
        .authenticate_as_with_checks(
            caller(0, "root"),
            "alice",
            CancelToken::new(),
            unused_session_check,
            || async { Ok(true) },
        )
        .await;
    assert_refused_in_band(refused, "lid closed");
}

/// `TestAuthenticate` is root-only diagnostic recognition and already skips
/// the lid gate (`PreCheckContext::test()`); it must not pay for a lid lookup
/// whose answer it would discard.
#[tokio::test]
async fn test_authenticate_skips_the_lid_gate_and_its_lookup() {
    let result = service(true)
        .test_authenticate_as(caller(0, "root"), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(
        result.matched,
        "the root-only diagnostic path stays lid-independent"
    );
}
