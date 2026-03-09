use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tracing::{info, warn};

use howdy_camera::capture::Camera;
use howdy_camera::device::{is_ir_camera, validate_device};
use howdy_core::Config;
use howdy_core::types::{cosine_similarity, FaceEmbedding};
use howdy_face::FaceEngine;
use howdy_store::FaceStore;

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

#[derive(Parser)]
#[command(name = "howdy-bench", about = "Benchmark and calibration tool for howdy")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    /// Generate a benchmark report
    Report,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Command::ColdAuth => cmd_cold_auth(),
        Command::WarmAuth => cmd_warm_auth(),
        Command::Preview => cmd_preview(),
        Command::Enrollment => cmd_enrollment(),
        Command::ModelLoad => cmd_model_load(),
        Command::Calibrate => cmd_calibrate(),
        Command::Report => cmd_report(),
    }
}

/// Load config, returning a helpful error if it fails.
fn load_config() -> Result<Config> {
    Config::load().context(
        "Failed to load config. Set HOWDY_CONFIG env var or ensure /etc/howdy/config.toml exists.",
    )
}

/// Resolve the model directory from config.
fn model_dir(config: &Config) -> &Path {
    Path::new(&config.daemon.model_dir)
}

/// Open the camera from config.
fn open_camera(config: &Config) -> Result<Camera<'static>> {
    let device_info = validate_device(&config.device.path)
        .with_context(|| format!("Camera device {} not accessible", config.device.path))?;
    info!(device = %config.device.path, ir = is_ir_camera(&device_info), "Camera validated");
    Camera::open(&config.device).context("Failed to open camera")
}

