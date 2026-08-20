//! Authorization-layer integration tests for the D-Bus service (D6),
//! exercised through the real `FacelockService` with a real `Handler` built
//! around mock camera/engine and an in-memory store — the tests that were
//! impossible while the server lived in the bin-only CLI crate.
//!
//! Covered here: the method-level entry points end to end minus zbus —
//! per-method authorization (denials, the Authenticate self-scope, the
//! root-only catch-all), the in-band `-2`/`-3`/`-4` sentinel encoding with its
//! byte-exact protocol strings, similarity redaction for non-root callers
//! (and the `-4` sentinel that keeps a redacted caller able to tell a
//! face-seen non-match from an empty frame), and which entry point — and
//! which kind of failure — charges the rate limit (N11, ADR 008 §4).
//!
//! NOT covered here, deliberately: the zbus wiring — caller-identity
//! resolution from message headers (`GetConnectionUnixUser`), signal
//! emission, D-Bus activation, and the bus policy. A live system bus in
//! unit-style tests is fragile; the container tiers
//! (`just test-arch-pam`, `just test-arch-integration`) prove that layer
//! against a real bus and PAM stack.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use facelock_core::config::Config;
use facelock_core::notify::{Notifier, NullNotifier};
use facelock_core::types::{CameraCaps, IrTextureScale};
use facelock_daemon::cancel::CancelToken;
use facelock_daemon::handler::Handler;
use facelock_daemon::rate_limit::RateLimiter;
use facelock_daemon::server::{CallerIdentity, FacelockService};
use facelock_store::FaceStore;
use facelock_test_support::fixtures;
use facelock_test_support::{MockCamera, MockFaceEngine};
use zbus::fdo;

type MockService = FacelockService<MockCamera, MockFaceEngine>;

/// The camera factory `Handler::new` takes. Named because the spelled-out
/// type trips `clippy::type_complexity`, which the `--all-targets` lint gate
/// makes a hard failure. Each integration test file is its own crate, so this
/// cannot be shared without exporting a test-only type from production code.
type MockCameraFactory = Box<dyn Fn(&Config) -> Result<MockCamera, String> + Send + Sync>;

/// Gates configured so the mock flow reaches the comparison loop: no IR
/// requirement, no variance/liveness gates, environment aborts off,
/// plaintext store (no sealer), audit off.
fn test_config(max_attempts: u32, timeout_secs: u32) -> Config {
    Config::parse(&format!(
        r#"
[recognition]
threshold = 0.45
timeout_secs = {timeout_secs}

[security]
require_ir = false
require_frame_variance = false
require_landmark_liveness = false
abort_if_ssh = false
abort_if_lid_closed = false

[security.rate_limit]
max_attempts = {max_attempts}
window_secs = 60

[encryption]
method = "none"

[audit]
enabled = false
"#
    ))
    .unwrap()
}

fn handler_with(
    config: Config,
    engine: MockFaceEngine,
    store: FaceStore,
) -> Handler<MockCamera, MockFaceEngine> {
    let rate_limiter = RateLimiter::new(
        config.security.rate_limit.max_attempts,
        config.security.rate_limit.window_secs,
    );
    let factory: MockCameraFactory = Box::new(|_| Ok(MockCamera::bright(64, 64, 60)));
    Handler::new(
        config,
        engine,
        store,
        rate_limiter,
        CameraCaps::default(),
        Some(factory),
        None,
    )
    .unwrap()
}

fn service(handler: Handler<MockCamera, MockFaceEngine>) -> MockService {
    // No rebuild recipe (reload disabled) and a null notifier: tests assert
    // authorization and encoding, not delivery.
    FacelockService::new(
        handler,
        None,
        None,
        Arc::new(|_user: &str| Box::new(NullNotifier) as Box<dyn Notifier>),
    )
}

fn matching_service_for(user: &str) -> MockService {
    let emb = fixtures::known_embedding(1);
    let store = FaceStore::open_memory().unwrap();
    store
        .add_model(user, "front", &emb, "test-embedder")
        .unwrap();
    service(handler_with(
        test_config(5, 2),
        MockFaceEngine::one_face(emb),
        store,
    ))
}

/// A file-backed store, for the tests that must read the `rate_limit` table
/// back through a *second* connection — the counter those tests are about is
/// on disk, and an in-memory store is private to the handler holding it.
fn temp_db_path(test_name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "facelock-authz-{test_name}-{}-{unique}.db",
        std::process::id()
    ))
}

fn cleanup_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
}

