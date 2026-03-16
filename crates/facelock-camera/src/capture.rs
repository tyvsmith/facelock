use facelock_core::config::DeviceConfig;
use facelock_core::error::{FacelockError, Result};
use facelock_core::traits::CameraSource;
use facelock_core::types::Frame;
use image::ImageReader;
use std::io::Cursor;
use std::time::Duration;
use v4l::Device;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;

/// Timeout for V4L2 DQBUF poll. If the camera doesn't produce a frame
/// within this time, capture returns an error instead of blocking forever.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);

use crate::ir_emitter;
use crate::ir_emitter::EmitterXuInfo;
use crate::preprocess;
use crate::quirks::Quirk;

/// A V4L2 camera for frame capture.
pub struct Camera<'a> {
    stream: Stream<'a>,
    width: u32,
    height: u32,
    format: String,
    rotation: u16,
    /// Device path, stored for IR emitter cleanup on drop.
    device_path: String,
    /// Whether the IR emitter was activated and should be disabled on drop.
    ir_emitter_active: bool,
    /// Emitter XU info, stored for disable on drop when the quirk ref is gone.
    emitter_xu_info: Option<EmitterXuInfo>,
}

impl<'a> Camera<'a> {
    /// Open a camera device with the given configuration.
    /// If `config.path` is `None`, auto-detects the best available camera.
    ///
    /// When a `quirk` is provided, its overrides take precedence:
    /// - `format_preference` is prepended to the format priority list.
    /// - `warmup_frames` replaces `config.warmup_frames`.
    /// - `rotation` replaces `config.rotation`.
    pub fn open(config: &DeviceConfig, quirk: Option<&Quirk>) -> Result<Camera<'static>> {
        let device_path = match config.path {
            Some(ref p) => p.clone(),
            None => {
                let info = crate::device::auto_detect_device()?;
                info.path
            }
        };

        let dev = Device::with_path(&device_path)
            .map_err(|e| FacelockError::Camera(format!("failed to open {device_path}: {e}")))?;

        // Verify VIDEO_CAPTURE capability
        let caps = dev.query_caps().map_err(|e| {
            FacelockError::Camera(format!("failed to query caps for {device_path}: {e}"))
        })?;
        if !caps
            .capabilities
            .contains(v4l::capability::Flags::VIDEO_CAPTURE)
        {
            return Err(FacelockError::Camera(format!(
                "{device_path}: not a video capture device",
            )));
        }

        // Select format: prefer GREY > YUYV > MJPG > any
        // If a quirk specifies a format_preference, prepend it to the priority list.
        let formats = dev
            .enum_formats()
            .map_err(|e| FacelockError::Camera(format!("failed to enum formats: {e}")))?;

        let default_preferred: &[&str] = &["GREY", "YUYV", "MJPG"];
        let quirk_fmt = quirk.and_then(|q| q.format_preference.as_deref());
        let mut preferred: Vec<&str> = Vec::with_capacity(4);
        if let Some(fmt_pref) = quirk_fmt {
            tracing::debug!(format = fmt_pref, "quirk: prepending format preference");
            preferred.push(fmt_pref);
        }
        preferred.extend_from_slice(default_preferred);

        let selected_fourcc = preferred
            .iter()
            .find_map(|&pref| {
                formats
                    .iter()
                    .find(|f| f.fourcc.to_string().trim() == pref.trim())
                    .map(|f| f.fourcc)
            })
            .or_else(|| formats.first().map(|f| f.fourcc))
            .ok_or_else(|| FacelockError::Camera(format!("{device_path}: no supported formats")))?;

        // Set format with resolution capped at 640x480, respecting max_height
        let max_h = config.max_height.min(480);
        let max_w = 640u32;

        let mut fmt = dev
            .format()
            .map_err(|e| FacelockError::Camera(format!("failed to get format: {e}")))?;
        fmt.fourcc = selected_fourcc;
        fmt.width = max_w;
        fmt.height = max_h;
        let fmt = dev
            .set_format(&fmt)
            .map_err(|e| FacelockError::Camera(format!("failed to set format: {e}")))?;

        let width = fmt.width;
        let height = fmt.height;
        let format_str = fmt.fourcc.to_string();

        // Create MMAP stream with 4 buffers and a capture timeout
        let mut stream = Stream::with_buffers(&dev, Type::VideoCapture, 4)
            .map_err(|e| FacelockError::Camera(format!("failed to create stream: {e}")))?;
        stream.set_timeout(CAPTURE_TIMEOUT);

        // Extract emitter XU info from quirk (if available) for use during
        // enable and later in Drop.
        let emitter_xu_info = quirk.and_then(EmitterXuInfo::from_quirk);

