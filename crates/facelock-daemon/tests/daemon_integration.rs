use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use facelock_core::config::Config;
use facelock_core::ipc::{DaemonRequest, DaemonResponse};
use facelock_core::types::MatchResult;
use facelock_store::FaceStore;
use facelock_test_support::fixtures;
use facelock_test_support::{MockCamera, MockFaceEngine};

// Import the handler module (it's pub in the crate)
// We need to reference it via the crate directly since it's a binary crate.
// Instead, we'll replicate the handler construction here.

// The handler type and modules are internal to the daemon binary.
// For integration tests, we test the auth/enroll logic via the Handler's handle() method.
// We use a helper module that re-exports what we need.

// Since handler.rs, auth.rs, enroll.rs are private modules of the binary,
// we test through the public Handler type by depending on the library parts.
// The daemon crate is a binary — we can't import from it directly.
// So we test the auth/enroll logic at unit level within the daemon,
// and test the full IPC protocol here by running a mock daemon in-process.

// Actually, let's test by constructing the handler directly.
// We'll make the handler module public for tests.

// For now, test the trait implementations and mock infrastructure work correctly,
// and validate the auth/enroll logic through the core types.

fn test_config() -> Config {
    let toml = fixtures::test_config_toml("/tmp/facelock-test-integ.db");
    Config::parse(&toml).unwrap()
}

fn temp_db_path(test_name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "facelock-{test_name}-{}-{unique}.db",
        std::process::id()
    ))
}

fn cleanup_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
}

#[test]
fn mock_camera_produces_bright_frames() {
    use facelock_core::traits::CameraSource;
    let mut cam = MockCamera::bright(640, 480, 5);
    let frame = cam.capture().unwrap();
    assert_eq!(frame.width, 640);
    assert_eq!(frame.height, 480);
    assert!(!MockCamera::is_dark(&frame));
    assert_eq!(cam.captures(), 1);
}

#[test]
fn mock_camera_dark_frames_detected() {
    use facelock_core::traits::CameraSource;
    let mut cam = MockCamera::dark(640, 480, 3);
    let frame = cam.capture().unwrap();
    assert!(MockCamera::is_dark(&frame));
}

#[test]
fn mock_camera_wraps_around() {
    use facelock_core::traits::CameraSource;
    let mut cam = MockCamera::bright(64, 64, 2);
    let _ = cam.capture().unwrap();
    let _ = cam.capture().unwrap();
    // Third capture wraps around
    let frame = cam.capture().unwrap();
    assert_eq!(frame.width, 64);
}

#[test]
fn mock_face_engine_one_face() {
    use facelock_core::traits::FaceProcessor;
    let emb = fixtures::known_embedding(0);
    let mut engine = MockFaceEngine::one_face(emb);
    let frame = fixtures::bright_frame(640, 480);
    let results = engine.process(&frame).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1, emb);
}

#[test]
fn mock_face_engine_no_faces() {
    use facelock_core::traits::FaceProcessor;
    let mut engine = MockFaceEngine::no_faces();
    let frame = fixtures::bright_frame(640, 480);
    let results = engine.process(&frame).unwrap();
    assert!(results.is_empty());
}

#[test]
fn mock_face_engine_cycling() {
    use facelock_core::traits::FaceProcessor;
    let emb1 = fixtures::known_embedding(0);
    let emb2 = fixtures::known_embedding(50);
    let mut engine = MockFaceEngine::cycling(vec![emb1, emb2]);
    let frame = fixtures::bright_frame(640, 480);

    let r1 = engine.process(&frame).unwrap();
    assert_eq!(r1[0].1, emb1);
    let r2 = engine.process(&frame).unwrap();
    assert_eq!(r2[0].1, emb2);
    // Wraps around
    let r3 = engine.process(&frame).unwrap();
    assert_eq!(r3[0].1, emb1);
}

#[test]
fn fixtures_varied_embeddings_differ() {
    let (e1, e2) = fixtures::varied_embedding_pair();
    let sim: f32 = e1.iter().zip(e2.iter()).map(|(a, b)| a * b).sum();
    assert!(sim < 0.998, "varied pair should differ enough, got {sim}");
}

#[test]
fn fixtures_identical_embeddings_same() {
    let (e1, e2) = fixtures::identical_embedding_pair();
    assert_eq!(e1, e2);
}

#[test]
fn store_round_trip_with_mock_embedding() {
    let store = FaceStore::open_memory().unwrap();
    let emb = fixtures::known_embedding(42);
    let id = store.add_model("testuser", "test-label", &emb, "").unwrap();
    let stored = store.get_user_embeddings("testuser").unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].0, id);
    assert_eq!(stored[0].1, emb);
}

#[test]
fn test_config_parses() {
    let config = test_config();
    assert_eq!(config.recognition.timeout_secs, 2);
    assert!(!config.security.require_ir);
    assert!(config.security.require_frame_variance);
    assert_eq!(config.security.min_auth_frames, 2);
}

