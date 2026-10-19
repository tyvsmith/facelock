use serde::Deserialize;
use std::path::Path;
use tracing::{debug, info, warn};

use crate::device::DeviceInfo;

/// A hardware quirk entry loaded from a TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct Quirk {
    /// USB vendor ID (hex string, e.g. "8086")
    #[serde(default)]
    pub vendor_id: Option<String>,
    /// USB product ID (hex string, e.g. "0b07")
    #[serde(default)]
    pub product_id: Option<String>,
    /// Glob-like pattern matched against the device name, a fallback when USB
    /// IDs are unavailable. NOT a full regex: only a leading `(?i)` (case
    /// insensitive) and `.*` (wildcard) are supported, and the pattern is
    /// anchored — it must describe the WHOLE name, not a substring (#99). See
    /// [`name_matches`].
    #[serde(default)]
    pub name_pattern: Option<String>,
    /// Kernel modalias of the node's parent hardware device, compared exactly
    /// against `/sys/class/video4linux/videoN/device/modalias`, or the
    /// `MODALIAS=` line of `.../device/uevent` on a bus that exposes no
    /// `modalias` file (e.g. `auxiliary:intel_ipu6.isys`). Identifies a
    /// non-USB (platform, auxiliary, PCI) capture pipeline. Only matches together with
    /// [`node_name`](Self::node_name); see [`PlatformIdentity`].
    #[serde(default)]
    pub modalias: Option<String>,
    /// Kernel-assigned V4L2 node name, compared exactly against
    /// `/sys/class/video4linux/videoN/name` (e.g. `Intel IPU6 ISYS Capture 0`).
    /// Required alongside [`modalias`](Self::modalias) so a platform quirk
    /// names one capture node, not every node on the card.
    #[serde(default)]
    pub node_name: Option<String>,
    /// Force this device to be treated as an IR camera
    #[serde(default)]
    pub force_ir: Option<bool>,
    /// UVC Extension Unit GUID for IR emitter control
    #[serde(default)]
    pub emitter_xu_guid: Option<String>,
    /// UVC XU control selector for emitter toggle
    #[serde(default)]
    pub emitter_xu_selector: Option<u8>,
    /// Override warmup frames for this camera
    #[serde(default)]
    pub warmup_frames: Option<u32>,
    /// Preferred pixel format
    #[serde(default)]
    pub format_preference: Option<String>,
    /// Verified effective bit depth of this sensor's Y16 samples (8..=16).
    /// When present and valid, the session Y16 -> 8-bit shift becomes
    /// `bit_depth - 8`; absent or invalid values cannot authorize Y16 texture
    /// enforcement, even if non-auth conversion calibrates from frames.
    #[serde(default)]
    pub y16_bit_depth: Option<u8>,
    /// Image rotation in degrees
    #[serde(default)]
    pub rotation: Option<u16>,
    /// Human-readable notes
    #[serde(default)]
    pub notes: Option<String>,
}

/// Container for a list of quirks in a TOML file.
#[derive(Debug, Deserialize)]
struct QuirksFile {
    #[serde(default)]
    quirk: Vec<Quirk>,
}

/// Provenance of a [`QuirksDb`] match — how the quirk matched, which bounds
/// how far it can be trusted for a security-relevant decision like `force_ir`.
///
/// Neither kind is truly unforgeable; they differ only in the *cost* of
/// forgery. A [`QuirkMatchKind::UsbId`] match rests on the sysfs USB identity,
/// which a software-only virtual device (v4l2loopback) cannot present — though
/// a programmable USB gadget could still advertise a chosen VID:PID. A
/// [`QuirkMatchKind::NameOnly`] match rests only on the free-text device name,
/// which is trivially attacker-controlled on virtual devices. This is why
/// `force_ir` treats them differently (see
/// [`crate::device::ir_source_with_quirks`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuirkMatchKind {
    /// Matched by USB vendor:product ID, read from sysfs. Not spoofable by
    /// a virtual device (e.g. v4l2loopback exposes no real USB node, so it
    /// can never win a USB-ID match).
    UsbId,
    /// Matched by `modalias` + `node_name` against the kernel-reported
    /// identity of a non-USB node (see [`PlatformIdentity`]). Same authority
    /// as [`QuirkMatchKind::UsbId`]: the modalias is written by the kernel
    /// bus driver of a real parent device, and a virtual node has no parent
    /// device to read it from.
    PlatformId,
    /// Matched by `name_pattern` against the free-text device/card name
    /// only. The name is attacker-controlled on virtual devices — see
    /// `facelock_camera::device::ir_source_with_quirks` for how a
    /// `force_ir = true` name-only match is required to be corroborated
    /// before it is trusted (#98 Task 3).
    NameOnly,
}

/// Database of hardware quirks loaded from TOML files.
#[derive(Debug, Default)]
pub struct QuirksDb {
    quirks: Vec<Quirk>,
}

impl QuirksDb {
    /// Load quirks from both system and user directories.
    /// System: `/usr/share/facelock/quirks.d/`
    /// User overrides: `/etc/facelock/quirks.d/`
    ///
    /// Files are loaded in alphabetical order within each directory, and the
    /// system directory before the user one. Later entries override earlier
    /// ones for the same device: matching is last-wins per pass, so an
    /// operator's `/etc` entry shadows the shipped entry it corrects. See
    /// [`find_match_with_kind`](Self::find_match_with_kind).
    pub fn load() -> Self {
        let mut db = Self::default();
        for dir in &["/usr/share/facelock/quirks.d", "/etc/facelock/quirks.d"] {
            db.load_dir(Path::new(dir));
        }
        if db.quirks.is_empty() {
            debug!("no hardware quirks loaded");
        } else {
            info!(count = db.quirks.len(), "loaded hardware quirks");
        }
        db
    }

    /// Load quirks from a specific directory.
    pub fn load_dir(&mut self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return, // Directory doesn't exist, that's fine
        };

        let mut paths: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
            .map(|e| e.path())
            .collect();
        paths.sort();

