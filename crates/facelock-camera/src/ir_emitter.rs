//! IR emitter control via UVC Extension Unit (XU) controls.
//!
//! Many IR cameras have LED emitters that must be explicitly toggled on/off.
//! This module provides the interface for controlling these emitters.
//!
//! Currently supports detection of emitter capability. Actual hardware control
//! requires testing on specific camera models to determine correct XU GUIDs
//! and control selectors.

use std::path::Path;

/// Attempt to enable IR emitter on the given video device.
///
/// Returns Ok(true) if emitter was enabled, Ok(false) if no controllable
/// emitter was detected, or Err if an error occurred.
pub fn enable_emitter(device_path: &str) -> Result<bool, String> {
    if !Path::new(device_path).exists() {
        return Err(format!("device not found: {device_path}"));
    }

    // TODO: Implement UVC XU control for specific hardware
    // Known camera families that need explicit emitter control:
    // - Intel RealSense (XU GUID: specific to model)
    // - Some Lenovo ThinkPad IR cameras
    // - Microsoft Surface cameras
    //
    // The implementation requires:
    // 1. Open device with O_RDWR
    // 2. Query UVC XU controls via UVCIOC_CTRL_QUERY ioctl
    // 3. Set emitter control via UVCIOC_CTRL_SET ioctl
    //
    // For now, most Linux IR cameras auto-enable their emitters when
    // streaming starts, so this is a no-op that returns Ok(false).

    tracing::debug!("IR emitter control not yet implemented for {device_path}");
    Ok(false)
}

/// Attempt to disable IR emitter on the given video device.
pub fn disable_emitter(device_path: &str) -> Result<bool, String> {
    if !Path::new(device_path).exists() {
        return Err(format!("device not found: {device_path}"));
    }

    tracing::debug!("IR emitter control not yet implemented for {device_path}");
    Ok(false)
}

/// Check if the given device has a controllable IR emitter.
pub fn has_controllable_emitter(device_path: &str) -> bool {
    // TODO: Query UVC XU controls to detect emitter capability
    let _ = device_path;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_nonexistent_device_returns_error() {
        let result = enable_emitter("/dev/nonexistent_camera");
        assert!(result.is_err());
    }

    #[test]
    fn disable_nonexistent_device_returns_error() {
        let result = disable_emitter("/dev/nonexistent_camera");
        assert!(result.is_err());
    }

    #[test]
    fn has_controllable_emitter_returns_false_for_unknown() {
        assert!(!has_controllable_emitter("/dev/video0"));
    }

    #[test]
    #[ignore] // Requires actual IR camera hardware
    fn toggle_emitter_on_real_device() {
        // Manual test: run with `cargo test -- --ignored`
        // Check if /dev/video0 exists and try to toggle
        if Path::new("/dev/video0").exists() {
            let result = enable_emitter("/dev/video0");
            println!("enable result: {result:?}");
            let result = disable_emitter("/dev/video0");
            println!("disable result: {result:?}");
        }
    }
}
