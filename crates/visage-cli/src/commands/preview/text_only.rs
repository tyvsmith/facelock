use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use visage_core::ipc::{DaemonRequest, DaemonResponse};

use crate::ipc_client;

/// Run the text-only preview mode.
///
/// Requests frames from the daemon and prints detection info as JSON lines
/// to stdout. Useful for SSH sessions, non-Wayland environments, and testing.
pub fn run(socket_path: &str, user: &str) -> anyhow::Result<()> {
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
        let response = match ipc_client::send_request(
            socket_path,
            &DaemonRequest::PreviewDetectFrame {
                user: user.to_string(),
            },
        ) {
            Ok(r) => r,
            Err(_) => break,
        };

        match response {
            DaemonResponse::DetectFrame { jpeg_data, faces } => {
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
                let recognized = faces.iter().filter(|f| f.recognized).count();
                let unrecognized = faces.len() - recognized;

                let output = serde_json::json!({
                    "frame": frame_count,
                    "fps": (current_fps * 10.0).round() / 10.0,
                    "jpeg_size": jpeg_data.len(),
                    "width": width,
                    "height": height,
                    "recognized": recognized,
                    "unrecognized": unrecognized,
                    "faces": faces.iter().map(|f| serde_json::json!({
                        "x": f.x, "y": f.y,
                        "width": f.width, "height": f.height,
                        "confidence": (f.confidence * 1000.0).round() / 1000.0,
                        "similarity": (f.similarity * 1000.0).round() / 1000.0,
                        "recognized": f.recognized,
                    })).collect::<Vec<_>>(),
                });

                let mut handle = stdout.lock();
                if writeln!(handle, "{output}").is_err() {
                    break;
                }
            }
            DaemonResponse::Frame { jpeg_data } => {
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
                let output = serde_json::json!({
                    "frame": frame_count,
                    "fps": (current_fps * 10.0).round() / 10.0,
                    "jpeg_size": jpeg_data.len(),
                    "width": width,
                    "height": height,
                    "recognized": 0,
                    "unrecognized": 0,
                    "faces": [],
                });

                let mut handle = stdout.lock();
                if writeln!(handle, "{output}").is_err() {
                    break;
                }
            }
            DaemonResponse::Error { message } => {
                let _ = ipc_client::send_request(socket_path, &DaemonRequest::ReleaseCamera);
                anyhow::bail!("daemon error: {message}");
            }
            other => {
                tracing::warn!("unexpected response from daemon: {other:?}");
            }
        }
    }

    let _ = ipc_client::send_request(socket_path, &DaemonRequest::ReleaseCamera);
    let _ = start;
    Ok(())
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
}
