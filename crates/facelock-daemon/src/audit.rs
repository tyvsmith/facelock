use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use facelock_core::config::AuditConfig;
use serde::Serialize;
use tracing::{debug, warn};

/// A structured audit log entry.
#[derive(Debug, Serialize)]
pub struct AuditEntry {
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Unix username
    pub user: String,
    /// Auth result: "success", "failure", "error", "rate_limited"
    pub result: String,
    /// Best cosine similarity score
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    /// Number of frames captured
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_count: Option<u32>,
    /// Authentication duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Camera device path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Matched model label (on success)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_label: Option<String>,
    /// Error message (on error)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Write an audit entry to the JSONL log file.
/// Performs size-based rotation if the file exceeds `rotate_size_mb`.
pub fn write_audit_entry(config: &AuditConfig, entry: &AuditEntry) {
    if !config.enabled {
        return;
    }

    let path = Path::new(&config.path);

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("failed to create audit log directory: {e}");
            return;
        }
    }

    // Check if rotation is needed
    if let Ok(metadata) = std::fs::metadata(path) {
        let max_bytes = config.rotate_size_mb as u64 * 1024 * 1024;
        if metadata.len() >= max_bytes {
            rotate_log(path);
        }
    }

    // Append entry
    let line = match serde_json::to_string(entry) {
        Ok(s) => s,
        Err(e) => {
            warn!("failed to serialize audit entry: {e}");
            return;
        }
    };

    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{line}") {
                warn!("failed to write audit entry: {e}");
            } else {
                debug!("wrote audit entry for user {}", entry.user);
            }
        }
        Err(e) => {
            warn!("failed to open audit log {}: {e}", path.display());
        }
    }
}

/// Rotate the log file by renaming current to .1 (overwriting any existing .1).
fn rotate_log(path: &Path) {
    let rotated = path.with_extension("jsonl.1");
    if let Err(e) = std::fs::rename(path, &rotated) {
        warn!("failed to rotate audit log: {e}");
    } else {
        debug!("rotated audit log to {}", rotated.display());
    }
}

