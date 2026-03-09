use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::paths;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    NotFound(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("validation error: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub device: DeviceConfig,
    #[serde(default)]
    pub recognition: RecognitionConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub notification: NotificationConfig,
    #[serde(default)]
    pub snapshots: SnapshotConfig,
    #[serde(default)]
    pub debug: DebugConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub path: String,
    #[serde(default = "default_max_height")]
    pub max_height: u32,
    #[serde(default)]
    pub rotation: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecognitionConfig {
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,
    #[serde(default = "default_confidence")]
    pub detection_confidence: f32,
    #[serde(default = "default_nms")]
    pub nms_threshold: f32,
}

impl Default for RecognitionConfig {
    fn default() -> Self {
        Self {
            threshold: default_threshold(),
            timeout_secs: default_timeout(),
            detection_confidence: default_confidence(),
            nms_threshold: default_nms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_socket")]
    pub socket_path: String,
    #[serde(default = "default_model_dir")]
    pub model_dir: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket(),
            model_dir: default_model_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default)]
    pub disabled: bool,
    #[serde(default = "default_true")]
    pub abort_if_ssh: bool,
    #[serde(default = "default_true")]
    pub abort_if_lid_closed: bool,
    #[serde(default)]
    pub suppress_unknown: bool,
    #[serde(default = "default_true")]
    pub require_ir: bool,
    #[serde(default = "default_true")]
    pub require_frame_variance: bool,
    #[serde(default = "default_min_auth_frames")]
    pub min_auth_frames: u32,
    #[serde(default = "default_true")]
    pub detection_notice: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            abort_if_ssh: true,
            abort_if_lid_closed: true,
            suppress_unknown: false,
            require_ir: true,
            require_frame_variance: true,
            min_auth_frames: default_min_auth_frames(),
            detection_notice: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnapshotConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_snapshot_dir")]
    pub dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DebugConfig {
    #[serde(default)]
    pub verbose: bool,
}

// Default value functions
fn default_max_height() -> u32 {
    480
}
fn default_threshold() -> f32 {
    0.45
}
fn default_timeout() -> u32 {
    5
}
fn default_confidence() -> f32 {
    0.5
}
fn default_nms() -> f32 {
    0.4
}
fn default_socket() -> String {
    paths::DEFAULT_SOCKET_PATH.to_string()
}
fn default_model_dir() -> String {
    paths::DEFAULT_MODEL_DIR.to_string()
}
fn default_db_path() -> String {
    paths::DEFAULT_DB_PATH.to_string()
}
fn default_snapshot_dir() -> String {
    paths::DEFAULT_SNAPSHOT_DIR.to_string()
}
fn default_min_auth_frames() -> u32 {
    3
}
fn default_true() -> bool {
    true
}

impl Config {
    /// Load config from the default path (respects `HOWDY_CONFIG` env var).
    pub fn load() -> Result<Self, ConfigError> {
        let path = paths::config_path();
        Self::load_from(&path)
    }

    /// Load config from a specific path.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ConfigError::NotFound(path.display().to_string())
            } else {
                ConfigError::Parse(format!("failed to read {}: {e}", path.display()))
            }
        })?;
        Self::parse(&content)
    }

    /// Parse config from a TOML string.
    pub fn parse(toml_str: &str) -> Result<Self, ConfigError> {
        let config: Config =
            toml::from_str(toml_str).map_err(|e| ConfigError::Parse(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate config values.
    fn validate(&self) -> Result<(), ConfigError> {
        if self.device.path.is_empty() {
            return Err(ConfigError::Validation(
                "device.path must not be empty".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.recognition.threshold) {
            return Err(ConfigError::Validation(format!(
                "recognition.threshold must be between 0.0 and 1.0, got {}",
                self.recognition.threshold
            )));
        }
        if !matches!(self.device.rotation, 0 | 90 | 180 | 270) {
            return Err(ConfigError::Validation(format!(
                "device.rotation must be 0, 90, 180, or 270, got {}",
                self.device.rotation
            )));
        }
        if self.recognition.timeout_secs == 0 {
            return Err(ConfigError::Validation(
                "recognition.timeout_secs must be > 0".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.device.path, "/dev/video0");
        assert_eq!(config.device.max_height, 480);
        assert_eq!(config.recognition.threshold, 0.45);
        assert_eq!(config.daemon.socket_path, paths::DEFAULT_SOCKET_PATH);
        assert!(config.security.require_ir);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[device]
path = "/dev/video2"
max_height = 720
rotation = 90

[recognition]
threshold = 0.5
timeout_secs = 10
detection_confidence = 0.6
nms_threshold = 0.3

[daemon]
socket_path = "/tmp/test.sock"
model_dir = "/tmp/models"

[storage]
db_path = "/tmp/test.db"

[security]
disabled = false
require_ir = false
require_frame_variance = true
min_auth_frames = 5

[notification]
enabled = false

[snapshots]
enabled = true
dir = "/tmp/snaps"

[debug]
verbose = true
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.device.path, "/dev/video2");
        assert_eq!(config.device.max_height, 720);
        assert_eq!(config.device.rotation, 90);
        assert_eq!(config.recognition.threshold, 0.5);
        assert_eq!(config.recognition.timeout_secs, 10);
        assert_eq!(config.daemon.socket_path, "/tmp/test.sock");
        assert!(!config.security.require_ir);
        assert_eq!(config.security.min_auth_frames, 5);
        assert!(config.debug.verbose);
    }

    #[test]
    fn reject_empty_device_path() {
        let toml = r#"
[device]
path = ""
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn reject_invalid_threshold() {
        let toml = r#"
[device]
path = "/dev/video0"
[recognition]
threshold = 1.5
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn reject_invalid_rotation() {
        let toml = r#"
[device]
path = "/dev/video0"
rotation = 45
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn reject_zero_timeout() {
        let toml = r#"
[device]
path = "/dev/video0"
[recognition]
timeout_secs = 0
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn missing_optional_sections_uses_defaults() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.storage.db_path, paths::DEFAULT_DB_PATH);
        assert!(config.security.abort_if_ssh);
        assert!(!config.snapshots.enabled);
    }
}
