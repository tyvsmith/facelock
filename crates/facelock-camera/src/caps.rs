//! Camera capability interrogation (gap D8).
//!
//! [`ResolvedCamera::interrogate`] is the ONE place [`CameraCaps`] are
//! computed: construction is the only moment that knows the device's own
//! format enumeration, its quirks match, and its sysfs identity all at once.
//! Everything downstream asks the camera (`CameraSource::capabilities`)
//! instead of threading `device_is_ir` / `DeviceFingerprint` as loose
//! parameters.

use facelock_core::config::DeviceConfig;
use facelock_core::error::Result;
use facelock_core::types::CameraCaps;

use crate::capture::{Camera, ir_texture_scale_for_format, select_format_for_quirk};
use crate::device::{self, DeviceInfo};
use crate::quirks::{Quirk, QuirksDb, device_fingerprint};

/// A resolved-but-not-yet-opened camera device: its identity, its matched
/// quirk, and the capabilities derived from interrogation.
///
/// Splitting resolution from opening lets pre-flight gates (`pre_check`'s
/// `require_ir`) consult `caps` before any camera stream — or its indicator
/// LED — is touched, while the eventually-opened [`Camera`] carries the very
/// same caps.
#[derive(Debug, Clone)]
pub struct ResolvedCamera {
    pub info: DeviceInfo,
    /// The quirks-DB entry matched for this device, if any. Its overrides
    /// (format preference, rotation, warmup) are applied at open time.
    pub quirk: Option<Quirk>,
    pub caps: CameraCaps,
}

impl ResolvedCamera {
    /// Resolve the configured device (or auto-detect one) and interrogate it.
    pub fn resolve(config: &DeviceConfig, quirks: &QuirksDb) -> Result<Self> {
        let info = match config.path.as_deref() {
            Some(path) => device::validate_device(path)?,
            // The caller's DB, not a second `QuirksDb::load()`: the quirk set
            // that picks the device must be the one that then classifies it.
            None => device::auto_detect_device_with(quirks)?,
        };
        Ok(Self::interrogate(info, quirks))
    }

    /// Interrogate an already-validated device into its capabilities.
    ///
    /// - `is_ir` adopts the evidence-derived, sibling-aware classification
    ///   (`ir_source_resolved`, #98): queried mono-only formats or a
    ///   corroborated quirk — never the free-text device name.
    /// - `fingerprint` reads sysfs identity; all-`None` when unreadable.
    /// - `applied_quirks` reflects the quirks match.
    ///
    /// The device's enumerated formats are deliberately NOT copied onto the
    /// caps: they already live on `info`, which every holder of a
    /// `ResolvedCamera` has.
    pub fn interrogate(info: DeviceInfo, quirks: &QuirksDb) -> Self {
        let quirk = quirks.find_match(&info).cloned();
        let available: Vec<String> = info
            .formats
            .iter()
            .map(|format| format.fourcc.trim().to_string())
            .collect();
        let selected_format = select_format_for_quirk(quirk.as_ref(), &available)
            .and_then(|index| available.get(index))
            .map(String::as_str)
            .unwrap_or("");
        let caps = CameraCaps {
            is_ir: device::is_ir_camera_resolved(&info, Some(quirks)),
            fingerprint: device_fingerprint(&info.path),
            applied_quirks: quirk.as_ref().map(quirk_id).into_iter().collect(),
            ir_texture_scale: ir_texture_scale_for_format(selected_format, quirk.as_ref()),
        };
        Self { info, quirk, caps }
    }

    /// Open the camera stream; the returned [`Camera`] carries these caps.
    /// The resolved device path takes precedence over `config.path`.
    pub fn open(self, config: &DeviceConfig) -> Result<Camera<'static>> {
        let mut config = config.clone();
        config.path = Some(self.info.path.clone());
        Camera::open_resolved(&config, self.quirk.as_ref(), self.caps)
    }
}