        // Attempt to enable IR emitter if configured
        let ir_emitter_active = if config.ir_emitter {
            match ir_emitter::enable_emitter(&device_path, quirk) {
                Ok(true) => {
                    tracing::info!("IR emitter enabled on {device_path}");
                    true
                }
                Ok(false) => {
                    tracing::debug!("no controllable IR emitter on {device_path}");
                    false
                }
                Err(e) => {
                    tracing::warn!("failed to enable IR emitter on {device_path}: {e}");
                    false
                }
            }
        } else {
            false
        };

        // Apply quirk overrides for rotation (warmup_frames is handled by the caller
        // since Camera::open doesn't consume warmup frames itself).
        let rotation = quirk
            .and_then(|q| q.rotation)
            .unwrap_or(config.rotation);
        if let Some(q) = quirk {
            if q.rotation.is_some() || q.warmup_frames.is_some() || q.format_preference.is_some() {
                tracing::info!(
                    rotation = ?q.rotation,
                    warmup_frames = ?q.warmup_frames,
                    format_preference = ?q.format_preference,
                    "applied quirk overrides for {device_path}"
                );
            }
        }

        Ok(Camera {
            stream,
            width,
            height,
            format: format_str,
            rotation,
            device_path,
            ir_emitter_active,
            emitter_xu_info,
        })
    }

    /// Return the negotiated pixel format string (e.g. "MJPG", "YUYV", "GREY").
    pub fn format(&self) -> &str {
        &self.format
    }

    /// Capture a single frame with preprocessing (RGB + raw grayscale).
    /// CLAHE is not applied here — callers that need it (e.g. IR texture checks)
    /// should run `preprocess::clahe()` on `frame.gray` themselves.
    pub fn capture(&mut self) -> Result<Frame> {
        let (rgb, width, height) = self.capture_rgb()?;

        // Convert to grayscale (no CLAHE — deferred to callers that need it)
        let gray = preprocess::rgb_to_gray(&rgb, width, height);

        Ok(Frame {
            rgb,
            gray,
            width,
            height,
        })
    }

    /// Capture a frame and return only the RGB data, skipping grayscale
    /// conversion and CLAHE. Use this for preview where no face detection runs.
    pub fn capture_rgb_only(&mut self) -> Result<Frame> {
        let (rgb, width, height) = self.capture_rgb()?;

        Ok(Frame {
            rgb,
            gray: Vec::new(),
            width,
            height,
        })
    }

    /// Internal: capture and convert to RGB, applying downscale and rotation.
    fn capture_rgb(&mut self) -> Result<(Vec<u8>, u32, u32)> {
        // stream.next() uses the v4l built-in poll with CAPTURE_TIMEOUT.
        // If the camera stops producing frames, this returns TimedOut error
        // instead of blocking forever.
        let (buf, _meta) = self
            .stream
            .next()
            .map_err(|e| FacelockError::Camera(format!("capture failed: {e}")))?;

        // Convert to RGB based on format
        let rgb: Vec<u8> = match self.format.as_str() {
            "GREY" => {
                // Replicate single channel 3x
                let mut rgb = Vec::with_capacity(buf.len() * 3);
                for &p in buf {
                    rgb.push(p);
                    rgb.push(p);
                    rgb.push(p);
                }
                rgb
            }
            "YUYV" => preprocess::yuyv_to_rgb(buf, self.width, self.height),
            "MJPG" => {
                let reader = ImageReader::with_format(Cursor::new(buf), image::ImageFormat::Jpeg)
                    .decode()
                    .map_err(|e| FacelockError::Camera(format!("MJPG decode failed: {e}")))?;
                reader.to_rgb8().into_raw()
            }
            other => {
                return Err(FacelockError::Camera(format!(
                    "unsupported format: {other}"
                )));
            }
        };

        let mut width = self.width;
        let mut height = self.height;
        let mut rgb = rgb;

        // Downscale if needed
        if height > self.height {
            let img = image::RgbImage::from_raw(width, height, rgb)
                .ok_or_else(|| FacelockError::Camera("failed to create image for resize".into()))?;
            let aspect = width as f64 / height as f64;
            let new_h = self.height;
            let new_w = (new_h as f64 * aspect) as u32;
            let resized =
                image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Triangle);
            width = new_w;
            height = new_h;
            rgb = resized.into_raw();
        }

        // Apply rotation
        if self.rotation != 0 {
            let img = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| {
                FacelockError::Camera("failed to create image for rotation".into())
            })?;
            let rotated = match self.rotation {
                90 => image::imageops::rotate90(&img),
                180 => image::imageops::rotate180(&img),
                270 => image::imageops::rotate270(&img),
                _ => img,
            };
            width = rotated.width();
            height = rotated.height();
            rgb = rotated.into_raw();
        }

        Ok((rgb, width, height))
    }

    /// Check if a frame is too dark using default thresholds.
    pub fn is_dark(frame: &Frame) -> bool {
        is_dark_with_config(frame, 0.6, 10)
    }
}