        for path in paths {
            match Self::load_file(&path) {
                Ok(quirks) => {
                    debug!(file = %path.display(), count = quirks.len(), "loaded quirks file");
                    self.quirks.extend(quirks);
                }
                Err(e) => {
                    warn!(file = %path.display(), "failed to load quirks file: {e}");
                }
            }
        }
    }

    fn load_file(path: &Path) -> Result<Vec<Quirk>, String> {
        let content = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
        let mut file: QuirksFile =
            toml::from_str(&content).map_err(|e| format!("parse error: {e}"))?;
        warn_incomplete_platform_keys(path, &file.quirk);
        // Quirk files are often written with the padded V4L2 FourCC ("Y16 ").
        // Normalize here so comparisons against `FormatInfo::fourcc` match.
        for quirk in &mut file.quirk {
            if let Some(pref) = &mut quirk.format_preference {
                *pref = pref.trim().to_string();
            }
        }
        Ok(file.quirk)
    }

    /// Find a matching quirk for the given device.
    /// Matches by USB vendor:product ID first, then by name pattern; within
    /// each pass the LAST-loaded entry wins (see [`find_match_with_kind`](Self::find_match_with_kind)).
    pub fn find_match(&self, device: &DeviceInfo) -> Option<&Quirk> {
        let usb_ids = read_usb_ids(&device.path);
        let platform = read_platform_identity(&device.path);
        self.find_match_with_kind(device, usb_ids.as_ref(), platform.as_ref())
            .map(|(q, _)| q)
    }

    /// Like [`find_match`](Self::find_match) but with the device's USB
    /// vendor:product IDs supplied by the caller instead of read from sysfs.
    /// This keeps the sysfs read at the call boundary so classification is
    /// testable and so callers that already resolved the identity (e.g.
    /// multi-node sibling grouping) don't re-read sysfs per lookup.
    pub fn find_match_with_ids(
        &self,
        device: &DeviceInfo,
        usb_ids: Option<&(String, String)>,
    ) -> Option<&Quirk> {
        self.find_match_with_kind(device, usb_ids, None)
            .map(|(q, _)| q)
    }

    /// Like [`find_match_with_ids`](Self::find_match_with_ids) but also
    /// reports HOW the quirk matched. Callers that gate a security-relevant
    /// decision (like `force_ir`, see `facelock_camera::device`) on the
    /// result need this: a [`QuirkMatchKind::NameOnly`] match rests on the
    /// free-text device name, which is attacker-controlled on virtual
    /// devices such as v4l2loopback, and should not be trusted the same way
    /// as a [`QuirkMatchKind::UsbId`] match.
    ///
    /// Passes run in order USB ID, platform identity, name. A
    /// [`QuirkMatchKind::PlatformId`] match needs both `modalias` and
    /// `node_name` on the quirk to equal the node's [`PlatformIdentity`].
    ///
    /// A USB-ID match always beats a name match — that is specificity, and it
    /// is independent of load order. *Within* each pass the LAST-loaded entry
    /// wins, which is what makes [`load`](Self::load)'s ordering an override
    /// mechanism: `/usr/share/facelock/quirks.d` is loaded first and
    /// `/etc/facelock/quirks.d` second, so an operator's `/etc` entry shadows
    /// the shipped entry for the same device. First-match-wins would have
    /// silently ignored that override — and since `force_ir = false` is
    /// authoritative unconditionally, the entry an operator writes precisely
    /// to disable a bad shipped `force_ir` is the one that most needs to win.
    pub fn find_match_with_kind(
        &self,
        device: &DeviceInfo,
        usb_ids: Option<&(String, String)>,
        platform: Option<&PlatformIdentity>,
    ) -> Option<(&Quirk, QuirkMatchKind)> {
        // First pass: match by USB vendor:product ID (most specific).
        if let Some((vendor, product)) = usb_ids {
            let matched = self.quirks.iter().rev().find(|quirk| {
                matches!(
                    (&quirk.vendor_id, &quirk.product_id),
                    (Some(qv), Some(qp))
                        if qv.eq_ignore_ascii_case(vendor) && qp.eq_ignore_ascii_case(product)
                )
            });
            if let Some(quirk) = matched {
                debug!(
                    device = %device.path,
                    vendor, product,
                    notes = quirk.notes.as_deref().unwrap_or(""),
                    "matched quirk by USB ID"
                );
                return Some((quirk, QuirkMatchKind::UsbId));
            }
        }

        // Second pass: match by kernel-reported platform identity. Exact
        // comparison on both keys; a quirk missing either never matches here.
        if let Some(id) = platform {
            let matched = self.quirks.iter().rev().find(|quirk| {
                matches!(
                    (&quirk.modalias, &quirk.node_name),
                    (Some(qm), Some(qn)) if *qm == id.modalias && *qn == id.node_name
                )
            });
            if let Some(quirk) = matched {
                debug!(
                    device = %device.path,
                    modalias = %id.modalias,
                    node_name = %id.node_name,
                    notes = quirk.notes.as_deref().unwrap_or(""),
                    "matched quirk by platform identity"
                );
                return Some((quirk, QuirkMatchKind::PlatformId));
            }
        }

        // Third pass: match by name pattern. Simple case-insensitive
        // pattern matching (see `name_matches`); we avoid the regex crate.
        let matched = self.quirks.iter().rev().find(|quirk| {
            quirk
                .name_pattern
                .as_deref()
                .is_some_and(|pattern| name_matches(pattern, &device.name))
        });
        if let Some(quirk) = matched {
            debug!(
                device = %device.path,
                name = %device.name,
                pattern = quirk.name_pattern.as_deref().unwrap_or(""),
                notes = quirk.notes.as_deref().unwrap_or(""),
                "matched quirk by name pattern"
            );
            return Some((quirk, QuirkMatchKind::NameOnly));
        }

        None
    }

    /// Get all quirks for display/debugging.
    pub fn all(&self) -> &[Quirk] {
        &self.quirks
    }

    /// Push a quirk directly (test-only helper for cross-module tests).
    #[cfg(test)]
    pub fn push_quirk_for_test(&mut self, quirk: Quirk) {
        self.quirks.push(quirk);
    }
}

/// Read USB vendor:product IDs from sysfs for a video device.
/// Returns (vendor_id, product_id) as hex strings, or None if unavailable.
pub(crate) fn read_usb_ids(device_path: &str) -> Option<(String, String)> {
    let fp = device_fingerprint(device_path);
    match (fp.vid, fp.pid) {
        (Some(v), Some(p)) => Some((v, p)),
        _ => None,
    }
}

