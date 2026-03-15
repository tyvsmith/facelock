//! IR emitter control via UVC Extension Unit (XU) controls.
//!
//! Many IR cameras have LED emitters that must be explicitly toggled on/off.
//! This module provides the interface for controlling these emitters using
//! the Linux UVC driver's `UVCIOC_CTRL_QUERY` ioctl.
//!
//! The quirks database supplies per-device XU GUID and selector values.

use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::quirks::Quirk;

// UVC XU query types (from linux/usb/video.h)
const UVC_GET_CUR: u8 = 0x81;
const UVC_SET_CUR: u8 = 0x01;

// UVCIOC_CTRL_QUERY ioctl number.
// Defined in linux/uvcvideo.h as _IOWR('u', 0x21, struct uvc_xu_control_query)
// Direction: read+write (0xC0), size: 0x10 (16 bytes), type: 'u' (0x75), nr: 0x21
// = 0xC010_7521
const UVCIOC_CTRL_QUERY: libc::c_ulong = 0xC010_7521;

/// Mirrors `struct uvc_xu_control_query` from linux/uvcvideo.h.
#[repr(C)]
struct UvcXuControlQuery {
    unit: u8,
    selector: u8,
    query: u8,
    size: u16,
    data: *mut u8,
}

/// Emitter XU parameters extracted from a quirk, suitable for storage
/// in structs that outlive the quirk reference.
#[derive(Debug, Clone)]
pub struct EmitterXuInfo {
    /// The UVC Extension Unit ID (parsed from the GUID via sysfs lookup).
    /// For now we use the XU unit byte directly from the GUID's first octet.
    pub xu_unit: u8,
    /// The control selector within the XU.
    pub selector: u8,
}

impl EmitterXuInfo {
    /// Try to extract emitter XU info from a quirk.
    /// Returns `None` if the quirk lacks `emitter_xu_guid` or `emitter_xu_selector`.
    pub fn from_quirk(quirk: &Quirk) -> Option<Self> {
        let guid = quirk.emitter_xu_guid.as_deref()?;
        let selector = quirk.emitter_xu_selector?;

        // Parse the XU unit ID from the GUID string.
        // The GUID is typically a hex string like "abcd-1234-..." or a raw hex blob.
        // We need the UVC Extension Unit ID which is an integer assigned by the
        // UVC descriptor, not directly derivable from the GUID alone.
        //
        // In practice, the quirks database should encode the XU unit ID in the first
        // segment of the GUID (e.g., "09-..." means unit 9).  If the GUID is a full
        // UUID, we parse the first hex segment as the unit ID.
        let xu_unit = parse_xu_unit_from_guid(guid)?;

        Some(Self { xu_unit, selector })
    }
}

/// Parse the XU unit ID from a quirks GUID string.
///
/// Supports formats:
/// - Plain decimal number: "9"
/// - First hex segment of a UUID-style string: "09-abcd-..." -> 9
/// - Full hex without dashes (first two hex chars): "09abcdef..." -> 9
fn parse_xu_unit_from_guid(guid: &str) -> Option<u8> {
    let first_segment = guid.split('-').next().unwrap_or(guid);
    // Try parsing as a hex number (handles both "09" and "9")
    u8::from_str_radix(first_segment.trim(), 16).ok()
}

/// Attempt to enable the IR emitter on the given video device.
///
/// Returns `Ok(true)` if the emitter was enabled, `Ok(false)` if no controllable
/// emitter was detected, or `Err` if an error occurred during the ioctl.
pub fn enable_emitter(device_path: &str, quirk: Option<&Quirk>) -> Result<bool, String> {
    if !Path::new(device_path).exists() {
        return Err(format!("device not found: {device_path}"));
    }

    let xu_info = match quirk.and_then(EmitterXuInfo::from_quirk) {
        Some(info) => info,
        None => {
            tracing::debug!(
                "no emitter XU info available for {device_path}, skipping"
            );
            return Ok(false);
        }
    };

    set_emitter_control(device_path, &xu_info, 1)?;
    tracing::debug!(
        device = device_path,
        xu_unit = xu_info.xu_unit,
        selector = xu_info.selector,
        "IR emitter enabled via UVC XU"
    );
    Ok(true)
}

