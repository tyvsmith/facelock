//! `preview --json`: one JSON document per frame, on stdout, until Ctrl+C.
//!
//! **stdout here is the frame stream and nothing else.** `--json` is parsed one
//! level up, so neither loop below knows whether a human or `jq` is reading,
//! and both must keep stdout clean either way. The regression this rule exists
//! for shipped: the no-enrollment notice went through `Terminal::info`, which
//! writes to stdout, so a fresh oneshot install prefixed the stream with a
//! localized sentence and a consumer failed on the first line.
//!
//! Two checks enforce it, one per way in. `#![deny(clippy::print_stdout)]`
//! below refuses a bare `println!` — CI runs clippy with `-D warnings` — while
//! leaving the frame loops' `writeln!` alone. The seam's stdout sinks are
//! invisible to that lint, so `no_stdout_sink_of_the_seam_is_used_here`
//! scans this file for them; [`warn`] is the convenience that makes the right
//! thing the short thing, not a barrier, since a fully-qualified call to
//! `crate::message::Terminal`'s informational sink needs no import to compile
//! here.
//!
//! The frame stream is the one payload in the CLI that does not go through
//! `message::payload`: it runs until interrupted, so a `--quiet` that silenced
//! it would leave a command that produces nothing forever.
#![deny(clippy::print_stdout)]

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::ipc_client;
use crate::message::FaceMessage;

/// This module's route for human text: stderr, never stdout.
///
/// A convenience, not an enforcement — see the module docs for what actually
/// checks that the stdout sinks stay out of this file.
fn warn(msg: &dyn crate::message::Message) {
    crate::message::Terminal.error(msg);
}

/// Run the text-only preview mode.
///
/// Requests frames from the daemon and prints detection info as JSON lines
/// to stdout. Useful for SSH sessions, non-Wayland environments, and testing.
pub fn run(user: &str) -> anyhow::Result<()> {
    eprintln!("Text-only preview mode. Press Ctrl+C to stop.\n");

    let stop = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop));

    let mut frame_count: u64 = 0;
    let start = Instant::now();
    let mut last_fps_time = start;
    let mut fps_frame_count: u64 = 0;
    let mut current_fps: f32 = 0.0;

    let stdout = std::io::stdout();

    while !stop.load(Ordering::Relaxed) {
        // A daemon-side failure arrives as a transport error (the server
        // replies with a D-Bus error, never an in-band error for previews).
        let (jpeg_data, faces) = match ipc_client::preview_detect_frame(user) {
            Ok(frame) => frame,
            Err(e) => {
                eprintln!("{e:#}");
                break;
            }
        };

        frame_count += 1;
        fps_frame_count += 1;

        let now = Instant::now();
        let elapsed = now.duration_since(last_fps_time).as_secs_f32();
        if elapsed >= 1.0 {
            current_fps = fps_frame_count as f32 / elapsed;
            fps_frame_count = 0;
            last_fps_time = now;
        }

        let (width, height) = jpeg_dimensions(&jpeg_data);
        let output = frame_json(
            frame_count,
            current_fps,
            Some(jpeg_data.len()),
            width,
            height,
            faces
                .iter()
                .map(|f| {
                    face_json(
                        f.x,
                        f.y,
                        f.width,
                        f.height,
                        f.confidence,
                        f.similarity,
                        f.recognized,
                    )
                })
                .collect(),
        );

        let mut handle = stdout.lock();
        if writeln!(handle, "{output}").is_err() {
            break;
        }
    }

    let _ = ipc_client::release_camera();
    let _ = start;
    Ok(())
}

