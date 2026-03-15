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
    /// Regex pattern to match against device name (fallback when IDs unavailable)
    #[serde(default)]
    pub name_pattern: Option<String>,
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

/// Database of hardware quirks loaded from TOML files.
#[derive(Debug, Default)]
pub struct QuirksDb {
    quirks: Vec<Quirk>,
}

impl QuirksDb {
    /// Load quirks from both system and user directories.
    /// System: `/usr/share/facelock/quirks.d/`
    /// User overrides: `/etc/facelock/quirks.d/`
    /// Files are loaded in alphabetical order; later files can override earlier ones.
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
        let file: QuirksFile = toml::from_str(&content).map_err(|e| format!("parse error: {e}"))?;
        Ok(file.quirk)
    }

    /// Find a matching quirk for the given device.
    /// Matches by USB vendor:product ID first, then by name pattern.
    pub fn find_match(&self, device: &DeviceInfo) -> Option<&Quirk> {
        let usb_ids = read_usb_ids(&device.path);

        // First pass: match by USB vendor:product ID (most specific)
        if let Some((vendor, product)) = &usb_ids {
            for quirk in &self.quirks {
                if let (Some(qv), Some(qp)) = (&quirk.vendor_id, &quirk.product_id) {
                    if qv.eq_ignore_ascii_case(vendor) && qp.eq_ignore_ascii_case(product) {
                        debug!(
                            device = %device.path,
                            vendor, product,
                            notes = quirk.notes.as_deref().unwrap_or(""),
                            "matched quirk by USB ID"
                        );
                        return Some(quirk);
                    }
                }
            }
        }

        // Second pass: match by name pattern
        for quirk in &self.quirks {
            if let Some(pattern) = &quirk.name_pattern {
                // Simple case-insensitive substring/pattern matching
                // We avoid the regex crate dependency by using a simple approach
                if name_matches(pattern, &device.name) {
                    debug!(
                        device = %device.path,
                        name = %device.name,
                        pattern,
                        notes = quirk.notes.as_deref().unwrap_or(""),
                        "matched quirk by name pattern"
                    );
                    return Some(quirk);
                }
            }
        }

        None
    }

    /// Get all quirks for display/debugging.
    pub fn all(&self) -> &[Quirk] {
        &self.quirks
    }
}

/// Read USB vendor:product IDs from sysfs for a video device.
/// Returns (vendor_id, product_id) as hex strings, or None if unavailable.
fn read_usb_ids(device_path: &str) -> Option<(String, String)> {
    // /dev/video0 -> /sys/class/video4linux/video0/device/
    let dev_name = device_path.strip_prefix("/dev/")?;
    let sysfs_base = format!("/sys/class/video4linux/{dev_name}/device");

    // Walk up to find the USB device (may be a few levels up)
    let vendor = try_read_sysfs_attr(&sysfs_base, "idVendor")
        .or_else(|| try_read_sysfs_attr(&format!("{sysfs_base}/.."), "idVendor"));
    let product = try_read_sysfs_attr(&sysfs_base, "idProduct")
        .or_else(|| try_read_sysfs_attr(&format!("{sysfs_base}/.."), "idProduct"));

    match (vendor, product) {
        (Some(v), Some(p)) => Some((v, p)),
        _ => None,
    }
}

fn try_read_sysfs_attr(base: &str, attr: &str) -> Option<String> {
    let path = format!("{base}/{attr}");
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Simple pattern matching: supports (?i) prefix for case-insensitive, .* for wildcards.
/// This is a simplified matcher that handles the most common quirks patterns
/// without pulling in the full regex crate.
fn name_matches(pattern: &str, name: &str) -> bool {
    let (case_insensitive, pattern) = if let Some(p) = pattern.strip_prefix("(?i)") {
        (true, p)
    } else {
        (false, pattern)
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

    // Split pattern on .* and check that all parts appear in order
    let parts: Vec<&str> = pattern.split(".*").collect();
    let mut pos = 0;
    for part in parts {
        if part.is_empty() {
            continue;
        }
        match name[pos..].find(part) {
            Some(idx) => pos += idx + part.len(),
            None => return false,
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
        assert!(name_matches("(?i)integrated.*ir", "Integrated IR Camera"));
        // "infrared" doesn't contain substring "ir", so use a separate pattern
        assert!(name_matches(
            "(?i)integrated.*infrared",
            "Integrated Infrared Camera"
        ));
        // Both keywords covered by searching for just the common part
        assert!(name_matches(
            "(?i)integrated.*in",
            "Integrated Infrared Camera"
        ));
    }

    #[test]
    fn name_matches_simple_pattern() {
        assert!(name_matches("(?i)hp.*ir", "HP IR Camera 5MP"));
        assert!(name_matches("(?i)dell.*ir", "Dell IR Camera"));
        assert!(!name_matches("(?i)dell.*ir", "Logitech Webcam"));
    }

    #[test]
    fn name_matches_case_sensitive() {
        assert!(name_matches("IR Camera", "My IR Camera"));
        assert!(!name_matches("IR Camera", "My ir camera"));
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
    }

    #[test]
    fn quirks_db_find_by_name() {
        let mut db = QuirksDb::default();
        db.quirks.push(Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)hp.*ir".into()),
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: Some(8),
            format_preference: None,
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
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: Some(10),
            format_preference: Some("Y16 ".into()),
            rotation: None,
            notes: Some("Intel RealSense".into()),
        });
        // Add a name-pattern quirk that would also match
        db.quirks.push(Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)realsense".into()),
            force_ir: Some(false), // Different value to test priority
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
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
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
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
            force_ir: Some(true),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            rotation: None,
            notes: Some("first pattern".into()),
        });
        db.quirks.push(Quirk {
            vendor_id: None,
            product_id: None,
            name_pattern: Some("(?i)second.*camera".into()),
            force_ir: Some(false),
            emitter_xu_guid: None,
            emitter_xu_selector: None,
            warmup_frames: None,
            format_preference: None,
            rotation: None,
            notes: Some("second pattern".into()),
        });

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
    fn name_matches_multiple_wildcards() {
        assert!(name_matches("(?i)a.*b.*c", "Alpha Beta Camera"));
        assert!(!name_matches("(?i)a.*b.*c", "Alpha Delta"));
    }

    #[test]
    fn name_matches_empty_pattern() {
        // Empty pattern should match anything (all parts empty after split)
        assert!(name_matches("", "Any Camera"));
        assert!(name_matches(".*", "Any Camera"));
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
rotation = 90
notes = "Full test quirk"
"#;
        let file: QuirksFile = toml::from_str(toml).unwrap();
        let q = &file.quirk[0];
        assert_eq!(q.emitter_xu_guid.as_deref(), Some("abcd-1234"));
        assert_eq!(q.emitter_xu_selector, Some(3));
        assert_eq!(q.warmup_frames, Some(15));
        assert_eq!(q.format_preference.as_deref(), Some("GREY"));
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
}