/// Attempt to enable the IR emitter using pre-extracted XU info.
///
/// This variant is used by the `Camera` drop path where the original `Quirk`
/// reference is no longer available.
pub fn enable_emitter_with_info(device_path: &str, xu_info: &EmitterXuInfo) -> Result<bool, String> {
    if !Path::new(device_path).exists() {
        return Err(format!("device not found: {device_path}"));
    }

    set_emitter_control(device_path, xu_info, 1)?;
    Ok(true)
}

/// Attempt to disable the IR emitter on the given video device.
///
/// Returns `Ok(())` on success or if no controllable emitter is present.
pub fn disable_emitter(device_path: &str, quirk: Option<&Quirk>) -> Result<(), String> {
    if !Path::new(device_path).exists() {
        return Err(format!("device not found: {device_path}"));
    }

    let xu_info = match quirk.and_then(EmitterXuInfo::from_quirk) {
        Some(info) => info,
        None => {
            tracing::debug!(
                "no emitter XU info available for {device_path}, skipping"
            );
            return Ok(());
        }
    };

    set_emitter_control(device_path, &xu_info, 0)?;
    tracing::debug!(
        device = device_path,
        xu_unit = xu_info.xu_unit,
        selector = xu_info.selector,
        "IR emitter disabled via UVC XU"
    );
    Ok(())
}

/// Disable the IR emitter using pre-extracted XU info.
pub fn disable_emitter_with_info(device_path: &str, xu_info: &EmitterXuInfo) -> Result<(), String> {
    if !Path::new(device_path).exists() {
        return Err(format!("device not found: {device_path}"));
    }

    set_emitter_control(device_path, xu_info, 0)?;
    Ok(())
}

/// Check if the given device has a controllable IR emitter.
///
/// Attempts a `UVC_GET_CUR` query on the emitter's XU control. If the ioctl
/// succeeds, the emitter is controllable.
pub fn has_controllable_emitter(device_path: &str, quirk: Option<&Quirk>) -> bool {
    let xu_info = match quirk.and_then(EmitterXuInfo::from_quirk) {
        Some(info) => info,
        None => return false,
    };

    match get_emitter_control(device_path, &xu_info) {
        Ok(_) => true,
        Err(e) => {
            tracing::debug!(
                device = device_path,
                error = %e,
                "emitter XU query failed, device not controllable"
            );
            false
        }
    }
}

/// Send a `UVC_SET_CUR` to the emitter XU control.
fn set_emitter_control(device_path: &str, xu_info: &EmitterXuInfo, value: u8) -> Result<(), String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device_path)
        .map_err(|e| format!("failed to open {device_path}: {e}"))?;

    let mut data = [value];

    let query = UvcXuControlQuery {
        unit: xu_info.xu_unit,
        selector: xu_info.selector,
        query: UVC_SET_CUR,
        size: 1,
        data: data.as_mut_ptr(),
    };

    // SAFETY: We pass a valid fd and a properly sized/aligned struct matching the
    // kernel's `struct uvc_xu_control_query`. The `data` pointer is valid for
    // `size` bytes and lives for the duration of the ioctl call.
    let ret = unsafe { libc::ioctl(file.as_raw_fd(), UVCIOC_CTRL_QUERY, &query) };

    if ret < 0 {
        let errno = std::io::Error::last_os_error();
        Err(format!(
            "UVC XU SET_CUR failed on {device_path} (unit={}, sel={}): {errno}",
            xu_info.xu_unit, xu_info.selector
        ))
    } else {
        Ok(())
    }
}