/// Human-readable identifier for a quirks-DB entry, for
/// `CameraCaps::applied_quirks` diagnostics. The quirks files carry no stable
/// ids, so this is derived from the match fields plus the entry's notes.
pub fn quirk_id(quirk: &Quirk) -> String {
    let base = match (&quirk.vendor_id, &quirk.product_id, &quirk.name_pattern) {
        (Some(vid), Some(pid), _) => format!("{vid}:{pid}"),
        (_, _, Some(pattern)) => format!("name:{pattern}"),
        _ => "unidentified-quirk".to_string(),
    };
    match quirk.notes.as_deref() {
        Some(notes) if !notes.is_empty() => format!("{base} ({notes})"),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir_emitter::EmitterXuInfo;
    use facelock_core::types::{DeviceFingerprint, FormatInfo, IrTextureScale};

    fn device_with(name: &str, fourccs: &[&str]) -> DeviceInfo {
        DeviceInfo {
            path: "/dev/nonexistent_test_video".into(),
            name: name.into(),
            driver: "uvcvideo".into(),
            capabilities: vec![],
            formats: fourccs
                .iter()
                .map(|f| FormatInfo {
                    fourcc: (*f).into(),
                    description: "test".into(),
                    sizes: vec![(640, 480)],
                })
                .collect(),
        }
    }

    fn quirk(name_pattern: &str) -> Quirk {
        Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some(name_pattern.into()),
            force_ir: None,
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: None,
        }
    }

    fn y16_quirks(bit_depth: Option<u8>) -> QuirksDb {
        let mut db = QuirksDb::default();
        let mut q = quirk("(?i)test y16 camera");
        q.format_preference = Some("Y16".into());
        q.y16_bit_depth = bit_depth;
        db.push_quirk_for_test(q);
        db
    }

    #[test]
    fn y16_without_a_declared_bit_depth_has_unverified_texture_scale() {
        let resolved = ResolvedCamera::interrogate(
            device_with("Test Y16 Camera", &["Y16"]),
            &y16_quirks(None),
        );

        assert_eq!(
            resolved.caps.ir_texture_scale,
            IrTextureScale::UnverifiedY16
        );
    }

    #[test]
    fn y16_with_an_invalid_declared_bit_depth_has_unverified_texture_scale() {
        for bit_depth in [0, 7, 17, u8::MAX] {
            let resolved = ResolvedCamera::interrogate(
                device_with("Test Y16 Camera", &["Y16"]),
                &y16_quirks(Some(bit_depth)),
            );

            assert_eq!(
                resolved.caps.ir_texture_scale,
                IrTextureScale::UnverifiedY16,
                "invalid bit depth {bit_depth} must not become trusted scale provenance"
            );
        }
    }

    #[test]
    fn y16_with_a_valid_declared_bit_depth_has_verified_texture_scale() {
        for bit_depth in [10, 12, 16] {
            let resolved = ResolvedCamera::interrogate(
                device_with("Test Y16 Camera", &["Y16"]),
                &y16_quirks(Some(bit_depth)),
            );

            assert_eq!(
                resolved.caps.ir_texture_scale,
                IrTextureScale::VerifiedY16 { bit_depth },
            );
        }
    }

    #[test]
    fn grey_selection_never_requires_y16_scale_provenance() {
        let resolved = ResolvedCamera::interrogate(
            device_with("Test Y16 Camera", &["GREY", "Y16"]),
            &QuirksDb::default(),
        );

        assert_eq!(resolved.caps.ir_texture_scale, IrTextureScale::NotY16);
    }

    #[test]
    fn mono_only_device_interrogates_to_ir_caps() {
        // C2's evidence rule, now living in the seam: a device that
        // enumerates ONLY mono/IR-typical formats is IR — the name plays no
        // part in it.
        let resolved =
            ResolvedCamera::interrogate(device_with("USB Camera", &["GREY"]), &QuirksDb::default());
        assert!(resolved.caps.is_ir);
        assert!(resolved.caps.applied_quirks.is_empty());
        // The enumerated formats stay on `info`; the caps do not copy them.
        assert_eq!(resolved.info.formats.len(), 1);
        assert_eq!(resolved.info.formats[0].fourcc, "GREY");
        // Fake path → no sysfs identity → all-None fingerprint, not an error.
        assert_eq!(resolved.caps.fingerprint, DeviceFingerprint::default());
    }

    #[test]
    fn crafted_ir_name_with_color_formats_is_not_ir_caps() {
        // #98 regression, pinned at the caps seam: a v4l2loopback device can
        // set CARD_LABEL to anything; a color-only "Fake IR Camera" must not
        // interrogate into is_ir caps.
        let resolved = ResolvedCamera::interrogate(
            device_with("Fake IR Camera", &["YUYV", "MJPG"]),
            &QuirksDb::default(),
        );
        assert!(!resolved.caps.is_ir);
    }

    #[test]
    fn matched_quirk_populates_applied_quirks() {
        let mut db = QuirksDb::default();
        let mut q = quirk("(?i)test.*module");
        q.emitter_xu_guid = Some("04".into());
        q.emitter_xu_selector = Some(1);
        q.notes = Some("test emitter module".into());
        db.push_quirk_for_test(q);

        // GREY-only: the quirk match is corroborated by the device's own
        // format evidence. The matched quirk itself rides on `resolved.quirk`,
        // which is what the emitter path reads at open time.
        let resolved = ResolvedCamera::interrogate(device_with("Test IR Module", &["GREY"]), &db);
        assert!(resolved.caps.is_ir);
        assert_eq!(
            resolved.caps.applied_quirks,
            vec!["name:(?i)test.*module (test emitter module)".to_string()]
        );
        assert!(EmitterXuInfo::from_quirk(resolved.quirk.as_ref().unwrap()).is_some());
    }

    #[test]
    fn quirk_without_emitter_fields_declares_no_emitter() {
        let mut db = QuirksDb::default();
        db.push_quirk_for_test(quirk("(?i)plain.*cam"));

        let resolved = ResolvedCamera::interrogate(device_with("Plain Cam", &["YUYV"]), &db);
        assert_eq!(resolved.caps.applied_quirks.len(), 1);
        assert!(EmitterXuInfo::from_quirk(resolved.quirk.as_ref().unwrap()).is_none());
    }

    #[test]
    fn unqueryable_caps_fail_closed_but_keep_identity() {
        // The state both the daemon and the oneshot path fall back to when
        // the configured device exists but cannot be interrogated: nothing
        // the device did not demonstrate is asserted, but the sysfs identity
        // (readable from the path alone) survives so device coupling still
        // has something to compare.
        let fp = DeviceFingerprint {
            vid: Some("046d".into()),
            pid: Some("085e".into()),
            serial: None,
            by_path: None,
        };
        let caps = CameraCaps::unqueryable(fp.clone());
        assert!(!caps.is_ir, "an uninterrogated device must not claim IR");
        assert!(caps.applied_quirks.is_empty());
        assert_eq!(caps.fingerprint, fp);
    }

    #[test]
    fn quirk_id_prefers_usb_identity_over_name_pattern() {
        let mut q = quirk("(?i)brio");
        q.vendor_id = Some("046d".into());
        q.product_id = Some("085e".into());
        q.notes = Some("Logitech BRIO".into());
        assert_eq!(quirk_id(&q), "046d:085e (Logitech BRIO)");
        assert_eq!(quirk_id(&quirk("(?i)brio")), "name:(?i)brio");
    }
}
