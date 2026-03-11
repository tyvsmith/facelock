use facelock_core::config::DeviceConfig;
use facelock_core::error::{FacelockError, Result};
use facelock_core::traits::CameraSource;
use facelock_core::types::Frame;
use image::ImageReader;
use std::io::Cursor;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::Device;

use crate::preprocess;

/// A V4L2 camera for frame capture.
pub struct Camera<'a> {
    stream: Stream<'a>,
    width: u32,
    height: u32,
    format: String,
    #[allow(dead_code)]
    dark_threshold: f32,
    rotation: u16,
}

impl<'a> Camera<'a> {
    /// Open a camera device with the given configuration.
    /// If `config.path` is `None`, auto-detects the best available camera.
    pub fn open(config: &DeviceConfig) -> Result<Camera<'static>> {
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
        let formats = dev
            .enum_formats()
            .map_err(|e| FacelockError::Camera(format!("failed to enum formats: {e}")))?;

        let preferred = ["GREY", "YUYV", "MJPG"];
        let selected_fourcc = preferred
            .iter()
            .find_map(|&pref| {
                formats
                    .iter()
                    .find(|f| f.fourcc.to_string() == pref)
                    .map(|f| f.fourcc)
            })
            .or_else(|| formats.first().map(|f| f.fourcc))
            .ok_or_else(|| {
                FacelockError::Camera(format!("{device_path}: no supported formats"))
            })?;

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

        // Create MMAP stream with 4 buffers
        let stream = Stream::with_buffers(&dev, Type::VideoCapture, 4)
            .map_err(|e| FacelockError::Camera(format!("failed to create stream: {e}")))?;

        Ok(Camera {
            stream,
            width,
            height,
            format: format_str,
            dark_threshold: 0.4,
            rotation: config.rotation,
        })
    }

    /// Return the negotiated pixel format string (e.g. "MJPG", "YUYV", "GREY").
    pub fn format(&self) -> &str {
        &self.format
    }

    /// Capture a single frame with full preprocessing (RGB + grayscale + CLAHE).
    /// Use this for authentication and enrollment where face detection is needed.
    pub fn capture(&mut self) -> Result<Frame> {
        let (rgb, width, height) = self.capture_rgb()?;

        // Convert to grayscale and apply CLAHE for face detection
        let gray = preprocess::rgb_to_gray(&rgb, width, height);
        let gray = preprocess::clahe(&gray, width, height);

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
                let reader = ImageReader::with_format(
                    Cursor::new(buf),
                    image::ImageFormat::Jpeg,
                )
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
            let img = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| {
                FacelockError::Camera("failed to create image for resize".into())
            })?;
            let aspect = width as f64 / height as f64;
            let new_h = self.height;
            let new_w = (new_h as f64 * aspect) as u32;
            let resized = image::imageops::resize(
                &img,
                new_w,
                new_h,
                image::imageops::FilterType::Triangle,
            );
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
        };
        let mut cam = Camera::open(&config).expect("failed to open camera");
        let frame = cam.capture().expect("failed to capture frame");
        assert!(frame.width > 0);
        assert!(frame.height > 0);
        assert_eq!(frame.rgb.len(), (frame.width * frame.height * 3) as usize);
        assert_eq!(frame.gray.len(), (frame.width * frame.height) as usize);
    }
}
