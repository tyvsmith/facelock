//! The remote-session physical-presence gate across the real service (D6).
//!
//! The daemon process environment is never caller provenance. For a
//! non-root D-Bus `Authenticate` with `abort_if_ssh = true`, the server must
//! consult the caller's live ProcessFD-backed logind session and deny every
//! remote or unverifiable identity as a D-Bus authorization error. Disabling
//! the setting skips the lookup altogether. Root skips only this provenance
//! gate; it still runs ordinary authorization and real authentication.

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

const PROCESS_PROVENANCE_DENIED: &str = "Authenticate requires a live local caller process";

fn service(abort_if_ssh: bool) -> FacelockService<MockCamera, MockFaceEngine> {
    let config = Config::parse(&format!(
        r#"
[recognition]
threshold = 0.45
timeout_secs = 2

[security]
require_ir = false
require_frame_variance = false
require_landmark_liveness = false
abort_if_ssh = {abort_if_ssh}
abort_if_lid_closed = false

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

#[track_caller]
fn assert_process_provenance_denied(
    result: zbus::fdo::Result<facelock_core::dbus_interface::AuthResult>,
) {
    match result {
        Err(zbus::fdo::Error::AccessDenied(message)) => {
            assert_eq!(message, PROCESS_PROVENANCE_DENIED);
        }
        other => panic!("expected out-of-band AccessDenied, got {other:?}"),
    }
}

#[tokio::test]
async fn non_root_authenticate_allows_local_and_denies_remote_session() {
    let local = service(true)
        .authenticate_as_with_session_check(
            caller(1000, "alice"),
            "alice",
            CancelToken::new(),
            || async { Ok(false) },
        )
        .await
        .unwrap();
    assert!(
        local.matched,
        "a verified local session reaches recognition"
    );

    let remote = service(true)
        .authenticate_as_with_session_check(
            caller(1000, "alice"),
            "alice",
            CancelToken::new(),
            || async { Ok(true) },
        )
        .await;
    assert_process_provenance_denied(remote);
}

#[tokio::test]
async fn missing_invalid_and_dead_processfd_fail_closed_out_of_band() {
    for reason in [
        "D-Bus credentials omitted ProcessFD",
        "ProcessFD is not a pidfd",
        "ProcessFD process exited",
    ] {
        let denied = service(true)
            .authenticate_as_with_session_check(
                caller(1000, "alice"),
                "alice",
                CancelToken::new(),
                move || async move { Err(reason.to_string()) },
            )
            .await;
        assert_process_provenance_denied(denied);
    }
}

#[tokio::test]
async fn disabled_remote_gate_performs_no_process_or_session_lookup() {
    let calls = Arc::new(AtomicUsize::new(0));
    let checked = Arc::clone(&calls);

    let result = service(false)
        .authenticate_as_with_session_check(
            caller(1000, "alice"),
            "alice",
            CancelToken::new(),
            move || async move {
                checked.fetch_add(1, Ordering::SeqCst);
                Err("lookup must not run".to_string())
            },
        )
        .await
        .unwrap();

    assert!(result.matched);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ordinary_authorization_denial_precedes_process_lookup() {
    let calls = Arc::new(AtomicUsize::new(0));
    let checked = Arc::clone(&calls);

    let result = service(true)
        .authenticate_as_with_session_check(
            caller(1000, "alice"),
            "bob",
            CancelToken::new(),
            move || async move {
                checked.fetch_add(1, Ordering::SeqCst);
                Ok(false)
            },
        )
        .await;

    assert!(matches!(result, Err(zbus::fdo::Error::AccessDenied(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn root_bypasses_only_remote_provenance() {
    let calls = Arc::new(AtomicUsize::new(0));
    let checked = Arc::clone(&calls);

    let result = service(true)
        .authenticate_as_with_session_check(
            caller(0, "root"),
            "alice",
            CancelToken::new(),
            move || async move {
                checked.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            },
        )
        .await
        .unwrap();

    assert!(result.matched, "root still runs real Authenticate");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