/// A service whose store lives at `db_path` and holds one enrolled model for
/// `user`, with an engine that sees a face on every frame and matches none of
/// them — so every attempt is a real failed attempt (a guess, and a wrong
/// one) and the only question is what it costs. `max_attempts` sets the
/// budget.
///
/// Deliberately not `MockFaceEngine::no_faces()`: since ADR 008 §4 an attempt
/// where the camera saw nobody charges nothing, so a blind engine would make
/// every "this failure is charged" test below pass vacuously. That case has
/// its own helper, [`unseen_service_at`].
fn failing_service_at(db_path: &Path, user: &str, max_attempts: u32) -> MockService {
    service_at(
        db_path,
        user,
        max_attempts,
        MockFaceEngine::one_face(unrelated_embedding()),
    )
}

/// The counterpart: an engine that never sees a face, for the attempts that
/// must cost nothing.
fn unseen_service_at(db_path: &Path, user: &str, max_attempts: u32) -> MockService {
    service_at(db_path, user, max_attempts, MockFaceEngine::no_faces())
}

fn service_at(
    db_path: &Path,
    user: &str,
    max_attempts: u32,
    engine: MockFaceEngine,
) -> MockService {
    let store = FaceStore::create(db_path).unwrap();
    store
        .add_model(
            user,
            "front",
            &fixtures::known_embedding(1),
            "test-embedder",
        )
        .unwrap();
    service(handler_with(test_config(max_attempts, 1), engine, store))
}

/// A rendezvous with a request that is inside its camera factory.
///
/// The factory runs after authorization and after the capture slot is claimed,
/// so a request parked there is unambiguously *the* request in flight — which
/// is what the concurrency tests below need to state as a fact rather than
/// win as a race. Two flags, no channel: the factory runs on the blocking
/// pool, where a plain sleep loop is the cheapest correct thing.
#[derive(Debug, Default)]
struct CameraGate {
    opening: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
}