/// Kernel-reported identity of a non-USB V4L2 capture node: the parent
/// device's `modalias` and the node's own `name`, both from sysfs.
///
/// The modalias is written by the kernel bus code for a real parent device
/// (`auxiliary:`, `of:`, `pci:`, `acpi:`, `platform:`), not by the V4L2
/// driver or by userspace. v4l2loopback lives under `/sys/devices/virtual`
/// with no parent device to read it from. A test driver such as vivid does
/// have a platform parent, but its own modalias (`platform:vivid`) and
/// driver-fixed node names cannot equal another device's identity. The
/// node name is what that real driver assigned to this one node, which is
/// what keeps a platform verdict per node rather than per card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformIdentity {
    pub modalias: String,
    pub node_name: String,
}

/// Read the [`PlatformIdentity`] of `/dev/videoN` from sysfs. `None` for a
/// USB node (covered by [`read_usb_ids`]), a virtual node, or any node whose
/// parent device or name cannot be read.
pub fn read_platform_identity(device_path: &str) -> Option<PlatformIdentity> {
    let dev_name = device_path.strip_prefix("/dev/")?;
    platform_identity_from_sysfs(&Path::new("/sys/class/video4linux").join(dev_name))
}

/// [`read_platform_identity`] against an explicit sysfs class directory, so
/// the rules are testable on a fixture tree.
fn platform_identity_from_sysfs(class_dir: &Path) -> Option<PlatformIdentity> {
    // A virtual node has no `device` link; one whose link resolves under
    // `devices/virtual` is refused too, so no kernel layout change can let a
    // software-only device present a parent modalias.
    let parent = std::fs::canonicalize(class_dir.join("device")).ok()?;
    let mut components = parent.components().map(|c| c.as_os_str());
    if components.any(|c| c == "devices") && components.next().is_some_and(|c| c == "virtual") {
        return None;
    }
    let modalias = read_modalias(&parent)?;
    if modalias.starts_with("usb:") {
        return None;
    }
    let node_name = try_read_sysfs_attr(&class_dir.to_string_lossy(), "name")?;
    Some(PlatformIdentity {
        modalias,
        node_name,
    })
}

