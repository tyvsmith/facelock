use facelock_core::error::{FacelockError, Result};
use v4l::Device;
use v4l::capability::Flags;
use v4l::framesize::FrameSizeEnum;
use v4l::video::Capture;

/// Information about a V4L2 video device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub path: String,
    pub name: String,
    pub driver: String,
    pub capabilities: Vec<String>,
    pub formats: Vec<FormatInfo>,
}

/// A supported pixel format with its available sizes.
#[derive(Debug, Clone)]
pub struct FormatInfo {
    pub fourcc: String,
    pub description: String,
    pub sizes: Vec<(u32, u32)>,
}

/// List all V4L2 video capture devices.
/// Returns an empty vec if no devices are found (does not error).
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mut devices = Vec::new();

    for i in 0..64 {
        let path = format!("/dev/video{i}");
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        match query_device(&path) {
            Ok(info) => devices.push(info),
            Err(e) => {
                tracing::debug!("skipping {path}: {e}");
                continue;
            }
        }
    }

    Ok(devices)
}

/// Validate that a specific device path is a usable video capture device.
pub fn validate_device(path: &str) -> Result<DeviceInfo> {
    query_device(path)
}

/// Heuristic: is this likely an IR camera?
/// Checks device name for "ir"/"infrared" or format list for GREY/Y16.
/// Also checks the hardware quirks database for `force_ir` overrides.
pub fn is_ir_camera(device: &DeviceInfo) -> bool {
    is_ir_camera_with_quirks(device, None)
}

/// Like `is_ir_camera` but accepts a quirks database for device-specific overrides.
pub fn is_ir_camera_with_quirks(
    device: &DeviceInfo,
    quirks: Option<&crate::quirks::QuirksDb>,
) -> bool {
    // Check quirks database first (most authoritative)
    if let Some(db) = quirks {
        if let Some(quirk) = db.find_match(device) {
            if let Some(force_ir) = quirk.force_ir {
                return force_ir;
            }
        }
    }

    // Fall back to heuristic detection
    let name_lower = device.name.to_lowercase();
    let has_ir_name = name_lower.contains("ir") || name_lower.contains("infrared");
    let has_ir_format = device
        .formats
        .iter()
        .any(|f| matches!(f.fourcc.as_str(), "GREY" | "Y16 "));
    has_ir_name || has_ir_format
}

/// Auto-detect the best available video capture device.
/// Prefers IR cameras, falls back to the first available device.
pub fn auto_detect_device() -> Result<DeviceInfo> {
    let devices = list_devices()?;
    devices
        .iter()
        .find(|d| is_ir_camera(d))
        .or(devices.first())
        .cloned()
        .ok_or_else(|| FacelockError::Camera("no video devices found".into()))
}

fn query_device(path: &str) -> Result<DeviceInfo> {
    let dev = Device::with_path(path).map_err(|e| FacelockError::Camera(format!("{path}: {e}")))?;

    let caps = dev
        .query_caps()
        .map_err(|e| FacelockError::Camera(format!("{path}: failed to query caps: {e}")))?;

    if !caps.capabilities.contains(Flags::VIDEO_CAPTURE) {
        return Err(FacelockError::Camera(format!(
            "{path}: not a video capture device"
        )));
    }

    let mut cap_strings = Vec::new();
    if caps.capabilities.contains(Flags::VIDEO_CAPTURE) {
        cap_strings.push("VIDEO_CAPTURE".to_string());
    }
    if caps.capabilities.contains(Flags::STREAMING) {
        cap_strings.push("STREAMING".to_string());
    }

    let mut formats = Vec::new();
    if let Ok(fmt_list) = dev.enum_formats() {
        for fmt in fmt_list {
            let fourcc = fmt.fourcc.to_string();
            let description = fmt.description.clone();
            let mut sizes = Vec::new();
            if let Ok(size_list) = dev.enum_framesizes(fmt.fourcc) {
                for fs in size_list {
                    match fs.size {
                        FrameSizeEnum::Discrete(d) => {
                            sizes.push((d.width, d.height));
                        }
                        FrameSizeEnum::Stepwise(s) => {
                            sizes.push((s.min_width, s.min_height));
                            sizes.push((s.max_width, s.max_height));
                        }
                    }
                }
            }
            formats.push(FormatInfo {
                fourcc,
                description,
                sizes,
            });
        }
    }

    Ok(DeviceInfo {
        path: path.to_string(),
        name: caps.card.clone(),
        driver: caps.driver.clone(),
        capabilities: cap_strings,
        formats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_ir_camera_grey_format() {
        let device = DeviceInfo {
            path: "/dev/video0".into(),
            name: "USB Camera".into(),
            driver: "uvcvideo".into(),
            capabilities: vec![],
            formats: vec![FormatInfo {
                fourcc: "GREY".into(),
                description: "Greyscale".into(),
                sizes: vec![(640, 480)],
            }],
        };
        assert!(is_ir_camera(&device));
    }

    #[test]
    fn is_ir_camera_mjpg_only() {
        let device = DeviceInfo {
            path: "/dev/video0".into(),
            name: "USB Camera".into(),
            driver: "uvcvideo".into(),
            capabilities: vec![],
            formats: vec![FormatInfo {
                fourcc: "MJPG".into(),
                description: "Motion JPEG".into(),
                sizes: vec![(640, 480)],
            }],
        };
        assert!(!is_ir_camera(&device));
    }

    #[test]
    fn is_ir_camera_infrared_name() {
        let device = DeviceInfo {
            path: "/dev/video0".into(),
            name: "Infrared Camera".into(),
            driver: "uvcvideo".into(),
            capabilities: vec![],
            formats: vec![FormatInfo {
                fourcc: "MJPG".into(),
                description: "Motion JPEG".into(),
                sizes: vec![(640, 480)],
            }],
        };
        assert!(is_ir_camera(&device));
    }

    #[test]
    fn is_ir_camera_y16_format() {
        let device = DeviceInfo {
            path: "/dev/video0".into(),
            name: "Depth Camera".into(),
            driver: "uvcvideo".into(),
            capabilities: vec![],
            formats: vec![FormatInfo {
                fourcc: "Y16 ".into(),
                description: "16-bit Greyscale".into(),
                sizes: vec![(640, 480)],
            }],
        };
        assert!(is_ir_camera(&device));
    }

    #[test]
    fn list_devices_does_not_crash() {
        // Should return Ok even if no devices exist
        let result = list_devices();
        assert!(result.is_ok());
    }
}