/// Get the current time as an ISO 8601 string.
pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple UTC timestamp without chrono dependency
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since epoch to y-m-d (simplified)
    let (y, m, d) = days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audit_entry_serializes_to_json() {
        let entry = AuditEntry {
            timestamp: "2026-03-12T10:00:00Z".into(),
            user: "alice".into(),
            result: "success".into(),
            similarity: Some(0.92),
            frame_count: Some(5),
            duration_ms: Some(1200),
            device: Some("/dev/video0".into()),
            model_label: Some("front".into()),
            error: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"user\":\"alice\""));
        assert!(json.contains("\"result\":\"success\""));
        assert!(!json.contains("error")); // None fields skipped
    }

    #[test]
    fn audit_write_creates_file() {
        let dir = std::env::temp_dir().join("facelock_audit_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config = AuditConfig {
            enabled: true,
            path: dir.join("test.jsonl").to_string_lossy().to_string(),
            rotate_size_mb: 10,
        };

        let entry = AuditEntry {
            timestamp: now_iso8601(),
            user: "testuser".into(),
            result: "success".into(),
            similarity: Some(0.85),
            frame_count: Some(3),
            duration_ms: Some(500),
            device: None,
            model_label: None,
            error: None,
        };

        write_audit_entry(&config, &entry);

        let mut contents = String::new();
        use std::io::Read;
        std::fs::File::open(&config.path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(contents.contains("testuser"));
        assert!(contents.ends_with('\n'));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audit_rotation_at_size_limit() {
        let dir = std::env::temp_dir().join("facelock_audit_rotate_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let log_path = dir.join("audit.jsonl");
        let rotated_path = dir.join("audit.jsonl.1");

        // Create a config with tiny rotation size (1 byte effectively)
        let config = AuditConfig {
            enabled: true,
            path: log_path.to_string_lossy().to_string(),
            rotate_size_mb: 0, // 0 MB = rotate immediately after first write
        };

        let entry1 = AuditEntry {
            timestamp: now_iso8601(),
            user: "first".into(),
            result: "success".into(),
            similarity: None,
            frame_count: None,
            duration_ms: None,
            device: None,
            model_label: None,
            error: None,
        };

        // Write first entry
        write_audit_entry(&config, &entry1);
        assert!(log_path.exists(), "log file should be created");

        // Write second entry — should trigger rotation since file > 0 bytes
        let entry2 = AuditEntry {
            timestamp: now_iso8601(),
            user: "second".into(),
            result: "failure".into(),
            similarity: Some(0.5),
            frame_count: None,
            duration_ms: None,
            device: None,
            model_label: None,
            error: None,
        };
        write_audit_entry(&config, &entry2);

        // Rotated file should exist
        assert!(rotated_path.exists(), "rotated .1 file should exist after rotation");

        // New log should have the latest entry
        let new_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(new_content.contains("second"), "new log should have second entry");

        // Rotated log should have the first entry
        let rotated_content = std::fs::read_to_string(&rotated_path).unwrap();
        assert!(rotated_content.contains("first"), "rotated log should have first entry");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audit_multiple_entries_append() {
        let dir = std::env::temp_dir().join("facelock_audit_append_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config = AuditConfig {
            enabled: true,
            path: dir.join("test.jsonl").to_string_lossy().to_string(),
            rotate_size_mb: 10, // large enough to not rotate
        };

        for i in 0..5 {
            let entry = AuditEntry {
                timestamp: now_iso8601(),
                user: format!("user{i}"),
                result: "success".into(),
                similarity: Some(0.8 + i as f32 * 0.01),
                frame_count: Some(3),
                duration_ms: Some(500),
                device: None,
                model_label: None,
                error: None,
            };
            write_audit_entry(&config, &entry);
        }

        let content = std::fs::read_to_string(&config.path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5, "should have 5 JSONL entries");
        assert!(lines[0].contains("user0"));
        assert!(lines[4].contains("user4"));

        // Each line should be valid JSON
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.is_object());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audit_entry_with_all_fields() {
        let entry = AuditEntry {
            timestamp: "2026-03-12T12:00:00Z".into(),
            user: "alice".into(),
            result: "success".into(),
            similarity: Some(0.92),
            frame_count: Some(5),
            duration_ms: Some(1234),
            device: Some("/dev/video2".into()),
            model_label: Some("front-profile".into()),
            error: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"similarity\":0.92"));
        assert!(json.contains("\"frame_count\":5"));
        assert!(json.contains("\"duration_ms\":1234"));
        assert!(json.contains("\"device\":\"/dev/video2\""));
        assert!(json.contains("\"model_label\":\"front-profile\""));
        assert!(!json.contains("\"error\""), "error should be omitted when None");
    }

    #[test]
    fn audit_entry_error_case() {
        let entry = AuditEntry {
            timestamp: "2026-03-12T12:00:00Z".into(),
            user: "bob".into(),
            result: "error".into(),
            similarity: None,
            frame_count: None,
            duration_ms: None,
            device: None,
            model_label: None,
            error: Some("camera not found".into()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"error\":\"camera not found\""));
        assert!(!json.contains("\"similarity\""), "similarity should be omitted when None");
    }

    #[test]
    fn audit_disabled_does_nothing() {
        let config = AuditConfig {
            enabled: false,
            path: "/tmp/should_not_exist.jsonl".into(),
            rotate_size_mb: 10,
        };
        let entry = AuditEntry {
            timestamp: now_iso8601(),
            user: "test".into(),
            result: "success".into(),
            similarity: None,
            frame_count: None,
            duration_ms: None,
            device: None,
            model_label: None,
            error: None,
        };
        write_audit_entry(&config, &entry);
        assert!(!Path::new("/tmp/should_not_exist.jsonl").exists());
    }

    #[test]
    fn now_iso8601_format() {
        let ts = now_iso8601();
        assert!(ts.contains('T'));
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
    }

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        // 2026-03-12 is day 20524 from epoch
        let (y, m, d) = days_to_ymd(20524);
        assert_eq!(y, 2026);
        assert_eq!(m, 3);
        assert_eq!(d, 12);
    }
}