/// Direct text-only preview (oneshot mode, no daemon).
pub fn run_direct(config: &facelock_core::Config, user: &str) -> anyhow::Result<()> {
    use facelock_core::types::{Wiped, cosine_similarity};

    eprintln!("Text-only preview mode (direct). Press Ctrl+C to stop.\n");

    let stop = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop));

    let mut camera = crate::direct::open_camera(config)?;
    let mut engine = crate::direct::load_engine(config)?;
    // Preview only reads: it shows live similarity against whatever is
    // enrolled and never writes a row. The create-based `open_store` would
    // materialise an empty database on a fresh install — the silent lie about
    // a store of biometric templates that `open_existing` exists to prevent.
    // An absent database is simply "nothing enrolled yet", which preview can
    // still render: every face just comes back unrecognized.
    // Decrypted templates, zeroized when the guard drops — Ctrl+C return and
    // unwind alike. The set outlives the whole preview loop (loaded once, not
    // per frame), which is exactly why it must not outlive the function too.
    let stored =
        facelock_core::types::Wiped::new(match crate::direct::open_store_existing(config) {
            Ok(store) => crate::direct::load_user_embeddings(&store, config, user)?,
            Err(facelock_store::StoreError::Absent { .. }) => {
                // stderr, not stdout: this is the notice that used to corrupt
                // the frame stream (see the module docs).
                warn(&FaceMessage::NoModelsEnrolled {
                    user: user.to_string(),
                });
                Vec::new()
            }
            Err(e) => return Err(e.into()),
        });
    let threshold = config.recognition.threshold;

    let mut frame_count: u64 = 0;
    let start = Instant::now();
    let mut last_fps_time = start;
    let mut fps_frame_count: u64 = 0;
    let mut current_fps: f32 = 0.0;
    let stdout = std::io::stdout();

    while !stop.load(Ordering::Relaxed) {
        let frame = match camera.capture() {
            Ok(f) => f,
            Err(_) => continue,
        };
        frame_count += 1;
        fps_frame_count += 1;

        let now = Instant::now();
        let elapsed = now.duration_since(last_fps_time).as_secs_f32();
        if elapsed >= 1.0 {
            current_fps = fps_frame_count as f32 / elapsed;
            fps_frame_count = 0;
            last_fps_time = now;
        }

        // Per-frame embeddings, zeroized when `faces` drops (#305).
        let faces = Wiped::new(engine.process(&frame).unwrap_or_default());

        let faces_json: Vec<serde_json::Value> = faces
            .iter()
            .map(|(det, embedding)| {
                let mut best_sim: f32 = 0.0;
                for (_, stored_emb) in stored.iter() {
                    let sim = cosine_similarity(embedding, stored_emb);
                    if sim > best_sim {
                        best_sim = sim;
                    }
                }
                face_json(
                    det.bbox.x,
                    det.bbox.y,
                    det.bbox.width,
                    det.bbox.height,
                    det.confidence,
                    best_sim,
                    best_sim >= threshold,
                )
            })
            .collect();

        let output = frame_json(
            frame_count,
            current_fps,
            None,
            frame.width,
            frame.height,
            faces_json,
        );

        let mut handle = stdout.lock();
        if writeln!(handle, "{output}").is_err() {
            break;
        }
    }

    Ok(())
}

/// One face, as `--json` reports it.
///
/// The two loops above build a face row from different sources (a
/// `PreviewFace` off the bus; a `FaceDetection` plus a locally computed
/// similarity in direct mode), so the field set and the rounding live here
/// rather than twice. Coordinates are pixels in the original, pre-JPEG frame.
///
/// `confidence` and `similarity` are rounded to three decimals **in `f32`**,
/// and JSON numbers are `f64`, so the widening puts the trailing digits back:
/// `0.988f32` serializes as `0.9879999756813049`. That is what this has always
/// emitted and the pins below record it. A consumer compares numbers, never
/// the rendered text.
fn face_json(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    confidence: f32,
    similarity: f32,
    recognized: bool,
) -> serde_json::Value {
    serde_json::json!({
        "x": x, "y": y,
        "width": width, "height": height,
        "confidence": (confidence * 1000.0).round() / 1000.0,
        "similarity": (similarity * 1000.0).round() / 1000.0,
        "recognized": recognized,
    })
}

