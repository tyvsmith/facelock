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
    #[serde(default)]
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
    pub tpm: TpmConfig,
    #[serde(default)]
    pub encryption: EncryptionConfig,
    #[serde(default)]
    pub audit: AuditConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default = "default_max_height")]
    pub max_height: u32,
    #[serde(default)]
    pub rotation: u16,
    /// Number of frames to discard after camera open for AGC/AE stabilization.
    #[serde(default = "default_warmup_frames")]
    pub warmup_frames: u32,
    /// Percentage of pixels that must be dark (< dark_pixel_value) to reject a frame.
    /// Range: 0.0 to 1.0. Default: 0.6 (60%).
    #[serde(default = "default_dark_threshold")]
    pub dark_threshold: f32,
    /// Pixel brightness value below which a pixel is considered "dark".
    /// Range: 0-255. Default: 10.
    #[serde(default = "default_dark_pixel_value")]
    pub dark_pixel_value: u8,
    /// Enable IR emitter control. When true, attempts to activate IR LED
    /// emitters when camera opens and deactivate when camera closes.
    /// Most cameras auto-enable emitters during streaming; enable this
    /// only if your camera requires explicit control.
    #[serde(default)]
    pub ir_emitter: bool,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            path: None,
            max_height: default_max_height(),
            rotation: 0,
            warmup_frames: default_warmup_frames(),
            dark_threshold: default_dark_threshold(),
            dark_pixel_value: default_dark_pixel_value(),
            ir_emitter: false,
        }
    }
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
    #[serde(default = "default_detector_model")]
    pub detector_model: String,
    #[serde(default = "default_embedder_model")]
    pub embedder_model: String,
    /// ORT execution provider: "cpu", "cuda", or "tensorrt".
    #[serde(default = "default_execution_provider")]
    pub execution_provider: String,
    /// Number of intra-op threads for ORT inference.
    #[serde(default = "default_threads")]
    pub threads: u32,
}

impl Default for RecognitionConfig {
    fn default() -> Self {
        Self {
            threshold: default_threshold(),
            timeout_secs: default_timeout(),
            detection_confidence: default_confidence(),
            nms_threshold: default_nms(),
            detector_model: default_detector_model(),
            embedder_model: default_embedder_model(),
            execution_provider: default_execution_provider(),
            threads: default_threads(),
        }
    }
}

/// How the PAM module reaches the face engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DaemonMode {
    /// Connect to a running facelock-daemon via D-Bus system bus.
    #[default]
    Daemon,
    /// Run facelock-auth per PAM call (no daemon needed).
    Oneshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_model_dir")]
    pub model_dir: String,
    #[serde(default)]
    pub idle_timeout_secs: u64,
    #[serde(default)]
    pub mode: DaemonMode,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            model_dir: default_model_dir(),
            idle_timeout_secs: 0,
            mode: DaemonMode::default(),
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
    /// Require landmark movement between frames to pass liveness check.
    #[serde(default = "default_true")]
    pub require_landmark_liveness: bool,
    #[serde(default = "default_min_auth_frames")]
    pub min_auth_frames: u32,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_window_secs")]
    pub window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            window_secs: default_window_secs(),
        }
    }
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
            require_landmark_liveness: true,
            min_auth_frames: default_min_auth_frames(),
            rate_limit: RateLimitConfig::default(),
        }
    }
}

/// Controls how auth feedback is delivered.
///
/// - `"off"` — no notifications at all
/// - `"terminal"` — PAM conversation text only ("Identifying face...", "Face recognized.")
/// - `"desktop"` — desktop popups only (via D-Bus/notify-send)
/// - `"both"` — terminal text and desktop popups
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NotificationMode {
    Off,
    #[default]
    Terminal,
    Desktop,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    #[serde(default)]
    pub mode: NotificationMode,
    /// Show prompt text/notification when scanning starts ("Identifying face...")
    #[serde(default = "default_true")]
    pub notify_prompt: bool,
    /// Show notification on successful face match
    #[serde(default = "default_true")]
    pub notify_on_success: bool,
    /// Show notification on failed face match
    #[serde(default = "default_true")]
    pub notify_on_failure: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            mode: NotificationMode::Terminal,
            notify_prompt: true,
            notify_on_success: true,
            notify_on_failure: false,
        }
    }
}

impl NotificationConfig {
    /// Whether terminal text (PAM conversation) is enabled
    pub fn terminal(&self) -> bool {
        matches!(
            self.mode,
            NotificationMode::Terminal | NotificationMode::Both
        )
    }

    /// Whether desktop popups are enabled
    pub fn desktop(&self) -> bool {
        matches!(
            self.mode,
            NotificationMode::Desktop | NotificationMode::Both
        )
    }
}

/// When to save camera snapshots.
///
/// - `"off"` — never save snapshots (default)
/// - `"all"` — save on every auth attempt
/// - `"failure"` — save only on failed auth (debugging false rejects)
/// - `"success"` — save only on successful auth (auditing)
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotMode {
    #[default]
    Off,
    All,
    Failure,
    Success,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    #[serde(default)]
    pub mode: SnapshotMode,
    #[serde(default = "default_snapshot_dir")]
    pub dir: String,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            mode: SnapshotMode::Off,
            dir: default_snapshot_dir(),
        }
    }
}