/// Load FaceEngine from config.
fn load_engine(config: &Config) -> Result<FaceEngine> {
    FaceEngine::load(&config.recognition, model_dir(config))
        .context("Failed to load face engine. Ensure models are installed.")
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

fn cmd_cold_auth() -> Result<()> {
    let config = load_config()?;
    let user = current_user();

    println!("=== Cold Auth Benchmark ===");
    println!("Measuring: config load + model load + camera open + first auth attempt");
    println!();

    let start = Instant::now();

    // Load models (cold)
    let mut engine = load_engine(&config)?;

    // Open camera
    let mut camera = open_camera(&config)?;

    // Open store
    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("Failed to open face store")?;

    let embeddings = store.get_user_embeddings(&user)?;
    if embeddings.is_empty() {
        bail!(
            "No enrolled faces for user '{}'. Enroll first with `howdy add`.",
            user
        );
    }

    // Capture and match
    let frame = camera.capture().context("Failed to capture frame")?;
    let faces = engine.process(&frame)?;

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;

    let matched = find_best_match(&faces, &embeddings, config.recognition.threshold);

    println!("Cold auth time:  {}ms", elapsed_ms);
    println!("Target:          <{}ms", TARGET_COLD_AUTH_MS);
    println!(
        "Result:          {}",
        pass_fail(elapsed_ms, TARGET_COLD_AUTH_MS)
    );
    println!("Faces detected:  {}", faces.len());
    println!(
        "Auth result:     {}",
        if matched { "MATCH" } else { "NO MATCH" }
    );

    Ok(())
}

fn cmd_warm_auth() -> Result<()> {
    let config = load_config()?;
    let user = current_user();

    println!("=== Warm Auth Benchmark ===");
    println!(
        "Measuring: auth latency with pre-loaded models ({} iterations)",
        WARM_ITERATIONS
    );
    println!();

    // Pre-load everything
    let mut engine = load_engine(&config)?;
    let mut camera = open_camera(&config)?;
    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("Failed to open face store")?;

    let embeddings = store.get_user_embeddings(&user)?;
    if embeddings.is_empty() {
        bail!(
            "No enrolled faces for user '{}'. Enroll first with `howdy add`.",
            user
        );
    }

    // Warm up with one capture
    let _ = camera.capture();

    let mut times = Vec::with_capacity(WARM_ITERATIONS as usize);

    for i in 0..WARM_ITERATIONS {
        let start = Instant::now();
        let frame = camera.capture().context("Failed to capture frame")?;
        let faces = engine.process(&frame)?;
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

fn cmd_preview() -> Result<()> {
    let config = load_config()?;

    println!("=== Preview Frame Benchmark ===");
    println!(
        "Measuring: frame capture + face detection ({} iterations)",
        WARM_ITERATIONS
    );
    println!();

    let mut engine = load_engine(&config)?;
    let mut camera = open_camera(&config)?;

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
        let _faces = engine.process(&frame)?;
        let det_ms = det_start.elapsed().as_millis() as u64;

        let total_ms = total_start.elapsed().as_millis() as u64;

        capture_times.push(cap_ms);
        detect_times.push(det_ms);
        total_times.push(total_ms);
        info!(iteration = i + 1, cap_ms, det_ms, total_ms, "preview iteration");
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

fn cmd_enrollment() -> Result<()> {
    let config = load_config()?;

    println!("=== Enrollment Benchmark ===");
    println!(
        "Measuring: time to capture and embed {} snapshots",
        ENROLLMENT_SNAPSHOTS
    );
    println!("NOTE: embeddings are NOT stored (dry run)");
    println!();

    let mut engine = load_engine(&config)?;
    let mut camera = open_camera(&config)?;

    // Warm up
    let _ = camera.capture();

    let start = Instant::now();
    let mut face_count = 0u32;

    for i in 0..ENROLLMENT_SNAPSHOTS {
        let frame = camera.capture().context("Failed to capture frame")?;
        let faces = engine.process(&frame)?;
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

fn cmd_model_load() -> Result<()> {
    let config = load_config()?;

    println!("=== Model Load Benchmark ===");
    println!("Measuring: ONNX model load time (SCRFD + ArcFace)");
    println!();

    let iterations = 3u32;
    let mut times = Vec::with_capacity(iterations as usize);

    for i in 0..iterations {
        let start = Instant::now();
        let _engine = load_engine(&config)?;
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

fn cmd_calibrate() -> Result<()> {
    let config = load_config()?;
    let user = current_user();

    println!("=== Threshold Calibration ===");
    println!("Sweeping recognition.threshold from 0.20 to 0.80 (step 0.05)");
    println!();

    let mut engine = load_engine(&config)?;
    let mut camera = open_camera(&config)?;
    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("Failed to open face store")?;

    let enrolled = store.get_user_embeddings(&user)?;
    if enrolled.is_empty() {
        bail!(
            "No enrolled faces for user '{}'. Enroll first with `howdy add`.",
            user
        );
    }

    // Capture several test frames to get live embeddings
    let num_test_frames = 10u32;
    println!(
        "Capturing {} test frames from camera...",
        num_test_frames
    );

    let mut live_embeddings: Vec<FaceEmbedding> = Vec::new();
    for _ in 0..num_test_frames {
        let frame = camera.capture().context("Failed to capture frame")?;
        let faces = engine.process(&frame)?;
        for (_, emb) in &faces {
            live_embeddings.push(*emb);
        }
    }

    if live_embeddings.is_empty() {
        bail!("No faces detected in test frames. Ensure you are facing the camera.");
    }

    println!("Captured {} face embeddings from live frames", live_embeddings.len());
    println!();

    // Compute all similarities between live and enrolled embeddings
    let mut similarities: Vec<f32> = Vec::new();
    for live_emb in &live_embeddings {
        for (_, enrolled_emb) in &enrolled {
            similarities.push(cosine_similarity(live_emb, enrolled_emb));
        }
    }

    let avg_sim = similarities.iter().sum::<f32>() / similarities.len().max(1) as f32;
    let max_sim = similarities.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let min_sim = similarities.iter().cloned().fold(f32::INFINITY, f32::min);

    println!("Similarity stats: min={:.4} avg={:.4} max={:.4}", min_sim, avg_sim, max_sim);
    println!();

    // Threshold sweep
    println!(
        "{:<12} {:<10} {:<12}",
        "Threshold", "Matches", "Match Rate"
    );
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
    println!(
        "{:<12} {:<15}",
        "Confidence", "Detections"
    );
    println!("{}", "-".repeat(27));

    // Re-capture a single frame for detection sweep
    let frame = camera.capture().context("Failed to capture frame")?;

    let mut conf = 0.30f32;
    while conf <= 0.90 + f32::EPSILON {
        let confidence = (conf * 100.0).round() / 100.0;
        let mut sweep_config = config.recognition.clone();
        sweep_config.detection_confidence = confidence;

        match FaceEngine::load(&sweep_config, model_dir(&config)) {
            Ok(mut sweep_engine) => {
                match sweep_engine.process(&frame) {
                    Ok(faces) => {
                        println!("{:<12.2} {:<15}", confidence, faces.len());
                    }
                    Err(e) => {
                        println!("{:<12.2} error: {}", confidence, e);
                    }
                }
            }
            Err(e) => {
                println!("{:<12.2} load error: {}", confidence, e);
            }
        }

        conf += 0.10;
    }

    Ok(())
}

fn cmd_report() -> Result<()> {
    let config = load_config()?;
    let user = current_user();

    // Gather system info
    let hostname = hostname();
    let cpu_info = cpu_model();
    let os_info = os_release();

    println!("# Howdy Benchmark Report");
    println!();
    println!("## Environment");
    println!("- Hostname: {}", hostname);
    println!("- CPU: {}", cpu_info);
    println!("- OS: {}", os_info);
    println!("- Camera: {}", config.device.path);
    println!("- Model pack: SCRFD 2.5G + ArcFace R50");
    println!("- Build: release");
    println!("- User: {}", user);
    println!();

    // Model load benchmark
    let model_load_ms = {
        let start = Instant::now();
        let _engine = load_engine(&config)?;
        start.elapsed().as_millis() as u64
    };

    // Open camera for subsequent benchmarks
    let mut engine = load_engine(&config)?;
    let mut camera = open_camera(&config)?;

    // Warm up
    let _ = camera.capture();

    // Preview benchmark
    let preview_ms = {
        let mut times = Vec::with_capacity(WARM_ITERATIONS as usize);
        for _ in 0..WARM_ITERATIONS {
            let start = Instant::now();
            let frame = camera.capture().context("Failed to capture frame")?;
            let _faces = engine.process(&frame)?;
            times.push(start.elapsed().as_millis() as u64);
        }
        percentile(&mut times, 50)
    };

    // Warm auth benchmark
    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("Failed to open face store")?;
    let embeddings = store.get_user_embeddings(&user)?;
    let has_enrolled = !embeddings.is_empty();

    let warm_auth_ms = if has_enrolled {
        let mut times = Vec::with_capacity(WARM_ITERATIONS as usize);
        for _ in 0..WARM_ITERATIONS {
            let start = Instant::now();
            let frame = camera.capture().context("Failed to capture frame")?;
            let faces = engine.process(&frame)?;
            let _matched =
                find_best_match(&faces, &embeddings, config.recognition.threshold);
            times.push(start.elapsed().as_millis() as u64);
        }
        Some(percentile(&mut times, 50))
    } else {
        None
    };

    // Cold auth (approximate: model load + one auth)
    let cold_auth_ms = if has_enrolled {
        let start = Instant::now();
        let mut cold_engine = load_engine(&config)?;
        let frame = camera.capture().context("Failed to capture frame")?;
        let faces = cold_engine.process(&frame)?;
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
            let _faces = engine.process(&frame)?;
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
    println!(
        "|{:-<27}|{:-<12}|{:-<12}|{:-<8}|",
        "", "", "", ""
    );

    print_report_row("Model load", model_load_ms, TARGET_MODEL_LOAD_MS);
    print_report_row("Preview frame", preview_ms, TARGET_PREVIEW_MS);

    if let Some(ms) = warm_auth_ms {
        print_report_row("Warm auth", ms, TARGET_WARM_AUTH_MS);
    } else {
        println!(
            "| {:<25} | {:<10} | {:<10} | {:<6} |",
            "Warm auth", "N/A", format!("<{}ms", TARGET_WARM_AUTH_MS), "SKIP"
        );
    }

    if let Some(ms) = cold_auth_ms {
        print_report_row("Cold auth", ms, TARGET_COLD_AUTH_MS);
    } else {
        println!(
            "| {:<25} | {:<10} | {:<10} | {:<6} |",
            "Cold auth", "N/A", format!("<{}ms", TARGET_COLD_AUTH_MS), "SKIP"
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
    println!(
        "- Current threshold: {:.2}",
        config.recognition.threshold
    );
    if has_enrolled {
        println!("- Run `howdy-bench calibrate` for threshold sweep");
    } else {
        println!("- No enrolled faces; calibration skipped");
    }

    println!();
    println!("## Notes");
    println!("- Preview and warm-auth are median of {} iterations", WARM_ITERATIONS);
    println!("- Cold auth includes model reload");
    if !has_enrolled {
        println!("- Auth benchmarks skipped: no enrolled faces for user '{}'", user);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the best match between detected faces and enrolled embeddings.
/// Returns true if any face matches above the threshold.
fn find_best_match(
    faces: &[(howdy_core::types::Detection, FaceEmbedding)],
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

/// Get the current username.
fn current_user() -> String {
    std::env::var("USER")
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
            howdy_core::types::Detection {
                bbox: howdy_core::types::BoundingBox {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                confidence: 0.9,
                landmarks: [howdy_core::types::Point2D { x: 0.0, y: 0.0 }; 5],
            },
            emb,
        )];
        let enrolled = vec![(1u32, emb)];
        // Identical embeddings => similarity = 1.0 (after dot product of identical vectors)
        assert!(find_best_match(&faces, &enrolled, 0.5));
    }

    #[test]
    fn test_find_best_match_below_threshold() {
        let mut emb_a = [0.0f32; 512];
        let mut emb_b = [0.0f32; 512];
        // Orthogonal embeddings
        for i in 0..256 {
            emb_a[i] = 1.0 / (256.0f32).sqrt();
        }
        for i in 256..512 {
            emb_b[i] = 1.0 / (256.0f32).sqrt();
        }
        let faces = vec![(
            howdy_core::types::Detection {
                bbox: howdy_core::types::BoundingBox {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                confidence: 0.9,
                landmarks: [howdy_core::types::Point2D { x: 0.0, y: 0.0 }; 5],
            },
            emb_a,
        )];
        let enrolled = vec![(1u32, emb_b)];
        assert!(!find_best_match(&faces, &enrolled, 0.5));
    }
}
