//! Benchmark and calibration subcommands.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use tracing::{info, warn};

use facelock_camera::Camera;
use facelock_core::Config;
use facelock_core::types::{FaceEmbedding, Wiped, cosine_similarity};
use facelock_face::FaceEngine;
use facelock_store::FaceStore;

use crate::direct;

/// Performance targets in milliseconds.
const TARGET_COLD_AUTH_MS: u64 = 3000;
const TARGET_WARM_AUTH_MS: u64 = 450;
const TARGET_PREVIEW_MS: u64 = 120;
const TARGET_ENROLLMENT_MS: u64 = 12000;
const TARGET_MODEL_LOAD_MS: u64 = 2000;

/// Number of iterations for warm benchmarks.
const WARM_ITERATIONS: u32 = 10;

/// Number of snapshots for enrollment benchmark.
const ENROLLMENT_SNAPSHOTS: u32 = 5;

/// Default number of reopen cycles for `bench camera-reopen`.
///
/// Five is enough for a stable median without keeping the IR emitter lit for
/// long: each cycle costs a full cold open plus the warmup discard.
const REOPEN_ITERATIONS: u32 = 5;

#[derive(Subcommand)]
pub enum BenchCommand {
    /// Measure cold auth latency (model load + first auth)
    ColdAuth,
    /// Measure warm auth latency (models already loaded)
    WarmAuth,
    /// Measure preview frame capture latency
    Preview,
    /// Measure enrollment time
    Enrollment,
    /// Measure ONNX model load time
    ModelLoad,
    /// Sweep thresholds and measure FAR/FRR
    Calibrate,
    /// Measure what reopening the camera costs (open, STREAMON, warmup)
    CameraReopen {
        /// Number of close→open→first-usable-frame cycles to time
        #[arg(long, default_value_t = REOPEN_ITERATIONS)]
        iterations: u32,
    },
    /// Generate a benchmark report
    Report,
}

pub fn run(config: &Config, command: BenchCommand) -> Result<()> {
    // DEC-6: `bench` is root by default (direct-mode access needs the 0600
    // root:root database regardless of subcommand, and auth benchmarks may
    // need TPM access besides); root is established by `main`'s
    // `require_root_for` gate (C6). Supersedes the old TPM-only conditional
    // check.

    match command {
        BenchCommand::ColdAuth => cmd_cold_auth(config),
        BenchCommand::WarmAuth => cmd_warm_auth(config),
        BenchCommand::Preview => cmd_preview(config),
        BenchCommand::Enrollment => cmd_enrollment(config),
        BenchCommand::ModelLoad => cmd_model_load(config),
        BenchCommand::Calibrate => cmd_calibrate(config),
        BenchCommand::CameraReopen { iterations } => cmd_camera_reopen(config, iterations),
        BenchCommand::Report => cmd_report(config),
    }
}

/// Resolve the model directory from config.
fn model_dir(config: &Config) -> &Path {
    Path::new(&config.daemon.model_dir)
}

/// Open the camera from config (with quirks and warmup frame discarding).
fn open_camera(config: &Config) -> Result<Camera<'static>> {
    direct::open_camera(config).context("Failed to open camera")
}