impl SnapshotConfig {
    /// Whether snapshots should be saved for a given auth outcome.
    pub fn should_save(&self, success: bool) -> bool {
        match self.mode {
            SnapshotMode::Off => false,
            SnapshotMode::All => true,
            SnapshotMode::Success => success,
            SnapshotMode::Failure => !success,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TpmConfig {
    #[serde(default)]
    pub seal_database: bool,
    #[serde(default)]
    pub pcr_binding: bool,
    #[serde(default = "default_pcr_indices")]
    pub pcr_indices: Vec<u32>,
    #[serde(default = "default_tcti")]
    pub tcti: String,
}

impl Default for TpmConfig {
    fn default() -> Self {
        Self {
            seal_database: false,
            pcr_binding: false,
            pcr_indices: default_pcr_indices(),
            tcti: default_tcti(),
        }
    }
}

/// Method for encrypting face embeddings at rest.
///
/// - `"none"` — no encryption (default, embeddings stored as plaintext)
/// - `"keyfile"` — AES-256-GCM with a key file
/// - `"tpm"` — AES-256-GCM with TPM-sealed key (key sealed by TPM, embeddings encrypted with AES)
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncryptionMethod {
    #[default]
    None,
    Keyfile,
    Tpm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionConfig {
    #[serde(default)]
    pub method: EncryptionMethod,
    /// Path to AES-256-GCM key file for `keyfile` method.
    /// Generated by `facelock setup` or `facelock encrypt --generate-key`.
    #[serde(default = "default_encryption_key_path")]
    pub key_path: String,
    /// Path to TPM-sealed AES key for `tpm` method.
    /// Generated by `facelock setup` or `facelock tpm seal-key`.
    #[serde(default = "default_sealed_key_path")]
    pub sealed_key_path: String,
}

impl Default for EncryptionConfig {
    fn default() -> Self {
        Self {
            method: EncryptionMethod::None,
            key_path: default_encryption_key_path(),
            sealed_key_path: default_sealed_key_path(),
        }
    }
}

// Default value functions
fn default_max_height() -> u32 {
    480
}
fn default_warmup_frames() -> u32 {
    5
}
fn default_dark_threshold() -> f32 {
    0.6
}
fn default_dark_pixel_value() -> u8 {
    10
}
fn default_threshold() -> f32 {
    0.80
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
fn default_max_attempts() -> u32 {
    5
}
fn default_window_secs() -> u64 {
    60
}
fn default_pcr_indices() -> Vec<u32> {
    vec![0, 1, 2, 3, 7]
}
fn default_tcti() -> String {
    "device:/dev/tpmrm0".to_string()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    /// Enable structured audit logging to JSONL file.
    #[serde(default)]
    pub enabled: bool,
    /// Path to the audit log file.
    #[serde(default = "default_audit_path")]
    pub path: String,
    /// Maximum log file size in MB before rotation.
    #[serde(default = "default_audit_rotate_size")]
    pub rotate_size_mb: u32,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_audit_path(),
            rotate_size_mb: default_audit_rotate_size(),
        }
    }
}

fn default_audit_path() -> String {
    "/var/log/facelock/audit.jsonl".to_string()
}
fn default_audit_rotate_size() -> u32 {
    10
}
fn default_encryption_key_path() -> String {
    "/etc/facelock/encryption.key".to_string()
}
fn default_sealed_key_path() -> String {
    "/etc/facelock/encryption.key.sealed".to_string()
}
fn default_detector_model() -> String {
    "scrfd_2.5g_bnkps.onnx".to_string()
}
fn default_embedder_model() -> String {
    "w600k_r50.onnx".to_string()
}
fn default_execution_provider() -> String {
    "cpu".to_string()
}
fn default_threads() -> u32 {
    4
}

impl Config {
    /// Load config from the default path (respects `FACELOCK_CONFIG` env var).
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
        // device.path is optional — when None, the daemon auto-detects a camera.
        // If explicitly set, reject empty strings.
        if let Some(ref path) = self.device.path {
            if path.is_empty() {
                return Err(ConfigError::Validation(
                    "device.path must not be empty when specified".into(),
                ));
            }
        }
        if !(0.0..=1.0).contains(&self.device.dark_threshold) {
            return Err(ConfigError::Validation(format!(
                "device.dark_threshold must be between 0.0 and 1.0, got {}",
                self.device.dark_threshold
            )));
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
        assert_eq!(config.device.path.as_deref(), Some("/dev/video0"));
        assert_eq!(config.device.max_height, 480);
        assert_eq!(config.recognition.threshold, 0.80);
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
model_dir = "/tmp/models"

[storage]
db_path = "/tmp/test.db"

[security]
disabled = false
require_ir = false
require_frame_variance = true
min_auth_frames = 5

[notification]
mode = "off"

[snapshots]
mode = "all"
dir = "/tmp/snaps"

"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.device.path.as_deref(), Some("/dev/video2"));
        assert_eq!(config.device.max_height, 720);
        assert_eq!(config.device.rotation, 90);
        assert_eq!(config.recognition.threshold, 0.5);
        assert_eq!(config.recognition.timeout_secs, 10);
        assert!(!config.security.require_ir);
        assert_eq!(config.security.min_auth_frames, 5);
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
        assert_eq!(config.snapshots.mode, SnapshotMode::Off);
    }

    #[test]
    fn recognition_gpu_config_defaults() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.recognition.execution_provider, "cpu");
        assert_eq!(config.recognition.threads, 4);
    }

    #[test]
    fn recognition_gpu_config_custom() {
        let toml = r#"
[device]
path = "/dev/video0"
[recognition]
execution_provider = "cuda"
threads = 8
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.recognition.execution_provider, "cuda");
        assert_eq!(config.recognition.threads, 8);
    }