/// Check if a frame is too dark to process.
///
/// Uses both a per-pixel threshold check and mean brightness:
/// - If the fraction of pixels below `dark_value` exceeds `threshold`, the frame is dark.
/// - If the mean brightness is below 20.0, the frame is dark (catches uniformly dim frames).
pub fn is_dark_with_config(frame: &Frame, threshold: f32, dark_value: u8) -> bool {
    if frame.gray.is_empty() {
        return false;
    }
    // Per-pixel dark ratio
    let dark_count = frame.gray.iter().filter(|&&p| p < dark_value).count();
    let dark_ratio = dark_count as f32 / frame.gray.len() as f32;
    if dark_ratio >= threshold {
        return true;
    }
    // Mean brightness check (catches uniformly dim frames)
    let sum: u64 = frame.gray.iter().map(|&p| p as u64).sum();
    let mean = sum as f32 / frame.gray.len() as f32;
    mean < 20.0
}

impl Drop for Camera<'_> {
    fn drop(&mut self) {
        if self.ir_emitter_active {
            if let Some(ref xu_info) = self.emitter_xu_info {
                match ir_emitter::disable_emitter_with_info(&self.device_path, xu_info) {
                    Ok(()) => tracing::debug!("IR emitter disabled on {}", self.device_path),
                    Err(e) => {
                        tracing::warn!(
                            "failed to disable IR emitter on {}: {e}",
                            self.device_path
                        )
                    }
                }
            }
        }
    }
}

impl CameraSource for Camera<'_> {
    fn capture(&mut self) -> Result<Frame> {
        Camera::capture(self)
    }

    fn capture_rgb_only(&mut self) -> Result<Frame> {
        Camera::capture_rgb_only(self)
    }

    fn is_dark(frame: &Frame) -> bool {
        Camera::is_dark(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_dark_all_black() {
        let frame = Frame {
            rgb: vec![0u8; 64 * 64 * 3],
            gray: vec![0u8; 64 * 64],
            width: 64,
            height: 64,
        };
        assert!(Camera::is_dark(&frame));
    }

    #[test]
    fn is_dark_all_white() {
        let frame = Frame {
            rgb: vec![255u8; 64 * 64 * 3],
            gray: vec![255u8; 64 * 64],
            width: 64,
            height: 64,
        };
        assert!(!Camera::is_dark(&frame));
    }

    #[test]
    fn all_black_frame_is_dark_with_config() {
        let frame = Frame {
            rgb: vec![0; 30],
            gray: vec![0; 10],
            width: 5,
            height: 2,
        };
        assert!(is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn bright_frame_is_not_dark_with_config() {
        let frame = Frame {
            rgb: vec![128; 30],
            gray: vec![128; 10],
            width: 5,
            height: 2,
        };
        assert!(!is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn custom_threshold_below_cutoff() {
        // 50% dark pixels (5/10), 60% threshold = not dark by ratio
        // but mean = (5*5 + 5*128)/10 = 66.5 > 20 so not dark by mean either
        let mut gray = vec![128u8; 10];
        for p in gray.iter_mut().take(5) {
            *p = 5;
        }
        let frame = Frame {
            rgb: vec![0; 30],
            gray,
            width: 5,
            height: 2,
        };
        assert!(!is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn custom_threshold_above_cutoff() {
        // 70% dark pixels (7/10), 60% threshold = dark by ratio
        let mut gray = vec![128u8; 10];
        for p in gray.iter_mut().take(7) {
            *p = 5;
        }
        let frame = Frame {
            rgb: vec![0; 30],
            gray,
            width: 5,
            height: 2,
        };
        assert!(is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn dim_frame_caught_by_mean_brightness() {
        // All pixels at 15 (above dark_value=10) so ratio=0, but mean=15 < 20
        let frame = Frame {
            rgb: vec![0; 30],
            gray: vec![15; 10],
            width: 5,
            height: 2,
        };
        assert!(is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    fn empty_gray_is_not_dark_with_config() {
        let frame = Frame {
            rgb: vec![0; 30],
            gray: vec![],
            width: 5,
            height: 2,
        };
        assert!(!is_dark_with_config(&frame, 0.6, 10));
    }

    #[test]
    #[ignore]
    fn camera_open_and_capture() {
        let config = DeviceConfig {
            path: Some("/dev/video0".into()),
            max_height: 480,
            rotation: 0,
            warmup_frames: 5,
            dark_threshold: 0.6,
            dark_pixel_value: 10,
            ir_emitter: false,
            camera_release_secs: 5,
        };
        let mut cam = Camera::open(&config, None).expect("failed to open camera");
        let frame = cam.capture().expect("failed to capture frame");
        assert!(frame.width > 0);
        assert!(frame.height > 0);
        assert_eq!(frame.rgb.len(), (frame.width * frame.height * 3) as usize);
        assert_eq!(frame.gray.len(), (frame.width * frame.height) as usize);
    }
}