/// Load FaceEngine from config.
fn load_engine(config: &Config) -> Result<FaceEngine> {
    FaceEngine::load(&config.recognition, model_dir(config))
        .context("Failed to load face engine. Ensure models are installed.")
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

fn cmd_cold_auth(config: &Config) -> Result<()> {
    let user = current_user();

    println!("=== Cold Auth Benchmark ===");
    println!("Measuring: config load + model load + camera open + first auth attempt");
    println!();

    let start = Instant::now();

    // Load models (cold)
    let mut engine = load_engine(config)?;

    // Open camera
    let mut camera = open_camera(config)?;

    // Open store
    let store = FaceStore::open_existing(Path::new(&config.storage.db_path))
        .context("Failed to open face store")?;

    // Decrypted templates (the CLI's decryption-aware load), zeroized when
    // the guard drops — early bails and panics included (#141).
    let embeddings = Wiped::new(direct::load_user_embeddings(&store, config, &user)?);
    if embeddings.is_empty() {
        bail!(
            "No enrolled faces for user '{}'. Enroll first with `facelock enroll`.",
            user
        );
    }

    // Capture frames until we detect a face or timeout, like the real auth loop.
    // The first few frames from a V4L2 camera are often dark/blank while the
    // sensor adjusts exposure.
    let timeout = std::time::Duration::from_secs(config.recognition.timeout_secs as u64);
    let deadline = start + timeout;
    let mut faces = Wiped::new(Vec::new());
    let mut frames_captured = 0u32;

    while Instant::now() < deadline {
        let frame = camera.capture().context("Failed to capture frame")?;
        frames_captured += 1;
        faces = Wiped::new(engine.process(&frame)?);
        if !faces.is_empty() {
            break;
        }
    }

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;

    let matched = find_best_match(&faces, &embeddings, config.recognition.threshold);

    println!("Cold auth time:  {}ms", elapsed_ms);
    println!("Target:          <{}ms", TARGET_COLD_AUTH_MS);
    println!(
        "Result:          {}",
        pass_fail(elapsed_ms, TARGET_COLD_AUTH_MS)
    );
    println!("Frames captured: {}", frames_captured);
    println!("Faces detected:  {}", faces.len());
    println!(
        "Auth result:     {}",
        if matched { "MATCH" } else { "NO MATCH" }
    );

    Ok(())
}

fn cmd_warm_auth(config: &Config) -> Result<()> {
    let user = current_user();

    println!("=== Warm Auth Benchmark ===");
    println!(
        "Measuring: auth latency with pre-loaded models ({} iterations)",
        WARM_ITERATIONS
    );
    println!();

    // Pre-load everything
    let mut engine = load_engine(config)?;
    let mut camera = open_camera(config)?;
    let store = FaceStore::open_existing(Path::new(&config.storage.db_path))
        .context("Failed to open face store")?;

    // Decrypted templates (the CLI's decryption-aware load), zeroized when
    // the guard drops — early bails and panics included (#141).
    let embeddings = Wiped::new(direct::load_user_embeddings(&store, config, &user)?);
    if embeddings.is_empty() {
        bail!(
            "No enrolled faces for user '{}'. Enroll first with `facelock enroll`.",
            user
        );
    }

    // Warm up with one capture
    let _ = camera.capture();

    let mut times = Vec::with_capacity(WARM_ITERATIONS as usize);

    for i in 0..WARM_ITERATIONS {
        let start = Instant::now();
        let frame = camera.capture().context("Failed to capture frame")?;
        let faces = Wiped::new(engine.process(&frame)?);
        let _matched = find_best_match(&faces, &embeddings, config.recognition.threshold);
        let elapsed_ms = start.elapsed().as_millis() as u64;
        times.push(elapsed_ms);
        info!(iteration = i + 1, elapsed_ms, "warm auth iteration");
    }

    let min = *times.iter().min().unwrap_or(&0);
    let max = *times.iter().max().unwrap_or(&0);
    let avg = times.iter().sum::<u64>() / times.len().max(1) as u64;
    let median = percentile(&mut times, 50);
    let p95 = percentile(&mut times, 95);

    println!("Iterations:  {}", WARM_ITERATIONS);
    println!("Min:         {}ms", min);
    println!("Max:         {}ms", max);
    println!("Average:     {}ms", avg);
    println!("Median:      {}ms", median);
    println!("P95:         {}ms", p95);
    println!("Target:      <{}ms", TARGET_WARM_AUTH_MS);
    println!("Result:      {}", pass_fail(median, TARGET_WARM_AUTH_MS));

    Ok(())
}

fn cmd_preview(config: &Config) -> Result<()> {
    println!("=== Preview Frame Benchmark ===");
    println!(
        "Measuring: frame capture + face detection ({} iterations)",
        WARM_ITERATIONS
    );
    println!();

    let mut engine = load_engine(config)?;
    let mut camera = open_camera(config)?;

    // Warm up
    let _ = camera.capture();

    let mut capture_times = Vec::with_capacity(WARM_ITERATIONS as usize);
    let mut detect_times = Vec::with_capacity(WARM_ITERATIONS as usize);
    let mut total_times = Vec::with_capacity(WARM_ITERATIONS as usize);

    for i in 0..WARM_ITERATIONS {
        let total_start = Instant::now();

        let cap_start = Instant::now();
        let frame = camera.capture().context("Failed to capture frame")?;
        let cap_ms = cap_start.elapsed().as_millis() as u64;

        let det_start = Instant::now();
        let _faces = Wiped::new(engine.process(&frame)?);
        let det_ms = det_start.elapsed().as_millis() as u64;

        let total_ms = total_start.elapsed().as_millis() as u64;

        capture_times.push(cap_ms);
        detect_times.push(det_ms);
        total_times.push(total_ms);
        info!(
            iteration = i + 1,
            cap_ms, det_ms, total_ms, "preview iteration"
        );
    }

    let cap_median = percentile(&mut capture_times, 50);
    let det_median = percentile(&mut detect_times, 50);
    let total_median = percentile(&mut total_times, 50);

    println!("Capture median:    {}ms", cap_median);
    println!("Detection median:  {}ms", det_median);
    println!("Total median:      {}ms", total_median);
    println!("Target:            <{}ms", TARGET_PREVIEW_MS);
    println!(
        "Result:            {}",
        pass_fail(total_median, TARGET_PREVIEW_MS)
    );

    Ok(())
}

fn cmd_enrollment(config: &Config) -> Result<()> {
    println!("=== Enrollment Benchmark ===");
    println!(
        "Measuring: time to capture and embed {} snapshots",
        ENROLLMENT_SNAPSHOTS
    );
    println!("NOTE: embeddings are NOT stored (dry run)");
    println!();

    let mut engine = load_engine(config)?;
    let mut camera = open_camera(config)?;

    // Warm up
    let _ = camera.capture();

    let start = Instant::now();
    let mut face_count = 0u32;

    for i in 0..ENROLLMENT_SNAPSHOTS {
        let frame = camera.capture().context("Failed to capture frame")?;
        let faces = Wiped::new(engine.process(&frame)?);
        if faces.is_empty() {
            warn!(snapshot = i + 1, "No face detected in snapshot");
        } else {
            face_count += 1;
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;

    println!("Snapshots:     {}", ENROLLMENT_SNAPSHOTS);
    println!("Faces found:   {} / {}", face_count, ENROLLMENT_SNAPSHOTS);
    println!("Total time:    {}ms", elapsed_ms);
    println!(
        "Per snapshot:  {}ms",
        elapsed_ms / ENROLLMENT_SNAPSHOTS as u64
    );
    println!("Target:        <{}ms", TARGET_ENROLLMENT_MS);
    println!(
        "Result:        {}",
        pass_fail(elapsed_ms, TARGET_ENROLLMENT_MS)
    );

    Ok(())
}

fn cmd_model_load(config: &Config) -> Result<()> {
    println!("=== Model Load Benchmark ===");
    println!("Measuring: ONNX model load time (SCRFD + ArcFace)");
    println!();

    let iterations = 3u32;
    let mut times = Vec::with_capacity(iterations as usize);

    for i in 0..iterations {
        let start = Instant::now();
        let _engine = load_engine(config)?;
        let elapsed_ms = start.elapsed().as_millis() as u64;
        times.push(elapsed_ms);
        info!(iteration = i + 1, elapsed_ms, "model load iteration");
    }

    let min = *times.iter().min().unwrap_or(&0);
    let max = *times.iter().max().unwrap_or(&0);
    let avg = times.iter().sum::<u64>() / times.len().max(1) as u64;

    println!("Iterations:  {}", iterations);
    println!("Min:         {}ms", min);
    println!("Max:         {}ms", max);
    println!("Average:     {}ms", avg);
    println!("Target:      <{}ms", TARGET_MODEL_LOAD_MS);
    println!("Result:      {}", pass_fail(avg, TARGET_MODEL_LOAD_MS));

    Ok(())
}

fn cmd_calibrate(config: &Config) -> Result<()> {
    let user = current_user();

    println!("=== Threshold Calibration ===");
    println!("Sweeping recognition.threshold from 0.20 to 0.80 (step 0.05)");
    println!();

    let mut engine = load_engine(config)?;
    let mut camera = open_camera(config)?;
    let store = FaceStore::open_existing(Path::new(&config.storage.db_path))
        .context("Failed to open face store")?;

    // Decrypted templates (the CLI's decryption-aware load), zeroized when
    // the guard drops — early bails and panics included (#141).
    let enrolled = Wiped::new(direct::load_user_embeddings(&store, config, &user)?);
    if enrolled.is_empty() {
        bail!(
            "No enrolled faces for user '{}'. Enroll first with `facelock enroll`.",
            user
        );
    }

    // Capture several test frames to get live embeddings
    let num_test_frames = 10u32;
    println!("Capturing {} test frames from camera...", num_test_frames);

    // Live-capture probe embeddings are biometric data like the stored set
    // above: accumulated inside the guard, zeroized when it drops — bails,
    // panics and buffer growth included (#269).
    let mut live_embeddings: Wiped<Vec<FaceEmbedding>, FaceEmbedding> =
        Wiped::with_capacity(num_test_frames as usize);
    for _ in 0..num_test_frames {
        let frame = camera.capture().context("Failed to capture frame")?;
        let faces = Wiped::new(engine.process(&frame)?);
        for (_, emb) in faces.iter() {
            live_embeddings.push(*emb);
        }
    }

    if live_embeddings.is_empty() {
        bail!("No faces detected in test frames. Ensure you are facing the camera.");
    }

    println!(
        "Captured {} face embeddings from live frames",
        live_embeddings.len()
    );
    println!();

    // Compute all similarities between live and enrolled embeddings
    let mut similarities: Vec<f32> = Vec::new();
    for live_emb in live_embeddings.iter() {
        for (_, enrolled_emb) in enrolled.iter() {
            similarities.push(cosine_similarity(live_emb, enrolled_emb));
        }
    }

    let avg_sim = similarities.iter().sum::<f32>() / similarities.len().max(1) as f32;
    let max_sim = similarities
        .iter()
        .cloned()
        .fold(f32::NEG_INFINITY, f32::max);
    let min_sim = similarities.iter().cloned().fold(f32::INFINITY, f32::min);

    println!(
        "Similarity stats: min={:.4} avg={:.4} max={:.4}",
        min_sim, avg_sim, max_sim
    );
    println!();

    // Threshold sweep
    println!("{:<12} {:<10} {:<12}", "Threshold", "Matches", "Match Rate");
    println!("{}", "-".repeat(34));

    let mut best_threshold = config.recognition.threshold;
    let mut best_rate_diff = f32::MAX;
    let target_rate = 0.90; // aim for 90% match rate for genuine user

    let mut step = 0.20f32;
    while step <= 0.80 + f32::EPSILON {
        let threshold = (step * 100.0).round() / 100.0;
        let matches = similarities.iter().filter(|&&s| s >= threshold).count();
        let rate = matches as f32 / similarities.len().max(1) as f32;

        println!(
            "{:<12.2} {:<10} {:<12.1}%",
            threshold,
            matches,
            rate * 100.0
        );

        let rate_diff = (rate - target_rate).abs();
        if rate_diff < best_rate_diff {
            best_rate_diff = rate_diff;
            best_threshold = threshold;
        }

        step += 0.05;
    }

    println!();
    println!("Recommended threshold: {:.2}", best_threshold);
    println!(
        "Current configured threshold: {:.2}",
        config.recognition.threshold
    );
    println!();

    // Detector confidence sweep
    println!("=== Detector Confidence Sweep ===");
    println!("Sweeping detection_confidence from 0.30 to 0.90 (step 0.10)");
    println!();
    println!("{:<12} {:<15}", "Confidence", "Detections");
    println!("{}", "-".repeat(27));

    // Re-capture a single frame for detection sweep
    let frame = camera.capture().context("Failed to capture frame")?;

    let mut conf = 0.30f32;
    while conf <= 0.90 + f32::EPSILON {
        let confidence = (conf * 100.0).round() / 100.0;
        let mut sweep_config = config.recognition.clone();
        sweep_config.detection_confidence = confidence;

        match FaceEngine::load(&sweep_config, model_dir(config)) {
            Ok(mut sweep_engine) => match sweep_engine.process(&frame).map(Wiped::new) {
                Ok(faces) => {
                    println!("{:<12.2} {:<15}", confidence, faces.len());
                }
                Err(e) => {
                    println!("{:<12.2} error: {}", confidence, e);
                }
            },
            Err(e) => {
                println!("{:<12.2} load error: {}", confidence, e);
            }
        }

        conf += 0.10;
    }

    Ok(())
}

/// One timed close→open→first-usable-frame cycle, split by phase.
///
/// The split is the point of the benchmark: "reopening costs ~400 ms" was an
/// assertion nobody had measured (ADR 008 §2), and the three phases have
/// different fixes — a slow `open` is driver/format negotiation, a slow
/// `first_frame` is the sensor starting to stream, a slow `warmup` is
/// `device.warmup_frames` (or a quirk override) buying AGC/AE settling time.
#[derive(Default)]
struct ReopenSplit {
    /// Resolve the device, open it, negotiate the capture format. No frames.
    open_ms: u64,
    /// The first `capture()`, which is what actually issues `STREAMON`: the
    /// V4L2 stream starts lazily on the first dequeue, so the cost of
    /// starting the sensor lands here rather than in `open`.
    first_frame_ms: u64,
    /// The rest of the warmup discard (the frame above was its first).
    warmup_ms: u64,
    /// The first frame an authentication would actually analyze.
    usable_ms: u64,
}

impl ReopenSplit {
    fn total_ms(&self) -> u64 {
        self.open_ms + self.first_frame_ms + self.warmup_ms + self.usable_ms
    }
}

/// Time what it costs to go from no camera to the first frame an
/// authentication can analyze.
///
/// This is the number `device.camera_release_secs` is traded against: holding
/// the stream open after a failed attempt spends LED-on time to save exactly
/// this, so the hold length is only arguable once it has been measured on the
/// device in question (ADR 008 §9, §11).
fn cmd_camera_reopen(config: &Config, iterations: u32) -> Result<()> {
    if iterations == 0 {
        bail!("--iterations must be at least 1");
    }

    println!("=== Camera Reopen Benchmark ===");
    println!(
        "Measuring: open + format negotiation, STREAMON + first frame, warmup discard, \
         first usable frame ({} iterations)",
        iterations
    );
    println!();

    let mut splits: Vec<ReopenSplit> = Vec::with_capacity(iterations as usize);
    let mut device_path = String::new();
    let mut format = String::new();
    let mut warmup_frames = 0u32;

    for i in 0..iterations {
        // Every iteration starts closed: the previous camera was dropped
        // before this timer started, so its STREAMOFF and IR-emitter shutdown
        // are not billed to this open.
        let mut split = ReopenSplit::default();

        let open_start = Instant::now();
        let resolved = direct::resolve_camera_device(config)?;
        // Quirk override beats the config value, the same rule the auth path
        // applies (`direct::open_camera`). Read before `open` consumes the
        // resolution.
        let warmup = resolved
            .quirk
            .as_ref()
            .and_then(|q| q.warmup_frames)
            .unwrap_or(config.device.warmup_frames);
        let path = resolved.info.path.clone();
        let mut camera = resolved
            .open(&config.device)
            .context("Failed to open camera")?;
        split.open_ms = open_start.elapsed().as_millis() as u64;

        let first_start = Instant::now();
        camera
            .capture()
            .context("Failed to capture the first frame")?;
        split.first_frame_ms = first_start.elapsed().as_millis() as u64;

        let warmup_start = Instant::now();
        // `1..warmup`, not `0..warmup`: the capture above already spent the
        // first of the warmup frames. Empty for `warmup <= 1`.
        for _ in 1..warmup {
            camera.capture().context("Failed to capture warmup frame")?;
        }
        split.warmup_ms = warmup_start.elapsed().as_millis() as u64;

        // With no warmup configured the frame captured above is already the
        // one an authentication would analyze, so there is nothing left to
        // charge — capturing another would report a cost the real path never
        // pays.
        if warmup > 0 {
            let usable_start = Instant::now();
            camera
                .capture()
                .context("Failed to capture the first usable frame")?;
            split.usable_ms = usable_start.elapsed().as_millis() as u64;
        }

        device_path = path;
        format = camera.format().trim().to_string();
        warmup_frames = warmup;

        println!(
            "iteration {:<2} open {:>5}ms  first_frame {:>5}ms  warmup {:>5}ms  \
             usable {:>5}ms  total {:>5}ms",
            i + 1,
            split.open_ms,
            split.first_frame_ms,
            split.warmup_ms,
            split.usable_ms,
            split.total_ms()
        );
        info!(
            iteration = i + 1,
            open_ms = split.open_ms,
            first_frame_ms = split.first_frame_ms,
            warmup_ms = split.warmup_ms,
            usable_ms = split.usable_ms,
            total_ms = split.total_ms(),
            "camera reopen iteration"
        );

        // Explicit, and load-bearing: the drop runs STREAMOFF and disables the
        // IR emitter, which is what makes the next iteration a cold open.
        drop(camera);
        splits.push(split);
    }

    let mut opens: Vec<u64> = splits.iter().map(|s| s.open_ms).collect();
    let mut firsts: Vec<u64> = splits.iter().map(|s| s.first_frame_ms).collect();
    let mut warmups: Vec<u64> = splits.iter().map(|s| s.warmup_ms).collect();
    let mut usables: Vec<u64> = splits.iter().map(|s| s.usable_ms).collect();
    let mut totals: Vec<u64> = splits.iter().map(|s| s.total_ms()).collect();

    println!();
    println!("Device:         {}", device_path);
    println!("Format:         {}", format);
    println!("Warmup frames:  {}", warmup_frames);
    println!();
    println!("Median split ({} iterations):", iterations);
    println!("  open:         {}ms", percentile(&mut opens, 50));
    println!("  first_frame:  {}ms", percentile(&mut firsts, 50));
    println!("  warmup:       {}ms", percentile(&mut warmups, 50));
    println!("  usable:       {}ms", percentile(&mut usables, 50));
    println!("  total:        {}ms", percentile(&mut totals, 50));
    println!();
    println!(
        "camera_release_secs trades this reopen cost against LED-on time after a failed attempt (ADR 008)"
    );

    Ok(())
}

fn cmd_report(config: &Config) -> Result<()> {
    let user = current_user();

    // Gather system info
    let hostname = hostname();
    let cpu_info = cpu_model();
    let os_info = os_release();

    println!("# Facelock Benchmark Report");
    println!();
    println!("## Environment");
    println!("- Hostname: {}", hostname);
    println!("- CPU: {}", cpu_info);
    println!("- OS: {}", os_info);
    println!(
        "- Camera: {}",
        config.device.path.as_deref().unwrap_or("(auto-detect)")
    );
    println!("- Model pack: SCRFD 2.5G + ArcFace R50");
    println!("- Build: release");
    println!("- User: {}", user);
    println!();

    // Model load benchmark
    let model_load_ms = {
        let start = Instant::now();
        let _engine = load_engine(config)?;
        start.elapsed().as_millis() as u64
    };

    // Open camera for subsequent benchmarks
    let mut engine = load_engine(config)?;
    let mut camera = open_camera(config)?;

    // Warm up
    let _ = camera.capture();

    // Preview benchmark
    let preview_ms = {
        let mut times = Vec::with_capacity(WARM_ITERATIONS as usize);
        for _ in 0..WARM_ITERATIONS {
            let start = Instant::now();
            let frame = camera.capture().context("Failed to capture frame")?;
            let _faces = Wiped::new(engine.process(&frame)?);
            times.push(start.elapsed().as_millis() as u64);
        }
        percentile(&mut times, 50)
    };

    // Warm auth benchmark
    let store = FaceStore::open_existing(Path::new(&config.storage.db_path))
        .context("Failed to open face store")?;
    // Decrypted templates (the CLI's decryption-aware load), zeroized when
    // the guard drops — early bails and panics included (#141).
    let embeddings = Wiped::new(direct::load_user_embeddings(&store, config, &user)?);
    let has_enrolled = !embeddings.is_empty();

    let warm_auth_ms = if has_enrolled {
        let mut times = Vec::with_capacity(WARM_ITERATIONS as usize);
        for _ in 0..WARM_ITERATIONS {
            let start = Instant::now();
            let frame = camera.capture().context("Failed to capture frame")?;
            let faces = Wiped::new(engine.process(&frame)?);
            let _matched = find_best_match(&faces, &embeddings, config.recognition.threshold);
            times.push(start.elapsed().as_millis() as u64);
        }
        Some(percentile(&mut times, 50))
    } else {
        None
    };

    // Cold auth (approximate: model load + one auth)
    let cold_auth_ms = if has_enrolled {
        let start = Instant::now();
        let mut cold_engine = load_engine(config)?;
        let frame = camera.capture().context("Failed to capture frame")?;
        let faces = Wiped::new(cold_engine.process(&frame)?);
        let _matched = find_best_match(&faces, &embeddings, config.recognition.threshold);
        Some(start.elapsed().as_millis() as u64)
    } else {
        None
    };

    // Enrollment benchmark (dry run)
    let enrollment_ms = {
        let start = Instant::now();
        for _ in 0..ENROLLMENT_SNAPSHOTS {
            let frame = camera.capture().context("Failed to capture frame")?;
            let _faces = Wiped::new(engine.process(&frame)?);
        }
        start.elapsed().as_millis() as u64
    };

    // Print results table
    println!("## Results");
    println!();
    println!(
        "| {:<25} | {:<10} | {:<10} | {:<6} |",
        "Metric", "Value", "Target", "Pass?"
    );
    println!("|{:-<27}|{:-<12}|{:-<12}|{:-<8}|", "", "", "", "");

    print_report_row("Model load", model_load_ms, TARGET_MODEL_LOAD_MS);
    print_report_row("Preview frame", preview_ms, TARGET_PREVIEW_MS);

    if let Some(ms) = warm_auth_ms {
        print_report_row("Warm auth", ms, TARGET_WARM_AUTH_MS);
    } else {
        println!(
            "| {:<25} | {:<10} | {:<10} | {:<6} |",
            "Warm auth",
            "N/A",
            format!("<{}ms", TARGET_WARM_AUTH_MS),
            "SKIP"
        );
    }

    if let Some(ms) = cold_auth_ms {
        print_report_row("Cold auth", ms, TARGET_COLD_AUTH_MS);
    } else {
        println!(
            "| {:<25} | {:<10} | {:<10} | {:<6} |",
            "Cold auth",
            "N/A",
            format!("<{}ms", TARGET_COLD_AUTH_MS),
            "SKIP"
        );
    }

    print_report_row(
        &format!("Enrollment ({} snapshots)", ENROLLMENT_SNAPSHOTS),
        enrollment_ms,
        TARGET_ENROLLMENT_MS,
    );

    println!();

    // Calibration summary
    println!("## Calibration");
    println!("- Current threshold: {:.2}", config.recognition.threshold);
    if has_enrolled {
        println!("- Run `facelock bench calibrate` for threshold sweep");
    } else {
        println!("- No enrolled faces; calibration skipped");
    }

    println!();
    println!("## Notes");
    println!(
        "- Preview and warm-auth are median of {} iterations",
        WARM_ITERATIONS
    );
    println!("- Cold auth includes model reload");
    if !has_enrolled {
        println!(
            "- Auth benchmarks skipped: no enrolled faces for user '{}'",
            user
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the best match between detected faces and enrolled embeddings.
/// Returns true if any face matches above the threshold.
fn find_best_match(
    faces: &[(facelock_core::types::Detection, FaceEmbedding)],
    enrolled: &[(u32, FaceEmbedding)],
    threshold: f32,
) -> bool {
    for (_, live_emb) in faces {
        for (_, stored_emb) in enrolled {
            let sim = cosine_similarity(live_emb, stored_emb);
            if sim >= threshold {
                return true;
            }
        }
    }
    false
}

/// Get the current username, preferring SUDO_USER when running under sudo.
fn current_user() -> String {
    std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Compute a percentile from a mutable slice (sorts in place).
fn percentile(values: &mut [u64], pct: u32) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort();
    let idx = ((pct as f64 / 100.0) * (values.len() - 1) as f64).round() as usize;
    values[idx.min(values.len() - 1)]
}

/// Format pass/fail string.
fn pass_fail(value_ms: u64, target_ms: u64) -> &'static str {
    if value_ms <= target_ms {
        "PASS"
    } else {
        "FAIL"
    }
}

/// Print a row in the report results table.
fn print_report_row(metric: &str, value_ms: u64, target_ms: u64) {
    println!(
        "| {:<25} | {:<10} | {:<10} | {:<6} |",
        metric,
        format!("{}ms", value_ms),
        format!("<{}ms", target_ms),
        pass_fail(value_ms, target_ms)
    );
}

/// Read hostname from /etc/hostname or fallback.
fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Read CPU model from /proc/cpuinfo.
fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|content| {
            content
                .lines()
                .find(|line| line.starts_with("model name"))
                .and_then(|line| line.split(':').nth(1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Read OS release info.
fn os_release() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|content| {
            content
                .lines()
                .find(|line| line.starts_with("PRETTY_NAME="))
                .map(|line| {
                    line.trim_start_matches("PRETTY_NAME=")
                        .trim_matches('"')
                        .to_string()
                })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentile_empty() {
        assert_eq!(percentile(&mut [], 50), 0);
    }

    #[test]
    fn test_percentile_single() {
        assert_eq!(percentile(&mut [42], 50), 42);
    }

    #[test]
    fn test_percentile_median() {
        let mut values = vec![10, 50, 30, 20, 40];
        assert_eq!(percentile(&mut values, 50), 30);
    }

    #[test]
    fn test_percentile_p95() {
        let mut values: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&mut values, 95), 95);
    }

    #[test]
    fn test_pass_fail() {
        assert_eq!(pass_fail(100, 200), "PASS");
        assert_eq!(pass_fail(200, 200), "PASS");
        assert_eq!(pass_fail(201, 200), "FAIL");
    }

    #[test]
    fn test_find_best_match_empty() {
        assert!(!find_best_match(&[], &[], 0.5));
    }

    #[test]
    fn test_find_best_match_above_threshold() {
        let emb = [0.1f32; 512];
        let faces = vec![(
            facelock_core::types::Detection {
                bbox: facelock_core::types::BoundingBox {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                confidence: 0.9,
                landmarks: [facelock_core::types::Point2D { x: 0.0, y: 0.0 }; 5],
            },
            emb,
        )];
        let enrolled = vec![(1u32, emb)];
        assert!(find_best_match(&faces, &enrolled, 0.5));
    }

    #[test]
    fn test_find_best_match_below_threshold() {
        let mut emb_a = [0.0f32; 512];
        let mut emb_b = [0.0f32; 512];
        emb_a[..256].fill(1.0 / (256.0f32).sqrt());
        emb_b[256..].fill(1.0 / (256.0f32).sqrt());
        let faces = vec![(
            facelock_core::types::Detection {
                bbox: facelock_core::types::BoundingBox {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                confidence: 0.9,
                landmarks: [facelock_core::types::Point2D { x: 0.0, y: 0.0 }; 5],
            },
            emb_a,
        )];
        let enrolled = vec![(1u32, emb_b)];
        assert!(!find_best_match(&faces, &enrolled, 0.5));
    }
}
