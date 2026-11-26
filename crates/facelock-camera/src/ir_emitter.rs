//! IR emitter control via UVC Extension Unit (XU) controls.
//!
//! Many IR cameras have LED emitters that must be explicitly toggled on/off.
//! This module provides the interface for controlling these emitters using
//! the Linux UVC driver's `UVCIOC_CTRL_QUERY` ioctl.
//!
//! The quirks database supplies per-device XU unit and selector values, and
//! optionally the byte to write (some firmware treats the control as an
//! intensity level) and a refresh interval (some firmware clears it a few
//! frames after every write, so a single write only lights the emitter for a
//! blip). See [`EmitterRefresher`] for the refresh.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::quirks::Quirk;

/// Shortest refresh interval honored. A quirk asking for less is clamped, so
/// a typo cannot turn every captured frame into a burst of ioctls.
pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(10);

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
    /// The UVC Extension Unit ID (`bUnitID` from the camera's UVC descriptor).
    pub xu_unit: u8,
    /// The control selector within the XU.
    pub selector: u8,
    /// Byte written to switch the emitter on. `1` unless the quirk says
    /// otherwise; always non-zero (zero is the off value).
    pub enable_value: u8,
    /// When set, the enable value is re-written this often while the camera
    /// streams. Never below [`MIN_REFRESH_INTERVAL`].
    pub refresh: Option<Duration>,
}

impl EmitterXuInfo {
    /// Try to extract emitter XU info from a quirk.
    ///
    /// Returns `None` when the quirk has no usable emitter configuration: see
    /// [`emitter_config_problem`] for the reasons, which the quirk loader
    /// reports as a warning.
    pub fn from_quirk(quirk: &Quirk) -> Option<Self> {
        if emitter_config_problem(quirk).is_some() {
            return None;
        }
        let selector = quirk.emitter_xu_selector?;
        let xu_unit = resolve_xu_unit(quirk)?;
        let enable_value = quirk.emitter_xu_value.unwrap_or(1);
        let refresh = quirk
            .emitter_xu_refresh_ms
            .map(|ms| Duration::from_millis(u64::from(ms)).max(MIN_REFRESH_INTERVAL));

        Some(Self {
            xu_unit,
            selector,
            enable_value,
            refresh,
        })
    }
}

/// Resolve the XU unit ID: the explicit `emitter_xu_unit` key wins, otherwise
/// the legacy first-segment-of-`emitter_xu_guid` spelling.
fn resolve_xu_unit(quirk: &Quirk) -> Option<u8> {
    match quirk.emitter_xu_unit {
        Some(unit) => Some(unit),
        None => parse_xu_unit_from_guid(quirk.emitter_xu_guid.as_deref()?),
    }
}

/// Why a quirk's emitter keys cannot produce a working emitter, or `None` when
/// they are either absent or complete and usable.
///
/// A quirk that sets no emitter key at all is not a problem: most cameras have
/// no controllable emitter.
pub(crate) fn emitter_config_problem(quirk: &Quirk) -> Option<String> {
    let any_emitter_key = quirk.emitter_xu_guid.is_some()
        || quirk.emitter_xu_unit.is_some()
        || quirk.emitter_xu_selector.is_some()
        || quirk.emitter_xu_value.is_some()
        || quirk.emitter_xu_refresh_ms.is_some();
    if !any_emitter_key {
        return None;
    }
    if quirk.emitter_xu_selector.is_none() {
        return Some("emitter_xu_selector is not set".into());
    }
    if resolve_xu_unit(quirk).is_none() {
        return Some(match quirk.emitter_xu_guid.as_deref() {
            Some(guid) => format!(
                "emitter_xu_guid {guid:?} is not read as a GUID: its first dash-separated \
                 segment must be the hex unit ID (e.g. \"05\" or \"05-...\"). Set \
                 emitter_xu_unit instead"
            ),
            None => "neither emitter_xu_unit nor emitter_xu_guid is set".into(),
        });
    }
    if quirk.emitter_xu_value == Some(0) {
        return Some(
            "emitter_xu_value is 0, which is the off value; the emitter would never turn on".into(),
        );
    }
    None
}

/// Parse the XU unit ID from a quirks GUID string.
///
/// Legacy spelling kept for existing quirk files. Only the first
/// dash-separated segment is read, as a hex number: `"09"`, `"9"` and
/// `"09-abcd-..."` are all unit 9. The rest of the string is ignored, so this
/// is not GUID matching. New entries should use `emitter_xu_unit`.
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
            tracing::debug!("no emitter XU info available for {device_path}, skipping");
            return Ok(false);
        }
    };

    set_emitter_control(device_path, &xu_info, xu_info.enable_value)?;
    tracing::debug!(
        device = device_path,
        xu_unit = xu_info.xu_unit,
        selector = xu_info.selector,
        value = xu_info.enable_value,
        refresh_ms = xu_info.refresh.map(|d| d.as_millis() as u64),
        "IR emitter enabled via UVC XU"
    );
    Ok(true)
}