impl CameraGate {
    /// Called from the camera factory: announce arrival, then park.
    fn park(&self) {
        use std::sync::atomic::Ordering;
        self.opening.store(true, Ordering::SeqCst);
        while !self.released.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Resolves once a request has reached the factory.
    async fn wait_until_opening(&self) {
        use std::sync::atomic::Ordering;
        for _ in 0..2_000 {
            if self.opening.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        panic!("no request reached the camera factory");
    }

    fn release(&self) {
        self.released
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Re-arm for a second request through the same service.
    fn reopen(&self) {
        use std::sync::atomic::Ordering;
        self.opening.store(false, Ordering::SeqCst);
        self.released.store(false, Ordering::SeqCst);
    }
}

/// A service whose camera factory parks on `gate`, holding one enrolled model
/// for `user` and an engine that matches it — so a released request succeeds
/// and only the cancellation under test can make it end otherwise.
fn gated_service_at(db_path: &Path, user: &str, gate: Arc<CameraGate>) -> MockService {
    let emb = fixtures::known_embedding(1);
    let store = FaceStore::create(db_path).unwrap();
    store
        .add_model(user, "front", &emb, "test-embedder")
        .unwrap();

    let config = test_config(5, 2);
    let rate_limiter = RateLimiter::new(
        config.security.rate_limit.max_attempts,
        config.security.rate_limit.window_secs,
    );
    let factory: MockCameraFactory = Box::new(move |_| {
        gate.park();
        Ok(MockCamera::bright(64, 64, 60))
    });
    service(
        Handler::new(
            config,
            MockFaceEngine::one_face(emb),
            store,
            rate_limiter,
            CameraCaps::default(),
            Some(factory),
            None,
        )
        .unwrap(),
    )
}

fn caller(uid: u32, username: Option<&str>) -> CallerIdentity {
    CallerIdentity {
        uid,
        username: username.map(str::to_string),
    }
}

fn root() -> CallerIdentity {
    caller(0, Some("root"))
}

fn alice() -> CallerIdentity {
    caller(1000, Some("alice"))
}

#[track_caller]
fn assert_denied<T: std::fmt::Debug>(result: fdo::Result<T>, entry_point: &str) {
    match result {
        Err(fdo::Error::AccessDenied(_)) => {}
        other => panic!("{entry_point}: expected AccessDenied, got {other:?}"),
    }
}

#[track_caller]
fn assert_not_denied<T: std::fmt::Debug>(result: fdo::Result<T>, entry_point: &str) {
    if let Err(fdo::Error::AccessDenied(msg)) = result {
        panic!("{entry_point}: root must pass authorization, denied: {msg}");
    }
}

/// N13 through the real service: every entry point except Authenticate
/// denies a non-root caller with AccessDenied — including the metadata
/// surfaces (ListModels, ListDevices, Ping) and the continuous-score feed
/// (PreviewDetectFrame). A denied caller must never reach the handler or the
/// capture slot.
#[tokio::test]
async fn every_entry_point_except_authenticate_denies_non_root() {
    let svc = matching_service_for("alice");
    let a = alice();

    assert_denied(
        svc.test_authenticate_as(a.clone(), "alice", CancelToken::new())
            .await,
        "TestAuthenticate",
    );
    assert_denied(
        svc.enroll_as(a.clone(), "alice", "front", CancelToken::new())
            .await,
        "Enroll",
    );
    assert_denied(svc.list_models_as(a.clone(), "alice").await, "ListModels");
    assert_denied(
        svc.remove_model_as(a.clone(), "alice", 1).await,
        "RemoveModel",
    );
    assert_denied(svc.clear_models_as(a.clone(), "alice").await, "ClearModels");
    assert_denied(svc.preview_frame_as(a.clone()).await, "PreviewFrame");
    assert_denied(svc.list_devices_as(a.clone()).await, "ListDevices");
    assert_denied(svc.release_camera_as(a.clone()).await, "ReleaseCamera");
    assert_denied(svc.ping_as(a.clone()).await, "Ping");
    assert_denied(svc.shutdown_as(a.clone()).await, "Shutdown");
    assert_denied(
        svc.preview_detect_frame_as(a, "alice").await,
        "PreviewDetectFrame",
    );
}

/// Root passes authorization on every entry point. Handler-side outcomes
/// vary (this environment has no real cameras and refuses plaintext
/// enrollment), so the pin is "never AccessDenied", plus full success
/// assertions where the mock flow guarantees one.
#[tokio::test]
async fn root_passes_authorization_on_every_entry_point() {
    let svc = matching_service_for("alice");
    let r = root();

    assert_eq!(svc.ping_as(r.clone()).await.unwrap(), "pong");
    assert!(
        svc.test_authenticate_as(r.clone(), "alice", CancelToken::new())
            .await
            .unwrap()
            .matched
    );
    assert_eq!(
        svc.list_models_as(r.clone(), "alice").await.unwrap().len(),
        1
    );
    // C8 (wire, Phase E): the unit reply cannot say whether model 99 existed.
    svc.remove_model_as(r.clone(), "alice", 99).await.unwrap();
    svc.clear_models_as(r.clone(), "alice").await.unwrap();
    svc.release_camera_as(r.clone()).await.unwrap();
    let jpeg = svc.preview_frame_as(r.clone()).await.unwrap();
    assert!(
        !jpeg.is_empty(),
        "mock camera preview must produce JPEG bytes"
    );
    assert_not_denied(svc.list_devices_as(r.clone()).await, "ListDevices");
    // `method = "none"` without allow_plaintext: enroll refuses fast, but as
    // a handler error — authorization must already have passed.
    assert_not_denied(
        svc.enroll_as(r.clone(), "alice", "front", CancelToken::new())
            .await,
        "Enroll",
    );
    svc.shutdown_as(r).await.unwrap();
}

/// Authenticate is the one user-scoped method: a user may request
/// authentication for themselves (screen lockers run PAM as the user),
/// never for anyone else, and an unresolvable username fails closed.
#[tokio::test]
async fn authenticate_is_scoped_to_the_caller_own_user() {
    let svc = matching_service_for("alice");

    let own = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(own.matched, "self-request must run the real comparison");

    assert_denied(
        svc.authenticate_as(alice(), "bob", CancelToken::new())
            .await,
        "Authenticate (cross-user)",
    );
    assert!(
        svc.authenticate_as(caller(1000, None), "alice", CancelToken::new())
            .await
            .is_err(),
        "an unresolvable caller username must fail closed"
    );

    let as_root = svc
        .authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(as_root.matched, "root may authenticate any user");
}

/// N12 through the real service: the similarity score is a hill-climbing
/// oracle and is zeroed for every non-root caller; the boolean outcome and
/// matched model survive. Root sees the real score.
#[tokio::test]
async fn similarity_is_redacted_for_non_root_callers() {
    let svc = matching_service_for("alice");

    let unredacted = svc
        .authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(unredacted.matched);
    assert!(
        unredacted.similarity > 0.9,
        "identical embeddings must score high for root, got {}",
        unredacted.similarity
    );

    let redacted = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(redacted.matched, "redaction must not change the outcome");
    assert_eq!(redacted.model_id, unredacted.model_id);
    assert_eq!(
        redacted.similarity, 0.0,
        "score must be zeroed for non-root"
    );
}

/// Recoverable failures travel in-band as `model_id == -2` with the message
/// in `label` — never as a D-Bus error, which would make PAM fall back to a
/// fresh root oneshot and silently bypass daemon-side state. The two
/// protocol strings PAM matches must arrive byte-identical.
#[tokio::test]
async fn rate_limited_flows_in_band_with_the_exact_protocol_string() {
    let emb = fixtures::known_embedding(1);
    let store = FaceStore::open_memory().unwrap();
    store
        .add_model("alice", "front", &emb, "test-embedder")
        .unwrap();
    // Exhaust the budget before the service ever runs.
    let limiter = RateLimiter::new(1, 60);
    limiter.record_failure(&store, "alice").unwrap();

    let svc = service(handler_with(
        test_config(1, 1),
        MockFaceEngine::one_face(emb),
        store,
    ));

    let result = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(!result.matched);
    assert_eq!(result.model_id, -2, "recoverable error sentinel");
    assert_eq!(
        result.label, "rate limited",
        "PAM string-matches this exactly"
    );
}

#[tokio::test]
async fn require_ir_flows_in_band_with_the_exact_protocol_string() {
    let emb = fixtures::known_embedding(1);
    let store = FaceStore::open_memory().unwrap();
    store
        .add_model("alice", "front", &emb, "test-embedder")
        .unwrap();
    let mut config = test_config(5, 1);
    config.security.require_ir = true;

    // CameraCaps::default() is non-IR, so the gate must reject in-band.
    let svc = service(handler_with(config, MockFaceEngine::one_face(emb), store));

    let result = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(!result.matched);
    assert_eq!(result.model_id, -2);
    assert!(
        result.label.contains("IR camera required"),
        "PAM string-matches \"IR camera required\", got: {}",
        result.label
    );
}

#[tokio::test]
async fn unverified_y16_flows_in_band_without_opening_the_camera() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let emb = fixtures::known_embedding(1);
    let store = FaceStore::open_memory().unwrap();
    store
        .add_model("alice", "front", &emb, "test-embedder")
        .unwrap();
    let config = test_config(5, 1);
    assert!(
        !config.security.require_ir,
        "this test pins the no-bypass rule"
    );
    let rate_limiter = RateLimiter::new(5, 60);
    let opened = Arc::new(AtomicBool::new(false));
    let opened_in_factory = Arc::clone(&opened);
    let factory: MockCameraFactory = Box::new(move |_| {
        opened_in_factory.store(true, Ordering::SeqCst);
        Ok(MockCamera::bright(64, 64, 1))
    });
    let caps = CameraCaps {
        is_ir: true,
        ir_texture_scale: IrTextureScale::UnverifiedY16,
        ..Default::default()
    };
    let svc = service(
        Handler::new(
            config,
            MockFaceEngine::one_face(emb),
            store,
            rate_limiter,
            caps,
            Some(factory),
            None,
        )
        .unwrap(),
    );

    let result = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(!result.matched);
    assert_eq!(result.model_id, -2, "recoverable error sentinel");
    assert_eq!(
        result.label,
        "Y16 IR texture scale is unverified; authentication requires a verified y16_bit_depth (8..=16) quirk"
    );
    assert!(
        !opened.load(Ordering::SeqCst),
        "preflight knew the scale was unverified, so it must not open the camera"
    );
}

#[tokio::test]
async fn negotiated_y16_drift_rejects_before_warmup_or_auth_capture() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let emb = fixtures::known_embedding(1);
    let store = FaceStore::open_memory().unwrap();
    store
        .add_model("alice", "front", &emb, "test-embedder")
        .unwrap();
    let mut config = test_config(5, 1);
    config.device.warmup_frames = 3;
    let rate_limiter = RateLimiter::new(5, 60);
    let opened = Arc::new(AtomicBool::new(false));
    let opened_in_factory = Arc::clone(&opened);
    let factory: MockCameraFactory = Box::new(move |_| {
        opened_in_factory.store(true, Ordering::SeqCst);
        Ok(MockCamera::with_frames(Vec::new()).with_caps(CameraCaps {
            is_ir: true,
            ir_texture_scale: IrTextureScale::UnverifiedY16,
            ..Default::default()
        }))
    });
    // Interrogation predicted GREY. Only opening reveals that the driver
    // negotiated Y16, so this path must open but must never capture a frame.
    let predicted_caps = CameraCaps {
        is_ir: true,
        ir_texture_scale: IrTextureScale::NotY16,
        ..Default::default()
    };
    let svc = service(
        Handler::new(
            config,
            MockFaceEngine::one_face(emb),
            store,
            rate_limiter,
            predicted_caps,
            Some(factory),
            None,
        )
        .unwrap(),
    );

    let result = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();

    assert!(opened.load(Ordering::SeqCst));
    assert!(!result.matched);
    assert_eq!(result.model_id, -2, "recoverable error sentinel");
    assert_eq!(
        result.label,
        "Y16 IR texture scale is unverified; authentication requires a verified y16_bit_depth (8..=16) quirk"
    );
}

/// `suppress_unknown` + no enrolled models maps to the `-3` sentinel, which
/// PAM turns into PAM_AUTHINFO_UNAVAIL so the stack falls through.
#[tokio::test]
async fn suppress_unknown_maps_to_the_minus_three_sentinel() {
    let mut config = test_config(5, 1);
    config.security.suppress_unknown = true;

    let svc = service(handler_with(
        config,
        MockFaceEngine::no_faces(),
        FaceStore::open_memory().unwrap(),
    ));

    let result = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(!result.matched);
    assert_eq!(result.model_id, -3, "suppressed sentinel");
    assert!(result.label.is_empty());
}

/// An embedding no fixture is near. The `known_embedding` family are all
/// near-uniform unit vectors (cosine ≈ 1.0 to each other), so a genuine
/// non-match needs a vector pointing elsewhere: cosine from this one to any
/// of them is ≈ 1/√512 ≈ 0.044, far under the 0.45 test threshold.
fn unrelated_embedding() -> facelock_core::types::FaceEmbedding {
    let mut emb = [0.0f32; 512];
    emb[0] = 1.0;
    emb
}

/// A non-root caller must be able to tell "we saw you and it was not a match"
/// from "the camera saw nobody" — the distinction PAM needs to choose
/// `PAM_AUTH_ERR` over `PAM_IGNORE`.
///
/// It could not: PAM inferred "no face was seen" from `similarity == 0.0`,
/// but the N12 redaction (PR #108) zeroes the score for *every* non-root
/// caller, so for a user-run locker (hyprlock) both cases arrived identical
/// and both abstained. #108 spotted this and deferred it to #109, which never
/// carried it.
///
/// What this covers: the daemon's reply, through the real service and the
/// real redaction, for the two cases as an unprivileged caller sees them.
/// What it does not: PAM itself, which is a separate binary that cannot be
/// linked here — `pam_code_for_no_match` in `pam-facelock` covers the
/// decision this reply feeds.
#[tokio::test]
async fn a_redacted_caller_can_tell_a_seen_face_from_an_empty_frame() {
    let store = FaceStore::open_memory().unwrap();
    store
        .add_model(
            "alice",
            "front",
            &fixtures::known_embedding(1),
            "test-embedder",
        )
        .unwrap();
    let saw_someone = service(handler_with(
        test_config(5, 1),
        MockFaceEngine::one_face(unrelated_embedding()),
        store,
    ));

    let seen = saw_someone
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(!seen.matched, "an unrelated face must not authenticate");
    assert_eq!(
        seen.similarity, 0.0,
        "the score is redacted for a non-root caller, which is why the \
         sentinel has to carry the signal instead"
    );
    assert_eq!(
        seen.model_id, -4,
        "a face was detected and did not match: {seen:?}"
    );

    let empty_store = FaceStore::open_memory().unwrap();
    empty_store
        .add_model(
            "alice",
            "front",
            &fixtures::known_embedding(1),
            "test-embedder",
        )
        .unwrap();
    let saw_nobody = service(handler_with(
        test_config(5, 1),
        MockFaceEngine::no_faces(),
        empty_store,
    ));

    let unseen = saw_nobody
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(!unseen.matched);
    assert_eq!(unseen.similarity, 0.0);
    assert_eq!(unseen.model_id, -1, "no face was detected: {unseen:?}");

    // The point of the whole exercise: the two replies are no longer the same
    // bytes to a caller who cannot see the score.
    assert_ne!(
        (seen.model_id, seen.similarity),
        (unseen.model_id, unseen.similarity),
        "a redacted caller must still be able to distinguish these"
    );
}

/// The `-4` sentinel must not reach a caller who did match, and must not
/// disturb the score a root caller is allowed to see.
#[tokio::test]
async fn a_match_still_reports_its_model_id_and_score_to_root() {
    let emb = fixtures::known_embedding(1);
    let store = FaceStore::open_memory().unwrap();
    let model_id = store
        .add_model("alice", "front", &emb, "test-embedder")
        .unwrap();

    let svc = service(handler_with(
        test_config(5, 1),
        MockFaceEngine::one_face(emb),
        store,
    ));

    let result = svc
        .authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(result.matched);
    assert_eq!(result.model_id, model_id as i32);
    assert!(
        result.similarity > 0.45,
        "root sees the real score: {result:?}"
    );
}

/// REGRESSION PIN: a ROOT caller's failed `Authenticate` charges the
/// rate-limit budget.
///
/// The daemon used to exempt every root caller, on the theory that a root
/// caller must be root-only `facelock test`. But `pam_facelock` runs inside
/// the PAM stack of whatever program is authenticating, and `sudo` is
/// setuid-root — as are `login`, `su`, and root-run display-manager
/// greeters. PAM tries D-Bus first, so a real failed face authentication at
/// a `sudo` prompt reached the daemon as UID 0 and was never charged: the
/// documented 5-attempts/user/60s limit was inert on the project's primary
/// documented PAM target, and an attacker presenting spoof material there
/// had unlimited daemon-mediated attempts.
///
/// Asserted against the on-disk `rate_limit` table through a second
/// connection, not just through the next reply, so this cannot pass on a
/// coincidence of the in-band encoding.
#[tokio::test]
async fn root_failed_authenticate_charges_the_rate_limit() {
    let db_path = temp_db_path("root-charges");
    cleanup_db(&db_path);
    // Budget of one failed attempt.
    let svc = failing_service_at(&db_path, "alice", 1);

    let first = svc
        .authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(!first.matched);
    assert_eq!(
        first.model_id, -4,
        "a face was seen and did not match — the failure that is charged: {first:?}"
    );

    let inspect = FaceStore::create(&db_path).unwrap();
    assert!(
        !inspect.check_rate_limit("alice", 1, 60).unwrap(),
        "a root caller's failed Authenticate MUST consume budget — this is \
         how a failed face auth at a sudo prompt gets counted"
    );

    // And the charge is enforced: the next attempt is rejected in-band
    // before the camera runs, root or not.
    let limited = svc
        .authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert_eq!(limited.model_id, -2);
    assert_eq!(limited.label, "rate limited");

    cleanup_db(&db_path);
}

/// A cancelled attempt is an abstention, not a failed attempt: the user
/// never got to make one, so it must cost no rate-limit budget (ADR 008 §5).
/// Otherwise a screen locker that aborts PAM on every unlock would walk the
/// user into a lockout.
#[tokio::test]
async fn a_cancelled_authenticate_charges_no_rate_limit() {
    let db_path = temp_db_path("cancel-charges-nothing");
    cleanup_db(&db_path);
    // Budget of one failed attempt — three cancelled runs must not reach it.
    let svc = failing_service_at(&db_path, "alice", 1);

    for _ in 0..3 {
        // A request whose token is already set — what `ReleaseCamera`,
        // suspend and the caller-departure watch each do to the one request
        // they are aimed at.
        let cancel = CancelToken::new();
        cancel.cancel();
        let reply = svc.authenticate_as(root(), "alice", cancel).await.unwrap();
        assert!(!reply.matched);
        assert_eq!(
            reply.model_id, -2,
            "a cancellation travels as a recoverable error: {reply:?}"
        );
        assert_eq!(reply.label, "cancelled", "frozen wire string");
    }

    let inspect = FaceStore::create(&db_path).unwrap();
    assert!(
        inspect.check_rate_limit("alice", 1, 60).unwrap(),
        "cancelled attempts must leave the budget untouched"
    );

    cleanup_db(&db_path);
}

/// The complement of the pin above: a failure where the camera never saw a
/// face costs no budget (ADR 008 §4). It is not a guess — nobody was there —
/// and charging it means a laptop opened in front of an empty desk, or a
/// locker that starts face auth on every wake, burns the user's whole
/// allowance before they sit down and meets them with a lockout.
///
/// Asserted twice over: through the reply of a *second* attempt (which would
/// be the `-2` rate-limit rejection if the first had charged) and directly
/// against the on-disk `rate_limit` table.
#[tokio::test]
async fn a_no_face_authenticate_charges_no_rate_limit() {
    let db_path = temp_db_path("no-face-charges-nothing");
    cleanup_db(&db_path);
    // Budget of one failed attempt — neither no-face run may reach it.
    let svc = unseen_service_at(&db_path, "alice", 1);

    for attempt in 0..2 {
        let reply = svc
            .authenticate_as(root(), "alice", CancelToken::new())
            .await
            .unwrap();
        assert!(!reply.matched);
        assert_eq!(
            reply.model_id, -1,
            "attempt {attempt} must stay an ordinary no-face non-match, never \
             a rate-limit rejection: {reply:?}"
        );
    }

    let inspect = FaceStore::create(&db_path).unwrap();
    assert!(
        inspect.check_rate_limit("alice", 1, 60).unwrap(),
        "an attempt at an empty chair must leave the budget untouched"
    );

    cleanup_db(&db_path);
}

/// The other half: a non-root user's own failed attempts are charged, as
/// they always were.
#[tokio::test]
async fn user_failed_authenticate_charges_the_rate_limit() {
    let db_path = temp_db_path("user-charges");
    cleanup_db(&db_path);
    let svc = failing_service_at(&db_path, "alice", 1);

    let charged = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(!charged.matched);
    assert_eq!(charged.model_id, -4, "a face was seen and did not match");

    let limited = svc
        .authenticate_as(alice(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert_eq!(limited.model_id, -2);
    assert_eq!(limited.label, "rate limited");

    cleanup_db(&db_path);
}

/// N11, now explicit: the root-only `TestAuthenticate` method is the one
/// entry point whose failures cost nothing. `facelock test` runs here, and a
/// handful of failed test runs must not lock the user out of real
/// authentication. The exemption is asked for by calling this method — it is
/// no longer inferred from the caller being root.
#[tokio::test]
async fn test_authenticate_does_not_charge_the_rate_limit() {
    let db_path = temp_db_path("test-charges-nothing");
    cleanup_db(&db_path);
    // Budget of one failed attempt — three failed test runs must not reach it.
    let svc = failing_service_at(&db_path, "alice", 1);

    for attempt in 0..3 {
        let result = svc
            .test_authenticate_as(root(), "alice", CancelToken::new())
            .await
            .unwrap();
        assert!(!result.matched);
        assert_eq!(
            result.model_id, -4,
            "test attempt {attempt} must be an ordinary face-seen non-match, \
             not a rate-limit rejection: {result:?}"
        );
    }

    let inspect = FaceStore::create(&db_path).unwrap();
    assert!(
        inspect.check_rate_limit("alice", 1, 60).unwrap(),
        "the rate_limit table must be untouched by diagnostic runs"
    );

    // The *check* is not exempted: an already-limited user still sees it.
    // (Charge through the real method, then test again.)
    svc.authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    let limited = svc
        .test_authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert_eq!(limited.model_id, -2);
    assert_eq!(
        limited.label, "rate limited",
        "an existing lockout must surface in `test`, not be masked by it"
    );

    cleanup_db(&db_path);
}

/// `TestAuthenticate` is root-only — that is what makes a
/// budget-free authentication endpoint safe to expose at all. A non-root
/// caller must be denied even for their own username, unlike `Authenticate`.
#[tokio::test]
async fn test_authenticate_denies_a_non_root_caller() {
    let svc = matching_service_for("alice");

    assert_denied(
        svc.test_authenticate_as(alice(), "alice", CancelToken::new())
            .await,
        "TestAuthenticate (own user)",
    );
    assert_denied(
        svc.test_authenticate_as(alice(), "bob", CancelToken::new())
            .await,
        "TestAuthenticate (cross-user)",
    );

    // Root runs the real comparison through it.
    let as_root = svc
        .test_authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(as_root.matched, "root must reach the comparison loop");
}

/// The root preview path end to end: frames and per-face metadata with the
/// unredacted score (root-only).
#[tokio::test]
async fn preview_detect_frame_for_root_returns_frames_and_scores() {
    let svc = matching_service_for("alice");

    let (jpeg, faces) = svc.preview_detect_frame_as(root(), "alice").await.unwrap();

    assert!(!jpeg.is_empty(), "root gets raw frame bytes");
    assert_eq!(faces.len(), 1);
    assert!(faces[0].recognized);
    assert!(
        faces[0].similarity > 0.9,
        "root sees the unredacted score, got {}",
        faces[0].similarity
    );
}

// ---------------------------------------------------------------------------
// Per-request cancellation (ADR 008 §5)
//
// zbus dispatches every method call in its own task, so "concurrent" is the
// normal case, not an exotic one. These pin that a request's cancel token is
// reachable by exactly one request: the one that owns it.
// ---------------------------------------------------------------------------

/// Nothing another call does can disturb an in-flight request's cancellation.
///
/// With a single service-owned token this was false three ways over: a second
/// call re-armed the flag (clearing a cancellation the running request had not
/// read yet), a rejected call re-armed it just the same because the reset ran
/// *before* authorization and before the capture slot, and the rejected
/// caller's departure watch — subscribed against that same shared flag — could
/// cancel the request it had just been rejected in favour of.
///
/// The concurrency is real rather than simulated, and deterministic rather
/// than timed: the first request parks inside its camera factory — which runs
/// after authorization and after the capture slot is claimed — until this test
/// lets it go, so "while the first request is in flight" is a fact here, not a
/// race the test hopes to win.
#[tokio::test(flavor = "multi_thread")]
async fn a_concurrent_request_cannot_disturb_the_one_in_flight() {
    let db_path = temp_db_path("concurrent-token-isolation");
    cleanup_db(&db_path);
    let gate = Arc::new(CameraGate::default());
    let svc = Arc::new(gated_service_at(&db_path, "alice", gate.clone()));

    let in_flight = CancelToken::new();
    let running = {
        let svc = svc.clone();
        let token = in_flight.clone();
        tokio::spawn(async move { svc.authenticate_as(root(), "alice", token).await })
    };
    gate.wait_until_opening().await;

    // A second caller, arriving while the first holds the capture slot. It is
    // rejected — and the rejection must cost the running request nothing.
    let second = svc
        .authenticate_as(root(), "alice", CancelToken::new())
        .await;
    assert!(
        matches!(second, Err(fdo::Error::Failed(ref m)) if m.contains("daemon busy")),
        "expected a busy rejection while a capture was in flight, got {second:?}"
    );
    assert!(
        !in_flight.is_cancelled(),
        "a busy-rejected request reached the in-flight request's token"
    );

    // ...and so is a call that authorization turns away before it can get
    // anywhere near the capture slot.
    assert_denied(
        svc.test_authenticate_as(alice(), "alice", CancelToken::new())
            .await,
        "TestAuthenticate",
    );
    assert!(
        !in_flight.is_cancelled(),
        "a denied request reached the in-flight request's token"
    );

    // The slot still names the running request, so suspend / ReleaseCamera /
    // shutdown reach it and nothing else — the rejected calls above did not
    // displace it with a token of their own.
    svc.current_request().cancel();
    assert!(
        in_flight.is_cancelled(),
        "the in-flight request was not reachable through the current-request slot"
    );

    gate.release();
    let reply = running.await.unwrap().unwrap();
    assert_eq!(reply.label, "cancelled", "frozen wire string: {reply:?}");

    // And the slot is empty again: a `ReleaseCamera` arriving with nothing in
    // flight must not leave a set flag behind for whoever comes next.
    svc.current_request().cancel();
    let next = CancelToken::new();
    gate.reopen();
    let following = {
        let svc = svc.clone();
        let token = next.clone();
        tokio::spawn(async move { svc.authenticate_as(root(), "alice", token).await })
    };
    gate.wait_until_opening().await;
    assert!(
        !next.is_cancelled(),
        "a cancel aimed at a finished request landed on its successor"
    );
    gate.release();
    let reply = following.await.unwrap().unwrap();
    assert_ne!(
        reply.label, "cancelled",
        "the request after a cancelled one must run normally: {reply:?}"
    );

    cleanup_db(&db_path);
}

/// Each request is born with a fresh token, so a cancellation never outlives
/// the request it was aimed at.
///
/// The wedge this replaces: `CameraLease::finish` did not clear the shared
/// flag, so after any cancelled authentication every later request — a preview
/// frame most visibly, since a preview issues ten a second — found the flag
/// already set and failed with `cancelled` forever.
#[tokio::test]
async fn a_cancelled_request_does_not_wedge_the_next_one() {
    let svc = matching_service_for("alice");

    let cancel = CancelToken::new();
    cancel.cancel();
    let cancelled = svc.authenticate_as(root(), "alice", cancel).await.unwrap();
    assert!(!cancelled.matched);
    assert_eq!(cancelled.label, "cancelled", "{cancelled:?}");

    let frame = svc
        .preview_frame_as(root())
        .await
        .expect("a preview frame after a cancelled authentication");
    assert!(!frame.is_empty(), "the preview was wedged by the cancel");

    let after = svc
        .authenticate_as(root(), "alice", CancelToken::new())
        .await
        .unwrap();
    assert!(
        after.matched,
        "the authentication after a cancelled one must run normally: {after:?}"
    );
}