    #[test]
    fn parse_no_device_section() {
        let toml = r#"
[recognition]
threshold = 0.5
"#;
        let config = Config::parse(toml).unwrap();
        assert!(config.device.path.is_none());
        assert_eq!(config.device.max_height, 480);
        assert_eq!(config.device.rotation, 0);
    }

    #[test]
    fn parse_device_section_without_path() {
        let toml = r#"
[device]
max_height = 720
"#;
        let config = Config::parse(toml).unwrap();
        assert!(config.device.path.is_none());
        assert_eq!(config.device.max_height, 720);
    }

    #[test]
    fn parse_device_with_explicit_path() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.device.path.as_deref(), Some("/dev/video0"));
    }

    #[test]
    fn idle_timeout_defaults_to_zero() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.daemon.idle_timeout_secs, 0);
    }

    #[test]
    fn idle_timeout_parses_custom_value() {
        let toml = r#"
[device]
path = "/dev/video0"
[daemon]
idle_timeout_secs = 300
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.daemon.idle_timeout_secs, 300);
    }

    #[test]
    fn tpm_config_defaults() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert!(!config.tpm.seal_database);
        assert!(!config.tpm.pcr_binding);
        assert_eq!(config.tpm.pcr_indices, vec![0, 1, 2, 3, 7]);
        assert_eq!(config.tpm.tcti, "device:/dev/tpmrm0");
    }

    #[test]
    fn warmup_frames_default() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.device.warmup_frames, 5);
    }

    #[test]
    fn warmup_frames_custom() {
        let toml = r#"
[device]
path = "/dev/video0"
warmup_frames = 10
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.device.warmup_frames, 10);
    }

    #[test]
    fn encryption_config_defaults() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.encryption.method, super::EncryptionMethod::None);
        assert_eq!(config.encryption.key_path, "/etc/facelock/encryption.key");
    }

    #[test]
    fn audit_config_defaults() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert!(!config.audit.enabled);
        assert_eq!(config.audit.path, "/var/log/facelock/audit.jsonl");
        assert_eq!(config.audit.rotate_size_mb, 10);
    }

    #[test]
    fn audit_config_custom() {
        let toml = r#"
[device]
path = "/dev/video0"
[audit]
enabled = true
path = "/var/log/custom/audit.jsonl"
rotate_size_mb = 50
"#;
        let config = Config::parse(toml).unwrap();
        assert!(config.audit.enabled);
        assert_eq!(config.audit.path, "/var/log/custom/audit.jsonl");
        assert_eq!(config.audit.rotate_size_mb, 50);
    }

    #[test]
    fn encryption_config_unknown_method_fails() {
        let toml = r#"
[device]
path = "/dev/video0"
[encryption]
method = "bogus"
"#;
        // Unknown encryption methods should be rejected by serde.
        assert!(Config::parse(toml).is_err());
    }

    #[test]
    fn encryption_config_tpm_method() {
        let toml = r#"
[device]
path = "/dev/video0"
[encryption]
method = "tpm"
sealed_key_path = "/etc/facelock/custom.sealed"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.encryption.method, super::EncryptionMethod::Tpm);
        assert_eq!(
            config.encryption.sealed_key_path,
            "/etc/facelock/custom.sealed"
        );
    }

    #[test]
    fn encryption_config_sealed_key_path_default() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(
            config.encryption.sealed_key_path,
            "/etc/facelock/encryption.key.sealed"
        );
    }

    #[test]
    fn encryption_config_keyfile() {
        let toml = r#"
[device]
path = "/dev/video0"
[encryption]
method = "keyfile"
key_path = "/etc/facelock/my.key"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.encryption.method, super::EncryptionMethod::Keyfile);
        assert_eq!(config.encryption.key_path, "/etc/facelock/my.key");
    }

    #[test]
    fn warmup_frames_zero() {
        let toml = r#"
[device]
path = "/dev/video0"
warmup_frames = 0
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.device.warmup_frames, 0);
    }
}