/// One frame, as `--json` emits it: a document per line on stdout.
///
/// `jpeg_size` is `Some` only on the daemon path, the only one holding a JPEG
/// to measure; the direct loop has never reported it, and this keeps that
/// difference stated instead of implied by two copies of the payload. The
/// counts are derived from `faces` rather than passed alongside them, so they
/// cannot disagree with the rows they summarize.
fn frame_json(
    frame: u64,
    fps: f32,
    jpeg_size: Option<usize>,
    width: u32,
    height: u32,
    faces: Vec<serde_json::Value>,
) -> serde_json::Value {
    let recognized = faces.iter().filter(|f| f["recognized"] == true).count();
    let mut output = serde_json::json!({
        "frame": frame,
        "fps": (fps * 10.0).round() / 10.0,
        "width": width,
        "height": height,
        "recognized": recognized,
        "unrecognized": faces.len() - recognized,
        "faces": faces,
    });
    if let Some(bytes) = jpeg_size {
        output["jpeg_size"] = serde_json::json!(bytes);
    }
    output
}

/// Try to extract JPEG dimensions without full decode.
fn jpeg_dimensions(data: &[u8]) -> (u32, u32) {
    match image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
    {
        Some((w, h)) => (w, h),
        None => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_dimensions_returns_zero_for_invalid() {
        let (w, h) = jpeg_dimensions(&[0, 1, 2, 3]);
        assert_eq!((w, h), (0, 0));
    }

    // Both loops need a daemon or a camera, so the payload is pinned one
    // level down, at the builders they share — the lowest seam here that
    // does no I/O. `preview --json` was `preview --text-only` and emitted
    // exactly these bytes; the rename must not have moved a key or a digit.

    #[test]
    fn daemon_frame_is_the_documented_json_line() {
        let frame = frame_json(
            7,
            15.24,
            Some(4096),
            640,
            480,
            vec![face_json(10.0, 20.0, 100.0, 120.0, 0.98765, 0.61234, true)],
        );

        assert_eq!(
            frame.to_string(),
            concat!(
                r#"{"faces":[{"confidence":0.9879999756813049,"height":120.0,"#,
                r#""recognized":true,"similarity":0.6119999885559082,"#,
                r#""width":100.0,"x":10.0,"y":20.0}],"#,
                r#""fps":15.199999809265137,"frame":7,"height":480,"#,
                r#""jpeg_size":4096,"recognized":1,"unrecognized":0,"width":640}"#,
            )
        );
    }

    /// The direct loop has no JPEG, so it reports no `jpeg_size`. Every other
    /// key is the same one, which is the half a consumer branches on.
    #[test]
    fn direct_frame_omits_jpeg_size_and_keeps_every_other_key() {
        let frame = frame_json(1, 0.0, None, 1280, 720, Vec::new());

        let object = frame.as_object().expect("a frame is a JSON object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "faces",
                "fps",
                "frame",
                "height",
                "recognized",
                "unrecognized",
                "width",
            ]
        );
    }

    /// The seam's stdout sinks, which no lint can see.
    ///
    /// `#![deny(clippy::print_stdout)]` catches a bare `println!`. It cannot
    /// catch either sink that writes to stdout through the seam: the
    /// informational one, which is what shipped as the bug, and the notice
    /// one, which `--quiet` does not even suppress. Neither needs an import
    /// to reach from here. So this scans the source, which is the only seam
    /// available — both frame loops need a daemon or a camera to run. The
    /// needles are assembled at runtime so the test does not match itself,
    /// following `backend.rs`'s single-siting pins.
    #[test]
    fn no_stdout_sink_of_the_seam_is_used_here() {
        let source = include_str!("text_only.rs");
        for sink in ["info", "notice"] {
            for sep in [".", "::"] {
                let needle = format!("Terminal{sep}{sink}(");
                assert!(
                    !source.contains(&needle),
                    "`{needle}` writes to stdout, which here is the frame \
                     stream; human text goes to stderr through `warn`"
                );
            }
        }
    }

    /// The counts summarize the rows, so they are read off them. A face row
    /// that says `recognized: false` cannot end up inside `recognized: 1`.
    #[test]
    fn counts_are_derived_from_the_face_rows() {
        let frame = frame_json(
            1,
            30.0,
            None,
            640,
            480,
            vec![
                face_json(0.0, 0.0, 1.0, 1.0, 0.9, 0.7, true),
                face_json(0.0, 0.0, 1.0, 1.0, 0.9, 0.1, false),
                face_json(0.0, 0.0, 1.0, 1.0, 0.9, 0.2, false),
            ],
        );

        assert_eq!(frame["recognized"], 1);
        assert_eq!(frame["unrecognized"], 2);
    }
}