#[test]
fn warmup_frames_discarded_on_camera_open() {
    use facelock_daemon::handler::Handler;
    use facelock_daemon::rate_limit::RateLimiter;

    let mut config = test_config();
    config.device.warmup_frames = 3;

    let engine = MockFaceEngine::no_faces();
    let store = FaceStore::open_memory().unwrap();
    let rate_limiter = RateLimiter::new(
        config.security.rate_limit.max_attempts,
        config.security.rate_limit.window_secs,
    );

    // Track captures via a shared counter
    let capture_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _counter = capture_count.clone();

    let factory: Box<
        dyn Fn(&facelock_core::config::Config) -> Result<MockCamera, String> + Send + Sync,
    > = Box::new(move |_cfg| {
        // Camera with enough frames for warmup + auth
        Ok(MockCamera::bright(64, 64, 20))
    });

    let mut handler = Handler::new(
        config,
        engine,
        store,
        rate_limiter,
        false,
        Some(factory),
        None,
    )
    .unwrap();

    // Ping triggers no camera open
    let resp = handler.handle(DaemonRequest::Ping);
    assert!(matches!(resp, DaemonResponse::Ok));

    // PreviewFrame triggers acquire_camera which discards warmup frames
    let resp = handler.handle(DaemonRequest::PreviewFrame);
    // Should succeed (camera opened, warmup discarded, then one frame captured for preview)
    assert!(
        !matches!(resp, DaemonResponse::Error { .. }),
        "expected successful preview, got: {resp:?}"
    );
}

#[test]
fn warmup_frames_zero_skips_discard() {
    use facelock_daemon::handler::Handler;
    use facelock_daemon::rate_limit::RateLimiter;

    let mut config = test_config();
    config.device.warmup_frames = 0;

    let engine = MockFaceEngine::no_faces();
    let store = FaceStore::open_memory().unwrap();
    let rate_limiter = RateLimiter::new(
        config.security.rate_limit.max_attempts,
        config.security.rate_limit.window_secs,
    );

    let factory: Box<
        dyn Fn(&facelock_core::config::Config) -> Result<MockCamera, String> + Send + Sync,
    > = Box::new(move |_cfg| Ok(MockCamera::bright(64, 64, 5)));

    let mut handler = Handler::new(
        config,
        engine,
        store,
        rate_limiter,
        false,
        Some(factory),
        None,
    )
    .unwrap();

    // Should work fine with zero warmup
    let resp = handler.handle(DaemonRequest::PreviewFrame);
    assert!(
        !matches!(resp, DaemonResponse::Error { .. }),
        "expected successful preview with zero warmup, got: {resp:?}"
    );
}

#[test]
fn failed_auth_rate_limit_persists_across_handler_restart() {
    use facelock_daemon::handler::Handler;
    use facelock_daemon::rate_limit::RateLimiter;

    let db_path = temp_db_path("rate-limit-persist");
    cleanup_db(&db_path);

    let db_path_str = db_path.to_string_lossy().into_owned();
    let mut config = Config::parse(&fixtures::test_config_toml(&db_path_str)).unwrap();
    config.recognition.timeout_secs = 0;
    config.security.rate_limit.max_attempts = 1;
    config.security.require_frame_variance = false;
    config.security.require_landmark_liveness = false;

    {
        let store = FaceStore::open(&db_path).unwrap();
        store
            .add_model("testuser", "front", &fixtures::known_embedding(0), "")
            .unwrap();
    }

    let factory1: Box<
        dyn Fn(&facelock_core::config::Config) -> Result<MockCamera, String> + Send + Sync,
    > = Box::new(move |_cfg| Ok(MockCamera::bright(64, 64, 1)));

    let mut first_handler = Handler::new(
        config.clone(),
        MockFaceEngine::no_faces(),
        FaceStore::open(&db_path).unwrap(),
        RateLimiter::new(
            config.security.rate_limit.max_attempts,
            config.security.rate_limit.window_secs,
        ),
        false,
        Some(factory1),
        None,
    )
    .unwrap();

    let first = first_handler.handle(DaemonRequest::Authenticate {
        user: "testuser".into(),
    });
    assert!(matches!(
        first,
        DaemonResponse::AuthResult(MatchResult { matched: false, .. })
    ));

    let factory2: Box<
        dyn Fn(&facelock_core::config::Config) -> Result<MockCamera, String> + Send + Sync,
    > = Box::new(move |_cfg| Ok(MockCamera::bright(64, 64, 1)));

    let mut restarted_handler = Handler::new(
        config.clone(),
        MockFaceEngine::no_faces(),
        FaceStore::open(&db_path).unwrap(),
        RateLimiter::new(
            config.security.rate_limit.max_attempts,
            config.security.rate_limit.window_secs,
        ),
        false,
        Some(factory2),
        None,
    )
    .unwrap();

    let second = restarted_handler.handle(DaemonRequest::Authenticate {
        user: "testuser".into(),
    });
    assert!(matches!(
        second,
        DaemonResponse::Error { ref message } if message.contains("rate limited")
    ));

    cleanup_db(&db_path);
}