/// Attempt to enable the IR emitter using pre-extracted XU info.
///
/// This variant is used by the `Camera` drop path where the original `Quirk`
/// reference is no longer available.
pub fn enable_emitter_with_info(
    device_path: &str,
    xu_info: &EmitterXuInfo,
) -> Result<bool, String> {
    if !Path::new(device_path).exists() {
        return Err(format!("device not found: {device_path}"));
    }

    set_emitter_control(device_path, xu_info, xu_info.enable_value)?;
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
            tracing::debug!("no emitter XU info available for {device_path}, skipping");
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
fn set_emitter_control(
    device_path: &str,
    xu_info: &EmitterXuInfo,
    value: u8,
) -> Result<(), String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device_path)
        .map_err(|e| format!("failed to open {device_path}: {e}"))?;

    write_emitter_control(&file, device_path, xu_info, value)
}

/// `UVC_SET_CUR` on an already-open device node. `device_path` is only used
/// to name the device in the error.
fn write_emitter_control(
    file: &File,
    device_path: &str,
    xu_info: &EmitterXuInfo,
    value: u8,
) -> Result<(), String> {
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

/// Keeps an IR emitter lit for the life of a streaming session on firmware
/// that clears the control a few frames after each write.
///
/// The owner calls [`refresh_if_due`](Self::refresh_if_due) from its frame
/// loop, so there is no timer thread to start, stop or race with `Drop`: the
/// emitter is re-asserted exactly while frames are being consumed, and is not
/// when nobody is reading (the warm-hold window between attempts). The first
/// call after such a gap writes immediately, which is why a camera's warmup
/// frames matter: the first lit frame arrives a few frames after that write.
///
/// Holds its own descriptor on the device node, opened once, so a refresh is a
/// single ioctl rather than an open/ioctl/close.
pub struct EmitterRefresher {
    file: File,
    device_path: String,
    xu_info: EmitterXuInfo,
    interval: Duration,
    last_write: Instant,
}

impl EmitterRefresher {
    /// Open the refresher for `device_path`, or `Ok(None)` when the quirk
    /// configures no refresh interval. The caller has already written the
    /// enable value once (via [`enable_emitter`]), so the first refresh is
    /// due one interval from now.
    pub fn new(device_path: &str, xu_info: &EmitterXuInfo) -> Result<Option<Self>, String> {
        let Some(interval) = xu_info.refresh else {
            return Ok(None);
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(device_path)
            .map_err(|e| format!("failed to open {device_path}: {e}"))?;
        Ok(Some(Self {
            file,
            device_path: device_path.to_string(),
            xu_info: xu_info.clone(),
            interval,
            last_write: Instant::now(),
        }))
    }

    /// Re-write the enable value if a full interval has passed since the last
    /// write. Cheap when not due: one clock read.
    pub fn refresh_if_due(&mut self) -> Result<(), String> {
        self.refresh_at(Instant::now())
    }

    fn refresh_at(&mut self, now: Instant) -> Result<(), String> {
        if !refresh_due(self.last_write, now, self.interval) {
            return Ok(());
        }
        // Advance before the write so a failing ioctl is not retried on every
        // frame; the caller decides whether an error ends refreshing.
        self.last_write = now;
        write_emitter_control(
            &self.file,
            &self.device_path,
            &self.xu_info,
            self.xu_info.enable_value,
        )
    }
}

/// Whether a refresh is due. `now` earlier than `last_write` (a clock quirk)
/// counts as not due rather than underflowing.
fn refresh_due(last_write: Instant, now: Instant, interval: Duration) -> bool {
    now.checked_duration_since(last_write)
        .is_some_and(|elapsed| elapsed >= interval)
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
    #[ignore] // requires /dev/video0
    fn enable_without_quirk_returns_false() {
        let result = enable_emitter("/dev/video0", None);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    #[ignore] // requires /dev/video0
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
    fn defaults_preserve_single_write_of_one() {
        // A quirk with only the original keys must behave exactly as before:
        // write 1 once, no refresh.
        let info = EmitterXuInfo::from_quirk(&make_quirk_with_emitter()).unwrap();
        assert_eq!(info.enable_value, 1);
        assert_eq!(info.refresh, None);
    }

    #[test]
    fn explicit_unit_wins_over_guid() {
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_guid = Some("09-abcd-1234".into());
        quirk.emitter_xu_unit = Some(5);
        assert_eq!(EmitterXuInfo::from_quirk(&quirk).unwrap().xu_unit, 5);
    }

    #[test]
    fn unit_alone_is_enough_without_a_guid() {
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_guid = None;
        quirk.emitter_xu_unit = Some(5);
        let info = EmitterXuInfo::from_quirk(&quirk).unwrap();
        assert_eq!((info.xu_unit, info.selector), (5, 3));
    }

    #[test]
    fn value_and_refresh_come_from_the_quirk() {
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_value = Some(50);
        quirk.emitter_xu_refresh_ms = Some(100);
        let info = EmitterXuInfo::from_quirk(&quirk).unwrap();
        assert_eq!(info.enable_value, 50);
        assert_eq!(info.refresh, Some(Duration::from_millis(100)));
    }

    #[test]
    fn refresh_interval_is_clamped_to_the_minimum() {
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_refresh_ms = Some(0);
        assert_eq!(
            EmitterXuInfo::from_quirk(&quirk).unwrap().refresh,
            Some(MIN_REFRESH_INTERVAL)
        );
        quirk.emitter_xu_refresh_ms = Some(1);
        assert_eq!(
            EmitterXuInfo::from_quirk(&quirk).unwrap().refresh,
            Some(MIN_REFRESH_INTERVAL)
        );
    }

    #[test]
    fn zero_enable_value_is_rejected() {
        // Zero is the off value; an "enable" of zero would never light anything.
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_value = Some(0);
        assert!(EmitterXuInfo::from_quirk(&quirk).is_none());
        assert!(
            emitter_config_problem(&quirk)
                .unwrap()
                .contains("off value")
        );
    }

    #[test]
    fn config_problem_is_none_without_any_emitter_key() {
        assert_eq!(emitter_config_problem(&make_quirk_without_emitter()), None);
    }

    #[test]
    fn config_problem_is_none_for_a_complete_quirk() {
        assert_eq!(emitter_config_problem(&make_quirk_with_emitter()), None);
    }

    #[test]
    fn config_problem_flags_a_missing_selector() {
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_selector = None;
        assert!(
            emitter_config_problem(&quirk)
                .unwrap()
                .contains("emitter_xu_selector")
        );
        assert!(EmitterXuInfo::from_quirk(&quirk).is_none());
    }

    #[test]
    fn config_problem_flags_a_full_uuid_in_the_guid_key() {
        // The trap this guards: `emitter_xu_guid` looks like it takes a GUID,
        // but only the first segment is read, as the hex unit ID. A real UUID's
        // first segment overflows a byte, so the emitter was silently skipped.
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_guid = Some("52182091-70d0-4ff5-acbc-0d04cc5f5a64".into());
        assert!(EmitterXuInfo::from_quirk(&quirk).is_none());
        let problem = emitter_config_problem(&quirk).unwrap();
        assert!(problem.contains("emitter_xu_unit"), "{problem}");
        assert!(problem.contains("52182091"), "{problem}");
    }

    #[test]
    fn config_problem_flags_missing_unit_and_guid() {
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_guid = None;
        assert!(
            emitter_config_problem(&quirk)
                .unwrap()
                .contains("neither emitter_xu_unit nor emitter_xu_guid")
        );
    }

    #[test]
    fn config_problem_flags_value_or_refresh_without_the_rest() {
        // Tuning keys on a quirk that never says where the control is.
        let mut quirk = make_quirk_without_emitter();
        quirk.emitter_xu_value = Some(50);
        assert!(emitter_config_problem(&quirk).is_some());
        let mut quirk = make_quirk_without_emitter();
        quirk.emitter_xu_refresh_ms = Some(100);
        assert!(emitter_config_problem(&quirk).is_some());
    }

    #[test]
    fn refresh_due_respects_the_interval() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(100);
        assert!(!refresh_due(t0, t0, interval));
        assert!(!refresh_due(t0, t0 + Duration::from_millis(99), interval));
        assert!(refresh_due(t0, t0 + Duration::from_millis(100), interval));
        assert!(refresh_due(t0, t0 + Duration::from_secs(5), interval));
    }

    #[test]
    fn refresh_due_tolerates_a_backwards_clock_reading() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(1);
        assert!(!refresh_due(later, t0, Duration::from_millis(100)));
    }

    #[test]
    fn refresher_is_absent_without_a_refresh_interval() {
        // Opens nothing: no interval configured means no descriptor either.
        let info = EmitterXuInfo::from_quirk(&make_quirk_with_emitter()).unwrap();
        assert!(
            EmitterRefresher::new("/dev/nonexistent_camera", &info)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn refresher_open_failure_is_an_error_not_a_panic() {
        let mut quirk = make_quirk_with_emitter();
        quirk.emitter_xu_refresh_ms = Some(100);
        let info = EmitterXuInfo::from_quirk(&quirk).unwrap();
        assert!(EmitterRefresher::new("/dev/nonexistent_camera", &info).is_err());
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
            modalias: None,
            node_name: None,
            force_ir: None,
            emitter_xu_guid: Some("09-abcd-1234".into()),
            emitter_xu_selector: Some(3),
            emitter_xu_unit: None,
            emitter_xu_value: None,
            emitter_xu_refresh_ms: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: None,
        }
    }

    fn make_quirk_without_emitter() -> Quirk {
        Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: None,
            modalias: None,
            node_name: None,
            force_ir: None,
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            emitter_xu_unit: None,
            emitter_xu_value: None,
            emitter_xu_refresh_ms: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: None,
        }
    }
}