/// Send a `UVC_GET_CUR` to the emitter XU control.
/// Returns the current value byte on success.
fn get_emitter_control(device_path: &str, xu_info: &EmitterXuInfo) -> Result<u8, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device_path)
        .map_err(|e| format!("failed to open {device_path}: {e}"))?;

    let mut data = [0u8];

    let query = UvcXuControlQuery {
        unit: xu_info.xu_unit,
        selector: xu_info.selector,
        query: UVC_GET_CUR,
        size: 1,
        data: data.as_mut_ptr(),
    };

    // SAFETY: Same as `set_emitter_control` above.
    let ret = unsafe { libc::ioctl(file.as_raw_fd(), UVCIOC_CTRL_QUERY, &query) };

    if ret < 0 {
        let errno = std::io::Error::last_os_error();
        Err(format!(
            "UVC XU GET_CUR failed on {device_path} (unit={}, sel={}): {errno}",
            xu_info.xu_unit, xu_info.selector
        ))
    } else {
        Ok(data[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_nonexistent_device_returns_error() {
        let quirk = make_quirk_with_emitter();
        let result = enable_emitter("/dev/nonexistent_camera", Some(&quirk));
        assert!(result.is_err());
    }

    #[test]
    fn disable_nonexistent_device_returns_error() {
        let quirk = make_quirk_with_emitter();
        let result = disable_emitter("/dev/nonexistent_camera", Some(&quirk));
        assert!(result.is_err());
    }

    #[test]
    fn enable_without_quirk_returns_false() {
        let result = enable_emitter("/dev/video0", None);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn disable_without_quirk_returns_ok() {
        let result = disable_emitter("/dev/video0", None);
        assert!(result.is_ok());
    }

    #[test]
    fn has_controllable_emitter_returns_false_for_unknown() {
        assert!(!has_controllable_emitter("/dev/video0", None));
    }

    #[test]
    fn has_controllable_emitter_false_without_quirk() {
        assert!(!has_controllable_emitter("/dev/video0", None));
    }

    #[test]
    fn parse_xu_unit_hex() {
        assert_eq!(parse_xu_unit_from_guid("09"), Some(9));
        assert_eq!(parse_xu_unit_from_guid("0a"), Some(10));
        assert_eq!(parse_xu_unit_from_guid("ff"), Some(255));
        assert_eq!(parse_xu_unit_from_guid("9"), Some(9));
    }

    #[test]
    fn parse_xu_unit_from_uuid_style() {
        assert_eq!(parse_xu_unit_from_guid("09-abcd-1234"), Some(9));
        assert_eq!(parse_xu_unit_from_guid("0a-dead-beef"), Some(10));
    }

    #[test]
    fn parse_xu_unit_invalid() {
        // "zz" is not valid hex
        assert_eq!(parse_xu_unit_from_guid("zz-1234"), None);
    }

    #[test]
    fn emitter_xu_info_from_quirk_with_fields() {
        let quirk = make_quirk_with_emitter();
        let info = EmitterXuInfo::from_quirk(&quirk);
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.xu_unit, 9);
        assert_eq!(info.selector, 3);
    }

    #[test]
    fn emitter_xu_info_from_quirk_without_fields() {
        let quirk = make_quirk_without_emitter();
        let info = EmitterXuInfo::from_quirk(&quirk);
        assert!(info.is_none());
    }

    #[test]
    #[ignore] // Requires actual IR camera hardware
    fn toggle_emitter_on_real_device() {
        // Manual test: run with `cargo test -- --ignored`
        if Path::new("/dev/video0").exists() {
            let quirk = make_quirk_with_emitter();
            let result = enable_emitter("/dev/video0", Some(&quirk));
            println!("enable result: {result:?}");
            let result = disable_emitter("/dev/video0", Some(&quirk));
            println!("disable result: {result:?}");
        }
    }

    fn make_quirk_with_emitter() -> Quirk {
        Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: None,
            force_ir: None,
            emitter_xu_guid: Some("09-abcd-1234".into()),
            emitter_xu_selector: Some(3),
            warmup_frames: None,
            format_preference: None,
            rotation: None,
            notes: None,
        }
    }

    fn make_quirk_without_emitter() -> Quirk {
        Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: None,
            force_ir: None,
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            rotation: None,
            notes: None,
        }
    }
}