/// The modalias of a parent device directory: the `modalias` attribute when
/// the bus exposes one, else the `MODALIAS=` line of its `uevent`.
///
/// Both come from the kernel bus code, but only some buses create the
/// attribute file. An auxiliary-bus device (Intel IPU6/IPU7 ISYS) has no
/// `modalias` file; its identity (`auxiliary:intel_ipu6.isys`) is written
/// into `uevent` only. The file wins when both exist, and the `uevent` key is
/// matched exactly (first `MODALIAS=` line, trimmed).
fn read_modalias(parent: &Path) -> Option<String> {
    let parent_str = parent.to_string_lossy();
    try_read_sysfs_attr(&parent_str, "modalias").or_else(|| {
        try_read_sysfs_attr(&parent_str, "uevent")?
            .lines()
            .find_map(|line| line.strip_prefix("MODALIAS="))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// A quirk with only one of `modalias` / `node_name` can never match by
/// platform identity. Say so at load rather than letting it fail silently.
fn warn_incomplete_platform_keys(path: &Path, quirks: &[Quirk]) {
    for quirk in quirks {
        if quirk.modalias.is_some() != quirk.node_name.is_some() {
            warn!(
                file = %path.display(),
                notes = quirk.notes.as_deref().unwrap_or(""),
                "quirk sets only one of modalias/node_name; both are required \
                 for a platform identity match, so it will not match by one"
            );
        }
    }
}

/// Read a stable-ish hardware fingerprint for a video device from sysfs.
///
/// Reads `idVendor`/`idProduct`/`serial` from the same sysfs base used for
/// quirks matching (walking one level up to reach the USB device node), plus
/// the `/dev/v4l/by-path` symlink target. **Tolerates every field being
/// missing** — a camera with no readable USB identity yields an all-`None`
/// fingerprint, which the enroll path stores as a NULL `device_id` so coupling
/// never becomes a hard lockout.
///
/// This is model-granularity (VID:PID) at best and forgeable by a programmable
/// USB device: advisory defense-in-depth, not attestation. See
/// [`facelock_core::types::DeviceFingerprint`].
pub fn device_fingerprint(device_path: &str) -> facelock_core::types::DeviceFingerprint {
    use facelock_core::types::DeviceFingerprint;

    let dev_name = match device_path.strip_prefix("/dev/") {
        Some(n) => n,
        None => return DeviceFingerprint::default(),
    };
    let sysfs_base = format!("/sys/class/video4linux/{dev_name}/device");
    let parent = format!("{sysfs_base}/..");

    // Vendor/product/serial live on the USB device node, which may be the
    // `device` symlink target itself or one level up.
    let read_attr = |attr: &str| {
        try_read_sysfs_attr(&sysfs_base, attr).or_else(|| try_read_sysfs_attr(&parent, attr))
    };

    DeviceFingerprint {
        // Normalize hex ids to lowercase so the canonical form is stable
        // regardless of sysfs casing.
        vid: read_attr("idVendor").map(|s| s.to_ascii_lowercase()),
        pid: read_attr("idProduct").map(|s| s.to_ascii_lowercase()),
        serial: read_attr("serial"),
        by_path: read_v4l_by_path(device_path),
    }
}

/// Resolve the stable `/dev/v4l/by-path/...` symlink that points at this device.
/// Returns the symlink filename (bus topology), not the resolved `/dev/videoN`.
fn read_v4l_by_path(device_path: &str) -> Option<String> {
    let entries = std::fs::read_dir("/dev/v4l/by-path").ok()?;
    let target = std::fs::canonicalize(device_path).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        if let Ok(resolved) = std::fs::canonicalize(entry.path())
            && resolved == target
        {
            return entry.file_name().into_string().ok();
        }
    }
    None
}

fn try_read_sysfs_attr(base: &str, attr: &str) -> Option<String> {
    let path = format!("{base}/{attr}");
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Simple pattern matching: supports (?i) prefix for case-insensitive, .* for
/// wildcards. This is a simplified matcher that handles the most common
/// quirks patterns without pulling in the full regex crate.
///
/// Matches against the WHOLE `name`, not a substring of it (#99): a part of
/// the pattern that isn't adjacent to a leading/trailing `.*` must anchor to
/// the corresponding end of `name`. Without this, `(?i)ir.*camera` matches
/// "Sirius Camera" — the "ir" inside "Sirius" satisfies the first part, and
/// "camera" appears later — silently granting `force_ir` to an unrelated RGB
/// webcam. `.*` at either end still means "anything may precede/follow
/// here", so `(?i).*ir camera.*` still matches "Integrated IR Camera 5MP".
fn name_matches(pattern: &str, name: &str) -> bool {
    let (case_insensitive, pattern) = match pattern.strip_prefix("(?i)") {
        Some(p) => (true, p),
        None => (false, pattern),
    };

    let name = if case_insensitive {
        name.to_lowercase()
    } else {
        name.to_string()
    };
    let pattern = if case_insensitive {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };

    // Splitting on the literal ".*" tells us, from the emptiness of the
    // first/last raw segment, whether the pattern opens/closes with a
    // wildcard (an empty segment there means ".*" was right at that edge).
    let raw_parts: Vec<&str> = pattern.split(".*").collect();
    let starts_wild = raw_parts.len() > 1 && raw_parts.first().is_some_and(|p| p.is_empty());
    let ends_wild = raw_parts.len() > 1 && raw_parts.last().is_some_and(|p| p.is_empty());
    let parts: Vec<&str> = raw_parts.into_iter().filter(|p| !p.is_empty()).collect();

    if parts.is_empty() {
        // Pattern is empty, or entirely wildcard (".*", ".*.*", ...) — matches anything.
        return true;
    }

    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == parts.len() - 1;
        if is_first && !starts_wild {
            // No leading wildcard: this part must anchor the very start of `name`.
            if !name[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
            // A single unwildcarded part must also anchor the end.
            if is_last && !ends_wild && pos != name.len() {
                return false;
            }
        } else if is_last && !ends_wild {
            // No trailing wildcard: this part must anchor the very end of
            // `name`, without re-consuming bytes already matched by an
            // earlier part.
            if part.len() > name.len() || name.len() - part.len() < pos || !name.ends_with(part) {
                return false;
            }
            pos = name.len();
        } else {
            match name[pos..].find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_device(name: &str) -> DeviceInfo {
        DeviceInfo {
            path: "/dev/nonexistent_test_video".into(),
            name: name.into(),
            driver: "uvcvideo".into(),
            capabilities: vec![],
            formats: vec![],
        }
    }

    #[test]
    fn name_matches_case_insensitive() {
        assert!(name_matches(
            "(?i).*integrated.*ir camera.*",
            "Integrated IR Camera"
        ));
        assert!(name_matches(
            "(?i).*integrated.*infrared camera.*",
            "Integrated Infrared Camera"
        ));
    }

    #[test]
    fn name_matches_anchors_start_and_end() {
        // No wildcard at either end: the pattern must describe the WHOLE name.
        assert!(name_matches("(?i)hp ir camera", "HP IR Camera"));
        assert!(!name_matches("(?i)hp ir camera", "HP IR Camera 5MP"));
        assert!(!name_matches("(?i)hp ir camera", "New HP IR Camera"));
    }

    #[test]
    fn name_matches_trailing_wildcard_anchors_start_only() {
        assert!(name_matches("(?i)hp.* ir.*", "HP IR Camera 5MP"));
        // Vendor token must still be the literal start of the name.
        assert!(!name_matches("(?i)hp.* ir.*", "New HP IR Camera"));
        assert!(!name_matches("(?i)dell.* ir.*", "Logitech Webcam"));
    }

    #[test]
    fn name_matches_case_sensitive() {
        assert!(name_matches("IR Camera", "IR Camera"));
        assert!(!name_matches("IR Camera", "ir camera"));
        assert!(!name_matches("IR Camera", "My IR Camera"));
    }

    #[test]
    fn name_matches_anchoring_rejects_substring_false_positive() {
        // #99 regression: the unmodified, unanchored pattern must no longer
        // match an unrelated RGB webcam whose name merely contains the "ir"
        // substring ("Sirius") or ("AIR-Cam") ahead of "camera" elsewhere.
        assert!(!name_matches("(?i)ir.*camera", "Sirius Camera"));
        assert!(!name_matches("(?i)ir.*camera", "AIR-Cam"));
        // A real anchored pattern for the same intent still matches its
        // actual device.
        assert!(name_matches("(?i)ir camera.*", "IR Camera 720p"));
        assert!(name_matches("(?i)ir camera.*", "IR Camera"));
    }

    #[test]
    fn name_matches_word_boundary_within_wildcard_span() {
        // A trailing wildcard after a vendor prefix must not let "ir" match
        // via an unrelated word that merely contains the letters "ir"
        // (e.g. "Air") — the pattern's " ir" requires a literal space
        // immediately before "ir".
        assert!(!name_matches("(?i)hp.* ir.*", "HP Air Camera"));
        assert!(!name_matches("(?i)surface.* ir.*", "Surface Air Camera"));
    }

    #[test]
    fn quirk_deserialization() {
        let toml = r#"
[[quirk]]
vendor_id = "8086"
product_id = "0b07"
force_ir = true
warmup_frames = 10
notes = "Test camera"
"#;
        let file: QuirksFile = toml::from_str(toml).unwrap();
        assert_eq!(file.quirk.len(), 1);
        assert_eq!(file.quirk[0].vendor_id.as_deref(), Some("8086"));
        assert_eq!(file.quirk[0].force_ir, Some(true));
        assert_eq!(file.quirk[0].warmup_frames, Some(10));
        // Omitted keys stay None: non-auth conversion may calibrate from
        // frames, but Y16 authentication remains unverified.
        assert_eq!(file.quirk[0].y16_bit_depth, None);
    }

    #[test]
    fn quirk_deserialization_y16_bit_depth() {
        let toml = r#"
[[quirk]]
vendor_id = "8086"
product_id = "0b07"
format_preference = "Y16 "
y16_bit_depth = 10
"#;
        let file: QuirksFile = toml::from_str(toml).unwrap();
        assert_eq!(file.quirk[0].y16_bit_depth, Some(10));
    }

    #[test]
    fn quirks_db_find_by_name() {
        let mut db = QuirksDb::default();
        db.quirks.push(Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)hp.* ir.*".into()),
            modalias: None,
            node_name: None,
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: Some(8),
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: Some("HP IR".into()),
        });

        let device = make_device("HP IR Camera 5MP");
        let quirk = db.find_match(&device);
        assert!(quirk.is_some());
        assert_eq!(quirk.unwrap().force_ir, Some(true));
    }

    #[test]
    fn quirks_db_no_match() {
        let db = QuirksDb::default();
        let device = make_device("Some Random Camera");
        assert!(db.find_match(&device).is_none());
    }

    #[test]
    fn quirks_db_usb_id_matching() {
        let mut db = QuirksDb::default();
        db.quirks.push(Quirk {
            vendor_id: Some("8086".into()),
            product_id: Some("0b07".into()),
            name_pattern: None,
            modalias: None,
            node_name: None,
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: Some(10),
            format_preference: Some("Y16".into()),
            y16_bit_depth: None,
            rotation: None,
            notes: Some("Intel RealSense".into()),
        });
        // Add a name-pattern quirk that would also match
        db.quirks.push(Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i).*realsense.*".into()),
            modalias: None,
            node_name: None,
            force_ir: Some(false), // Different value to test priority
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: None,
        });

        // USB ID matching not testable without sysfs, but we can verify
        // name pattern is the fallback when USB IDs don't match
        let device = make_device("Intel RealSense D435");
        let quirk = db.find_match(&device);
        assert!(quirk.is_some());
        // Should match by name pattern since we can't read sysfs in tests
        // The force_ir from name match quirk (false) or USB match (true)
        // depends on whether sysfs IDs are readable
    }

    #[test]
    fn quirks_db_case_insensitive_usb_id() {
        // USB IDs should match case-insensitively
        let mut db = QuirksDb::default();
        db.quirks.push(Quirk {
            vendor_id: Some("046D".into()),  // uppercase D
            product_id: Some("085E".into()), // uppercase E
            name_pattern: None,
            modalias: None,
            node_name: None,
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: Some("Logitech".into()),
        });

        // Can only test name fallback in tests (no sysfs)
        let device = make_device("Some other camera");
        assert!(db.find_match(&device).is_none());
    }

    #[test]
    fn quirks_db_multiple_name_patterns() {
        let mut db = QuirksDb::default();
        db.quirks.push(Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)first.*camera".into()),
            modalias: None,
            node_name: None,
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: Some("first pattern".into()),
        });
        db.quirks.push(Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)second.*camera.*".into()),
            modalias: None,
            node_name: None,
            force_ir: Some(false),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            y16_bit_depth: None,
            rotation: None,
            notes: Some("second pattern".into()),
        });

        // "first.*camera" (no trailing wildcard) anchors to the end too —
        // still matches because the device name happens to end in "Camera".
        let dev1 = make_device("First IR Camera");
        let q1 = db.find_match(&dev1);
        assert!(q1.is_some());
        assert_eq!(q1.unwrap().force_ir, Some(true));

        let dev2 = make_device("Second Camera Module");
        let q2 = db.find_match(&dev2);
        assert!(q2.is_some());
        assert_eq!(q2.unwrap().force_ir, Some(false));

        let dev3 = make_device("Third Webcam");
        assert!(db.find_match(&dev3).is_none());
    }

    #[test]
    fn name_matches_multiple_wildcards_anchored() {
        // Anchored: the final part must now match at the very end of the name.
        assert!(name_matches("(?i)a.*b.*c", "Alpha Beta Attic"));
        assert!(!name_matches("(?i)a.*b.*c", "Alpha Delta"));
        assert!(!name_matches("(?i)a.*b.*c", "Alpha Beta Camera"));
        assert!(name_matches("(?i)a.*b.*c.*", "Alpha Beta Camera"));
    }

    #[test]
    fn name_matches_empty_pattern() {
        // Empty pattern should match anything (all parts empty after split)
        assert!(name_matches("", "Any Camera"));
        assert!(name_matches(".*", "Any Camera"));
    }

    #[test]
    fn device_fingerprint_tolerates_missing_sysfs() {
        // A path with no sysfs backing degrades to an all-None fingerprint
        // (canonical "::"), which stores as NULL and is legacy-governed.
        let fp = device_fingerprint("/dev/nonexistent_test_video");
        assert!(fp.is_unknown());
        assert!(!fp.has_serial());
        assert_eq!(fp.canonical(), "::");
        assert_eq!(fp.canonical_for_storage(), None);
    }

    #[test]
    fn device_fingerprint_rejects_non_dev_path() {
        let fp = device_fingerprint("relative/path");
        assert!(fp.is_unknown());
    }

    /// An `/etc/facelock/quirks.d` entry must beat the shipped
    /// `/usr/share/facelock/quirks.d` entry for the same device, in both
    /// match passes.
    ///
    /// `load()` loads the shipped directory first and the operator's second,
    /// and the shipped TOML comments direct operators to `/etc` to correct a
    /// bad entry. That promise is only kept if matching honors load order, so
    /// this exercises the real `load_dir` path rather than an in-memory
    /// vector. The override under test disables a shipped `force_ir = true`,
    /// which is the case that matters most: `force_ir = false` is
    /// authoritative unconditionally, so a shadowed override is silent.
    #[test]
    fn etc_override_beats_shipped_quirk_for_same_device() {
        let root =
            std::env::temp_dir().join(format!("facelock-quirks-load-order-{}", std::process::id()));
        let shipped = root.join("usr-share");
        let etc = root.join("etc");
        std::fs::create_dir_all(&shipped).unwrap();
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(
            shipped.join("00-defaults.toml"),
            r#"
[[quirk]]
name_pattern = "(?i).* ir camera.*"
force_ir = true
warmup_frames = 8
notes = "shipped"

[[quirk]]
vendor_id = "dead"
product_id = "beef"
force_ir = true
notes = "shipped by usb id"
"#,
        )
        .unwrap();
        std::fs::write(
            etc.join("99-local.toml"),
            r#"
[[quirk]]
name_pattern = "(?i).* ir camera.*"
force_ir = false
notes = "local override"

[[quirk]]
vendor_id = "dead"
product_id = "beef"
force_ir = false
notes = "local override by usb id"
"#,
        )
        .unwrap();

        let mut db = QuirksDb::default();
        db.load_dir(&shipped);
        db.load_dir(&etc); // same order as QuirksDb::load()
        std::fs::remove_dir_all(&root).ok();

        // Name pass.
        let device = make_device("Integrated IR Camera");
        let (quirk, kind) = db
            .find_match_with_kind(&device, None, None)
            .expect("a name pattern matches");
        assert_eq!(kind, QuirkMatchKind::NameOnly);
        assert_eq!(
            quirk.notes.as_deref(),
            Some("local override"),
            "the /etc name entry must win over the shipped one"
        );
        assert_eq!(quirk.force_ir, Some(false));

        // USB-ID pass.
        let ids = ("dead".to_string(), "beef".to_string());
        let (quirk, kind) = db
            .find_match_with_kind(&device, Some(&ids), None)
            .expect("a USB id matches");
        assert_eq!(kind, QuirkMatchKind::UsbId);
        assert_eq!(
            quirk.notes.as_deref(),
            Some("local override by usb id"),
            "the /etc USB-ID entry must win over the shipped one"
        );
        assert_eq!(quirk.force_ir, Some(false));
    }

    #[test]
    fn quirk_deserialization_platform_identity() {
        let toml = r#"
[[quirk]]
modalias = "auxiliary:intel_ipu6.isys"
node_name = "Intel IPU6 ISYS Capture 0"
force_ir = true
"#;
        let file: QuirksFile = toml::from_str(toml).unwrap();
        assert_eq!(
            file.quirk[0].modalias.as_deref(),
            Some("auxiliary:intel_ipu6.isys")
        );
        assert_eq!(
            file.quirk[0].node_name.as_deref(),
            Some("Intel IPU6 ISYS Capture 0")
        );
        assert_eq!(file.quirk[0].vendor_id, None);
    }

    fn platform_db() -> QuirksDb {
        let quirks = toml::from_str::<QuirksFile>(
            r#"
[[quirk]]
modalias = "auxiliary:intel_ipu6.isys"
node_name = "Intel IPU6 ISYS Capture 0"
force_ir = true
notes = "full"

[[quirk]]
modalias = "auxiliary:intel_ipu6.isys"
force_ir = true
notes = "modalias only"

[[quirk]]
node_name = "Intel IPU6 ISYS Capture 1"
force_ir = true
notes = "node_name only"
"#,
        )
        .unwrap()
        .quirk;
        QuirksDb { quirks }
    }

    fn ipu6(node_name: &str) -> PlatformIdentity {
        PlatformIdentity {
            modalias: "auxiliary:intel_ipu6.isys".into(),
            node_name: node_name.into(),
        }
    }

    #[test]
    fn platform_match_requires_both_keys_exactly() {
        let db = platform_db();
        let device = make_device("Intel IPU6 ISYS Capture 0");

        let (quirk, kind) = db
            .find_match_with_kind(&device, None, Some(&ipu6("Intel IPU6 ISYS Capture 0")))
            .expect("modalias + node_name match");
        assert_eq!(kind, QuirkMatchKind::PlatformId);
        assert_eq!(quirk.notes.as_deref(), Some("full"));

        // One key alone never matches, and node_name is not a pattern.
        for node in [
            "Intel IPU6 ISYS Capture 1",
            "Intel IPU6 ISYS Capture 2",
            "intel ipu6 isys capture 0",
        ] {
            assert!(
                db.find_match_with_kind(&device, None, Some(&ipu6(node)))
                    .is_none()
            );
        }
        let other_bus = PlatformIdentity {
            modalias: "auxiliary:intel_ipu7.isys".into(),
            node_name: "Intel IPU6 ISYS Capture 0".into(),
        };
        assert!(
            db.find_match_with_kind(&device, None, Some(&other_bus))
                .is_none()
        );
        // No identity (a virtual node) matches nothing.
        assert!(db.find_match_with_kind(&device, None, None).is_none());
    }

    #[test]
    fn usb_id_match_beats_platform_and_platform_beats_name() {
        let mut db = platform_db();
        db.quirks.push(Quirk {
            name_pattern: Some("(?i)intel ipu6.*".into()),
            modalias: None,
            node_name: None,
            notes: Some("name".into()),
            ..db.quirks[0].clone()
        });
        db.quirks.push(Quirk {
            vendor_id: Some("dead".into()),
            product_id: Some("beef".into()),
            modalias: None,
            node_name: None,
            notes: Some("usb".into()),
            ..db.quirks[0].clone()
        });
        let device = make_device("Intel IPU6 ISYS Capture 0");
        let id = ipu6("Intel IPU6 ISYS Capture 0");
        let usb = ("dead".to_string(), "beef".to_string());

        let (_, kind) = db
            .find_match_with_kind(&device, Some(&usb), Some(&id))
            .unwrap();
        assert_eq!(kind, QuirkMatchKind::UsbId);
        let (_, kind) = db.find_match_with_kind(&device, None, Some(&id)).unwrap();
        assert_eq!(kind, QuirkMatchKind::PlatformId);
        let (_, kind) = db.find_match_with_kind(&device, None, None).unwrap();
        assert_eq!(kind, QuirkMatchKind::NameOnly);
    }

    /// A scratch sysfs tree: `devices/...` holds parent devices, `class/videoN`
    /// is the node directory with its `name` and optional `device` link.
    struct FakeSysfs(std::path::PathBuf);

    impl FakeSysfs {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("facelock-platform-id-{tag}-{}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn parent(&self, rel: &str, modalias: Option<&str>) -> std::path::PathBuf {
            let dir = self.0.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            if let Some(m) = modalias {
                std::fs::write(dir.join("modalias"), format!("{m}\n")).unwrap();
            }
            dir
        }

        /// A parent whose bus writes the identity only into `uevent`, as the
        /// auxiliary bus does; `modalias` is the `MODALIAS=` value, if any.
        fn parent_with_uevent(
            &self,
            rel: &str,
            uevent_modalias: Option<&str>,
            modalias_file: Option<&str>,
        ) -> std::path::PathBuf {
            let dir = self.parent(rel, modalias_file);
            let mut uevent = String::from("DRIVER=intel_ipu6_isys\n");
            if let Some(m) = uevent_modalias {
                uevent.push_str(&format!("MODALIAS={m}\n"));
            }
            std::fs::write(dir.join("uevent"), uevent).unwrap();
            dir
        }

        fn node(&self, name: &str, parent: Option<&Path>) -> std::path::PathBuf {
            let dir = self.0.join("class").join("video9");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("name"), format!("{name}\n")).unwrap();
            if let Some(p) = parent {
                std::os::unix::fs::symlink(p, dir.join("device")).unwrap();
            }
            dir
        }
    }

    impl Drop for FakeSysfs {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn platform_identity_reads_parent_modalias_and_node_name() {
        let fs = FakeSysfs::new("real");
        let parent = fs.parent(
            "devices/pci0000:00/0000:00:05.0/intel_ipu6.isys.40",
            Some("auxiliary:intel_ipu6.isys"),
        );
        let node = fs.node("Intel IPU6 ISYS Capture 0", Some(&parent));
        assert_eq!(
            platform_identity_from_sysfs(&node),
            Some(ipu6("Intel IPU6 ISYS Capture 0"))
        );
    }

    #[test]
    fn platform_identity_falls_back_to_uevent_modalias() {
        // An auxiliary-bus parent has no `modalias` file; the kernel writes
        // MODALIAS= into its uevent only.
        let fs = FakeSysfs::new("uevent");
        let parent = fs.parent_with_uevent(
            "devices/pci0000:00/0000:00:05.0/intel_ipu6.isys.40",
            Some("auxiliary:intel_ipu6.isys"),
            None,
        );
        let node = fs.node("Intel IPU6 ISYS Capture 0", Some(&parent));
        assert_eq!(
            platform_identity_from_sysfs(&node),
            Some(ipu6("Intel IPU6 ISYS Capture 0"))
        );

        // A uevent without a MODALIAS= line gives no identity.
        let fs = FakeSysfs::new("uevent-none");
        let parent = fs.parent_with_uevent("devices/platform/soc/acb7000.isp", None, None);
        let node = fs.node("msm_vfe0_video0", Some(&parent));
        assert_eq!(platform_identity_from_sysfs(&node), None);

        // The uevent fallback still refuses a USB parent.
        let fs = FakeSysfs::new("uevent-usb");
        let parent = fs.parent_with_uevent(
            "devices/pci0000:00/usb1/1-1/1-1:1.0",
            Some("usb:v046Dp085Ed0317dcEFdsc02dp01ic0Eisc01ip00in00"),
            None,
        );
        let node = fs.node("Logitech BRIO", Some(&parent));
        assert_eq!(platform_identity_from_sysfs(&node), None);
    }

    #[test]
    fn platform_identity_prefers_modalias_file_over_uevent() {
        let fs = FakeSysfs::new("both");
        let parent = fs.parent_with_uevent(
            "devices/platform/soc/acb7000.isp",
            Some("of:NcamssT(null)Cstale,camss"),
            Some("of:NcamssT(null)Cqcom,x1e80100-camss"),
        );
        let node = fs.node("msm_vfe0_video0", Some(&parent));
        assert_eq!(
            platform_identity_from_sysfs(&node),
            Some(PlatformIdentity {
                modalias: "of:NcamssT(null)Cqcom,x1e80100-camss".into(),
                node_name: "msm_vfe0_video0".into(),
            })
        );
    }

    #[test]
    fn platform_identity_refuses_virtual_nodes() {
        // v4l2loopback: no `device` link at all.
        let fs = FakeSysfs::new("loopback");
        let node = fs.node("Intel IPU6 ISYS Capture 0", None);
        assert_eq!(platform_identity_from_sysfs(&node), None);

        // A link that resolves under devices/virtual is refused even if a
        // modalias file is present there.
        let fs = FakeSysfs::new("virtual");
        let parent = fs.parent(
            "devices/virtual/video4linux/video9",
            Some("auxiliary:intel_ipu6.isys"),
        );
        let node = fs.node("Intel IPU6 ISYS Capture 0", Some(&parent));
        assert_eq!(platform_identity_from_sysfs(&node), None);
    }

    #[test]
    fn platform_identity_skips_usb_and_unreadable_parents() {
        let fs = FakeSysfs::new("usb");
        let parent = fs.parent(
            "devices/pci0000:00/usb1/1-1/1-1:1.0",
            Some("usb:v046Dp085Ed0317dcEFdsc02dp01ic0Eisc01ip00in00"),
        );
        let node = fs.node("Logitech BRIO", Some(&parent));
        assert_eq!(platform_identity_from_sysfs(&node), None);

        let fs = FakeSysfs::new("nomodalias");
        let parent = fs.parent("devices/platform/soc/acb7000.isp", None);
        let node = fs.node("msm_vfe0_video0", Some(&parent));
        assert_eq!(platform_identity_from_sysfs(&node), None);
    }

    #[test]
    fn platform_identity_tolerates_missing_sysfs() {
        assert_eq!(read_platform_identity("/dev/nonexistent_test_video"), None);
        assert_eq!(read_platform_identity("relative/path"), None);
    }

    #[test]
    fn quirks_load_dir_nonexistent() {
        let mut db = QuirksDb::default();
        db.load_dir(Path::new("/nonexistent/quirks/dir"));
        assert!(
            db.quirks.is_empty(),
            "nonexistent dir should load zero quirks"
        );
    }

    #[test]
    fn quirk_with_all_optional_fields() {
        let toml = r#"
[[quirk]]
vendor_id = "1234"
product_id = "5678"
name_pattern = "(?i)test"
force_ir = true
emitter_xu_guid = "abcd-1234"
emitter_xu_selector = 3
warmup_frames = 15
format_preference = "GREY"
y16_bit_depth = 12
rotation = 90
notes = "Full test quirk"
"#;
        let file: QuirksFile = toml::from_str(toml).unwrap();
        let q = &file.quirk[0];
        assert_eq!(q.emitter_xu_guid.as_deref(), Some("abcd-1234"));
        assert_eq!(q.emitter_xu_selector, Some(3));
        assert_eq!(q.warmup_frames, Some(15));
        assert_eq!(q.format_preference.as_deref(), Some("GREY"));
        assert_eq!(q.y16_bit_depth, Some(12));
        assert_eq!(q.rotation, Some(90));
    }

    #[test]
    fn load_defaults_file() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/quirks.d/00-defaults.toml");
        if path.exists() {
            let quirks = QuirksDb::load_file(&path).unwrap();
            assert!(!quirks.is_empty(), "defaults file should have quirks");
        }
    }

    /// Ordinary RGB webcam names that no shipped `name_pattern` may match.
    /// Includes the specific decoys ("Sirius Camera", "AIR-Cam") that the
    /// pre-anchoring substring matcher was fooled by (#99). Mirrors
    /// `device::tests::ir_classification_corpus`.
    const QUIRK_NAME_DECOYS: &[&str] = &[
        "Integrated Webcam",
        "USB2.0 HD UVC WebCam",
        "AIR-Cam",
        "Sirius Camera",
        "Chicony USB2.0 Camera",
    ];

    fn shipped_default_quirks() -> Option<Vec<Quirk>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/quirks.d/00-defaults.toml");
        if !path.exists() {
            return None;
        }
        Some(QuirksDb::load_file(&path).expect("defaults file parses"))
    }

    /// Every `format_preference` shipped in `config/quirks.d/00-defaults.toml`
    /// must be both a format facelock can actually decode and an IR-typical
    /// format that may safely act as node-level IR evidence.
    ///
    /// This is the coupling #90 introduced and nothing else guards: the file
    /// writes the padded V4L2 spelling (`"Y16 "`), `load_file` normalizes it,
    /// and `capture::quirk_format_preference` then checks it against
    /// `DECODABLE_FORMATS` and *drops it with a warning* if it does not match.
    /// A shipped preference that fails this check does not fail loudly at
    /// runtime — it silently stops selecting the RealSense IR sensor node.
    #[test]
    fn shipped_quirk_format_preferences_are_decodable_and_ir_typical() {
        let Some(quirks) = shipped_default_quirks() else {
            return;
        };
        let mut checked = 0;
        for q in &quirks {
            let Some(pref) = q.format_preference.as_deref() else {
                continue;
            };
            checked += 1;
            assert!(
                crate::capture::DECODABLE_FORMATS.contains(&pref.trim()),
                "shipped quirk {:?} prefers {pref:?}, which facelock cannot decode — \
                 it would be dropped at open with a warning",
                q.notes
            );
            assert!(
                crate::device::is_ir_typical_fourcc(pref),
                "shipped quirk {:?} prefers {pref:?}, which is not IR-typical — \
                 it must not act as node-level IR evidence",
                q.notes
            );
        }
        assert!(
            checked >= 4,
            "expected the shipped RealSense (Y16) and BRIO (GREY) preferences to be \
             covered; only {checked} were found"
        );
    }

    /// `load_file` is what turns the file's padded `"Y16 "` into the `"Y16"`
    /// every comparison downstream expects. Asserted against the real shipped
    /// file, not a fixture, because the padded spelling in *that* file is what
    /// makes the normalization load-bearing.
    #[test]
    fn shipped_quirk_format_preferences_are_normalized_by_the_loader() {
        let Some(quirks) = shipped_default_quirks() else {
            return;
        };
        for q in &quirks {
            if let Some(pref) = q.format_preference.as_deref() {
                assert_eq!(pref, pref.trim(), "loader left padding on {pref:?}");
            }
        }
    }

    /// The repository has no Y16 hardware validation record yet. Keep the
    /// shipped RealSense entries explicitly unverified instead of inventing a
    /// sensor depth from their 16-bit V4L2 container.
    #[test]
    fn shipped_y16_quirks_do_not_claim_an_unverified_bit_depth() {
        let Some(quirks) = shipped_default_quirks() else {
            return;
        };
        let y16: Vec<_> = quirks
            .iter()
            .filter(|q| q.format_preference.as_deref() == Some("Y16"))
            .collect();

        assert_eq!(y16.len(), 3, "expected the three shipped RealSense entries");
        for q in y16 {
            assert_eq!(
                q.y16_bit_depth, None,
                "shipped quirk {:?} claims Y16 depth without validation evidence",
                q.notes
            );
        }
    }

    /// Table-driven regression for #99: every shipped `name_pattern` quirk in
    /// `config/quirks.d/00-defaults.toml` must still match a device name of
    /// the shape it is written for, and none of them may match an ordinary
    /// RGB webcam name.
    ///
    /// Keyed on the EXACT pattern string, not a substring of it. The earlier
    /// substring keying silently collapsed several patterns onto one case, so
    /// a pattern could be added without any name being asserted against it.
    #[test]
    fn shipped_quirk_name_patterns_match_intended_devices_and_reject_decoys() {
        let Some(quirks) = shipped_default_quirks() else {
            return;
        };

        // (exact shipped pattern, a device name of the shape it targets)
        let cases: &[(&str, &str)] = &[
            ("(?i)ir camera.*", "IR Camera"),
            ("(?i).* ir camera.*", "Integrated IR Camera"),
            ("(?i)surface.* ir.*", "Surface Pro IR Camera"),
            ("(?i).* surface.* ir.*", "Microsoft Surface IR Camera"),
            ("(?i)hp.* ir.*", "HP IR Camera 5MP"),
            ("(?i).* hp.* ir.*", "Chicony HP IR Camera"),
            ("(?i)dell.* ir.*", "Dell IR Camera"),
            ("(?i).* dell.* ir.*", "Integrated Dell IR Camera"),
        ];

        let mut exercised = 0;
        for quirk in &quirks {
            let Some(pattern) = &quirk.name_pattern else {
                continue;
            };
            let (_, expected_name) = cases
                .iter()
                .find(|(shipped, _)| shipped == pattern)
                .unwrap_or_else(|| {
                    panic!("no test case wired up for shipped pattern {pattern:?} — add one")
                });
            assert!(
                name_matches(pattern, expected_name),
                "pattern {pattern:?} should match {expected_name:?}"
            );
            for decoy in QUIRK_NAME_DECOYS {
                assert!(
                    !name_matches(pattern, decoy),
                    "pattern {pattern:?} must NOT match decoy {decoy:?}"
                );
            }
            exercised += 1;
        }
        assert_eq!(
            exercised,
            cases.len(),
            "every expected shipped name_pattern quirk was exercised"
        );
    }

    /// Card strings real IR cameras report, each of which must be matched by
    /// at least ONE shipped `name_pattern`.
    ///
    /// The companion test above only proves each pattern matches *something*.
    /// After #99's anchoring the shipped patterns were checked against names
    /// derived from the patterns themselves, so nothing caught that
    /// `(?i)ir camera.*` — the ThinkPad entry — could no longer match the
    /// "Integrated IR Camera" nodes its own notes name. That false negative
    /// is silent: the device still works, it just loses `warmup_frames`. This
    /// test is keyed on hardware-reported names instead, so a pattern that
    /// matches no real device fails here.
    #[test]
    fn shipped_quirk_name_patterns_match_real_hardware_names() {
        let Some(quirks) = shipped_default_quirks() else {
            return;
        };
        let patterns: Vec<&str> = quirks
            .iter()
            .filter_map(|q| q.name_pattern.as_deref())
            .collect();

        // V4L2 card strings are capped at 31 chars, hence the truncated forms.
        let real_names = [
            "IR Camera",
            "Integrated IR Camera",
            "Integrated IR Camera: Integrate",
            "Chicony HP IR Camera",
            "Microsoft Surface IR Camera",
        ];

        for name in real_names {
            assert!(
                patterns.iter().any(|p| name_matches(p, name)),
                "no shipped name_pattern matches the real device name {name:?}"
            );
        }

        // The corpus above must not have been bought with false positives.
        for decoy in QUIRK_NAME_DECOYS {
            assert!(
                !patterns.iter().any(|p| name_matches(p, decoy)),
                "a shipped name_pattern matches decoy {decoy:?}"
            );
        }
    }
}
