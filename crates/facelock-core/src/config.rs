use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

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

/// `Default` is derived and composes each section's own `Default`, so
/// `Config::default()` and `Config::parse("")` are the same value by
/// construction rather than by coincidence — a property
/// `empty_document_parses_to_default` pins directly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub polkit: PolkitConfig,
    #[serde(default)]
    pub pam: PamConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Daemon only. Seconds to keep the camera streaming after a **failed**
    /// authentication, so the retry a failure invites skips the reopen cost.
    /// Success, cancellation and errors always release immediately — the
    /// interaction is over and the IR LED must go out with it (ADR 008).
    /// `0` means never hold; it used to be silently substituted with 5.
    /// Default: 3.
    #[serde(default = "default_camera_release_secs")]
    pub camera_release_secs: u32,
    /// Daemon only. Seconds to keep the camera streaming after a
    /// **successful** authentication as well. `0` — the default, and the
    /// right answer for almost every setup — releases at once: a success ends
    /// the interaction, so a hold after one keeps the camera, and on IR
    /// hardware its emitter LED, lit for a retry nobody is going to make.
    ///
    /// Raise it only where privileged actions repeat with no authentication
    /// caching in front of them — `sudo` with `timestamp_timeout=0`, a polkit
    /// action without `auth_admin_keep` — so that each action is a fresh
    /// authentication that would otherwise pay a camera reopen. Added on
    /// maintainer request as the opt-in ADR 008 §3 had deferred.
    ///
    /// Failed attempts are unaffected: they hold for
    /// [`DeviceConfig::camera_release_secs`]. Cancellations and errors
    /// release immediately whatever either key says.
    /// Default: 0.
    #[serde(default)]
    pub camera_release_after_success_secs: u32,
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
            camera_release_secs: default_camera_release_secs(),
            // No `default_*` function: this key's default is the type's, so
            // `#[serde(default)]` and this line cannot drift apart.
            camera_release_after_success_secs: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecognitionConfig {
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,
    /// End an attempt early once this many seconds have passed with **no face
    /// at all** detected. `timeout_secs` still bounds the other, slower case —
    /// a face was seen and has not matched yet — which is the one worth
    /// waiting out. Scanning an empty chair for the full timeout only keeps
    /// the camera (and on IR hardware its emitter LED) lit for nothing, and
    /// such an attempt is not a guess, so it also charges no rate-limit
    /// budget (ADR 008 §3/§4).
    ///
    /// `0` disables the early exit. The effective value is clamped to
    /// `timeout_secs` — see [`RecognitionConfig::effective_no_face_timeout`].
    /// Default: 2.
    #[serde(default = "default_no_face_timeout")]
    pub no_face_timeout_secs: u32,
    #[serde(default = "default_confidence")]
    pub detection_confidence: f32,
    #[serde(default = "default_nms")]
    pub nms_threshold: f32,
    #[serde(default = "default_detector_model")]
    pub detector_model: String,
    /// SHA256 for `detector_model` when the model is not covered by the bundled manifest.
    /// Bundled models are verified against their manifest hash at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector_sha256: Option<String>,
    #[serde(default = "default_embedder_model")]
    pub embedder_model: String,
    /// SHA256 for `embedder_model` when the model is not covered by the bundled manifest.
    /// Bundled models are verified against their manifest hash at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedder_sha256: Option<String>,
    /// ORT execution provider: "cpu", "cuda", "rocm", or "openvino".
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
            no_face_timeout_secs: default_no_face_timeout(),
            detection_confidence: default_confidence(),
            nms_threshold: default_nms(),
            detector_model: default_detector_model(),
            detector_sha256: None,
            embedder_model: default_embedder_model(),
            embedder_sha256: None,
            execution_provider: default_execution_provider(),
            threads: default_threads(),
        }
    }
}

impl RecognitionConfig {
    /// How long an attempt may run before "nobody is there" ends it, or
    /// `None` when the early exit is switched off (`no_face_timeout_secs = 0`).
    ///
    /// Clamped to `timeout_secs` rather than validated against it, on purpose
    /// (ADR 008 §3, "no migration"): an existing `/etc/facelock/config.toml`
    /// with a short `timeout_secs` — say 1 — predates this key and must keep
    /// loading, and a no-face deadline past the overall deadline could never
    /// fire anyway. So the pair can never be an invalid combination, only a
    /// redundant one.
    pub fn effective_no_face_timeout(&self) -> Option<Duration> {
        (self.no_face_timeout_secs > 0)
            .then(|| Duration::from_secs(self.no_face_timeout_secs.min(self.timeout_secs) as u64))
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub require_landmark_liveness: bool,
    /// Minimum pixel displacement to count a landmark as "moving" between frames.
    #[serde(default = "default_landmark_displacement_px")]
    pub landmark_displacement_px: f32,
    /// Number of landmarks (out of 5) that must show movement for liveness.
    #[serde(default = "default_landmark_min_moving")]
    pub landmark_min_moving: u32,
    #[serde(default = "default_min_auth_frames")]
    pub min_auth_frames: u32,
    /// Minimum per-face standard deviation (on the RAW grayscale frame) required
    /// to pass the IR texture check. Flat photos/screens score low in IR; real
    /// skin has micro-texture. Only applied on IR devices. Default 10.0
    /// (docs calibration: flat < 5, real > 15 on raw frames).
    #[serde(default = "default_ir_texture_min_stddev")]
    pub ir_texture_min_stddev: f32,
    /// Maximum consecutive matched-frame cosine similarity allowed by the passive
    /// frame-variance check, evaluated over a sliding window of the most recent
    /// `min_auth_frames` matches. Higher = more permissive. Default 0.985: truly
    /// static input sits ≳0.999, a frozen live human at 0.98–0.995; the default
    /// sits inside the frozen-human band for margin against static replays (a
    /// fully frozen user recovers via the sliding window as soon as they move).
    /// Passive anti-photo only; does not defeat video replay.
    #[serde(default = "default_frame_variance_max_similarity")]
    pub frame_variance_max_similarity: f32,
    /// Couple each enrolled template to the camera that captured it. When true
    /// (default), the auth path skips any template whose enrolling-camera
    /// fingerprint does not match the live camera at `device_match_granularity`,
    /// so a swapped-in camera degrades to password instead of matching.
    ///
    /// Advisory defense-in-depth only: the fingerprint is model-granularity
    /// (VID:PID) and forgeable by a programmable USB device — NOT attestation.
    #[serde(default = "default_true")]
    pub bind_templates_to_device: bool,
    /// How strictly the live camera must match a template's enrolling camera.
    /// `model` (default) compares VID:PID; `unit` also requires a matching
    /// serial (and enrollment refuses `unit` on cameras with no serial).
    #[serde(default)]
    pub device_match_granularity: crate::types::DeviceMatchGranularity,
    /// Allow legacy templates that predate device coupling (NULL `device_id`,
    /// or models enrolled on a camera with no readable USB identity) to
    /// authenticate. Default true (allow-with-warn) so upgrades don't break;
    /// set false to require every template to carry a matching device id.
    #[serde(default = "default_true")]
    pub bind_legacy_templates: bool,
    /// Permit storing face embeddings as plaintext (`encryption.method = "none"`).
    /// Default **false**: embeddings are encrypted at rest, and an explicit
    /// `method = "none"` is refused at enroll unless this is set true (an
    /// informed opt-out, warned prominently). Never weakens an already-encrypted
    /// store; it only governs whether new plaintext enrollment is allowed.
    #[serde(default)]
    pub allow_plaintext: bool,
    /// Opt-in **hard** device binding: fold the enrolling camera's `device_id`
    /// into the AES-GCM Additional Authenticated Data so a template sealed under
    /// one camera cannot even be decrypted under a different one. Default
    /// **false** — hard binding fails closed on unstable/absent device ids
    /// (unlike the advisory skip-on-mismatch coupling of Plan 02), so it is
    /// opt-in only. Requires an active encryption method to have any effect.
    #[serde(default)]
    pub bind_device_aad: bool,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

/// Why the device-binding policy refused an enrollment (#309).
///
/// Every variant is a refusal the operator can act on: the message names the
/// key that caused it and the way out. Enrollment is the only place these
/// fire; an existing template is never re-judged, so none of them is ever a
/// lockout. `identity` is the camera's canonical `vid:pid:serial` form with
/// missing fields rendered empty, as the operator will see it in `facelock
/// devices`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnrollmentBindingError {
    /// `device_match_granularity = "unit"` on a camera with no non-empty serial.
    #[error(
        "refusing to enroll: security.device_match_granularity = \"unit\" binds each template \
         to one camera unit by its USB serial, and this camera (identity \"{identity}\") \
         exposes none, so a template bound to it could never match. Set \
         security.device_match_granularity = \"model\" to bind by VID:PID, or enroll on a \
         camera that reports a stable serial."
    )]
    UnitNeedsSerial { identity: String },
    /// `unit` on a camera whose canonical id would be NULL: a serial alone is
    /// not an identity, and the row would bind to nothing.
    #[error(
        "refusing to enroll: security.device_match_granularity = \"unit\" binds each template \
         to one camera unit, and this camera (identity \"{identity}\") exposes no usable USB \
         identity, so the template would bind to nothing. Set \
         security.device_match_granularity = \"model\", or enroll on a camera that reports \
         idVendor, idProduct, and a stable serial."
    )]
    UnitNeedsIdentity { identity: String },
    /// Coupling on, legacy rows barred, and no usable identity: the NULL row
    /// it would store could never authenticate.
    #[error(
        "refusing to enroll: this camera (identity \"{identity}\") exposes no usable USB \
         identity, so its template would be stored without a device id, and \
         security.bind_legacy_templates = false bars such templates from authenticating. Set \
         security.bind_legacy_templates = true to accept templates with no device id, or \
         enroll on a camera that reports idVendor and idProduct."
    )]
    LegacyBarredNeedsIdentity { identity: String },
}

impl SecurityConfig {
    /// Resolve the device-binding policy consumed by the auth compare path.
    pub fn device_binding_policy(&self) -> crate::types::DeviceBindingPolicy {
        crate::types::DeviceBindingPolicy {
            enabled: self.bind_templates_to_device,
            granularity: self.device_match_granularity,
            allow_legacy: self.bind_legacy_templates,
        }
    }

    /// Whether enrollment may proceed under the device-binding policy for the
    /// camera about to record the template.
    ///
    /// Evaluated once per enrollment, after the camera is open and before the
    /// first model write, so a refusal leaves no durable state. It judges the
    /// id that would be persisted, not whether the auth path consults it
    /// today: `bind_templates_to_device` can be turned on later and the stored
    /// id must match then. Enrollment is the only place this fails closed; an
    /// existing template is never re-judged, so a policy change can refuse a
    /// new enrollment but never lock an authentication out. The error says
    /// which rule refused, and its message names the key and the way out.
    pub fn ensure_enrollment_binding_allowed(
        &self,
        fp: &crate::types::DeviceFingerprint,
    ) -> Result<(), EnrollmentBindingError> {
        let identity = || fp.canonical();
        if self.device_match_granularity == crate::types::DeviceMatchGranularity::Unit {
            if !fp.has_serial() {
                return Err(EnrollmentBindingError::UnitNeedsSerial {
                    identity: identity(),
                });
            }
            // A serial alone is not an identity: the row would be stored NULL
            // and, under the legacy policy, bind to nothing at all. The
            // strictest granularity must never produce that row.
            if fp.canonical_for_storage().is_none() {
                return Err(EnrollmentBindingError::UnitNeedsIdentity {
                    identity: identity(),
                });
            }
        }
        // With coupling on and legacy rows barred, a camera with no usable
        // identity would store a NULL row that can never authenticate.
        if self.bind_templates_to_device
            && !self.bind_legacy_templates
            && fp.canonical_for_storage().is_none()
        {
            return Err(EnrollmentBindingError::LegacyBarredNeedsIdentity {
                identity: identity(),
            });
        }
        self.require_device_aad(fp.canonical_for_storage().as_deref())
            .map(|_| ())
    }

    /// The AAD an enrollment must seal under, or why it must not proceed.
    ///
    /// The enrollment-path counterpart of [`Self::device_aad`]: with hard
    /// binding on, a missing device id is a refusal here rather than the
    /// unbound template `device_aad` would quietly produce (#312). `Ok(None)`
    /// when hard binding is off: ordinary encryption, no AAD.
    pub fn require_device_aad(&self, device_id: Option<&str>) -> Result<Option<Vec<u8>>, String> {
        if !self.bind_device_aad {
            return Ok(None);
        }
        crate::types::device_binding_aad(device_id)
            .map(Some)
            .ok_or_else(|| {
                "refusing to enroll: security.bind_device_aad = true seals each template \
                 under its enrolling camera's device id, and this camera exposes no usable \
                 USB identity, so the template could not be bound. Set \
                 security.bind_device_aad = false to enroll without hard binding, or enroll \
                 on a camera that reports both idVendor and idProduct."
                    .into()
            })
    }

    /// AAD bytes to decrypt a stored template under opt-in hard device
    /// binding (`bind_device_aad`), from the id its row records. `None` when
    /// disabled, and `None` for a row with no device id: such a row was sealed
    /// without AAD (a legacy unbound row, see [`Self::classify_device_binding`])
    /// and still decrypts, so enabling hard binding never locks anyone out.
    /// This is the authentication path; enrollment uses
    /// [`Self::require_device_aad`], which refuses instead.
    pub fn device_aad(&self, device_id: Option<&str>) -> Option<Vec<u8>> {
        if self.bind_device_aad {
            crate::types::device_binding_aad(device_id)
        } else {
            None
        }
    }

    /// How a stored template stands under the hard-binding policy, from the
    /// device id its row records.
    pub fn classify_device_binding(&self, device_id: Option<&str>) -> crate::types::DeviceBinding {
        if self.bind_device_aad && crate::types::device_binding_aad(device_id).is_none() {
            crate::types::DeviceBinding::LegacyUnbound
        } else {
            crate::types::DeviceBinding::Bound
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            require_landmark_liveness: false,
            landmark_displacement_px: default_landmark_displacement_px(),
            landmark_min_moving: default_landmark_min_moving(),
            min_auth_frames: default_min_auth_frames(),
            ir_texture_min_stddev: default_ir_texture_min_stddev(),
            frame_variance_max_similarity: default_frame_variance_max_similarity(),
            bind_templates_to_device: true,
            device_match_granularity: crate::types::DeviceMatchGranularity::Model,
            bind_legacy_templates: true,
            allow_plaintext: false,
            bind_device_aad: false,
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
///
/// **This `Default` is kept deliberately, and its PAM mirror is deliberately
/// deleted.** After [`NotificationConfig`] moved to a container-level
/// `#[serde(default)]`, nothing calls this impl — the section default names
/// the mode explicitly. It stays because its two siblings in this file,
/// [`SnapshotMode`] and [`EncryptionMethod`], still have live `Default`s via
/// their own field-level defaults, so one enum here silently lacking one
/// would read as an oversight rather than a decision.
///
/// The cost of keeping it is a second answer to "what is the default mode",
/// and this is the impl that drifted to `Both` in a shipped release — so
/// `notification_mode_default_agrees_with_the_section_default` pins the two
/// equal. `pam-facelock`'s `PamNotificationMode` deleted its `Default`
/// instead, which is stronger where it applies: with no impl, re-adding
/// `#[serde(default)]` to the field is a compile error rather than a test
/// failure. That option is not open here while the siblings need theirs.
///
/// The contract between the two crates is agreement on the default *value*
/// (`Terminal` on both sides), not on how each spells it. Do not delete this
/// impl for symmetry with PAM, and do not restore PAM's for symmetry with
/// this one.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NotificationMode {
    Off,
    #[default]
    Terminal,
    Desktop,
    Both,
}

/// The container-level `#[serde(default)]` is the fix for a shipped bug, not
/// a style choice. With per-field defaults, an omitted key and an absent
/// `[notification]` section are filled by two different mechanisms — serde's
/// field defaults and `Default for NotificationConfig` — which must agree by
/// hand. They did not: `notify_on_failure` once read `true` from serde and
/// `false` from `Default`, so the effective value flipped depending on
/// whether the section header was present. The shipped template has exactly
/// the triggering shape: an active `[notification]` header with every key
/// commented out.
///
/// Filling the whole struct from `Default` instead leaves one source of
/// truth, and "section present, key omitted" is then identical to "section
/// absent" by construction. Do not reintroduce `#[serde(default = "...")]` on
/// these fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub mode: NotificationMode,
    /// Show prompt text/notification when scanning starts ("Identifying face...")
    pub notify_prompt: bool,
    /// Show notification on successful face match
    pub notify_on_success: bool,
    /// Show notification on failed face match.
    /// Default: false — a failed match is already visible (you get a password
    /// prompt).
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
/// - `"keyfile"` — AES-256-GCM with a key file (**default**)
/// - `"tpm"` — AES-256-GCM with TPM-sealed key (key sealed by TPM, embeddings encrypted with AES)
/// - `"none"` — no encryption, embeddings stored as plaintext. Only honored when
///   `security.allow_plaintext = true`; otherwise enrollment refuses to store
///   plaintext biometric templates.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncryptionMethod {
    None,
    #[default]
    Keyfile,
    Tpm,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncryptionConfig {
    #[serde(default)]
    pub method: EncryptionMethod,
    /// Path to AES-256-GCM key file for `keyfile` method.
    /// Generated by `facelock setup` or `facelock tpm encrypt --generate-key`.
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
            method: EncryptionMethod::Keyfile,
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
    2
}
fn default_dark_threshold() -> f32 {
    0.6
}
fn default_camera_release_secs() -> u32 {
    3
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
fn default_no_face_timeout() -> u32 {
    2
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
fn default_landmark_displacement_px() -> f32 {
    1.5
}
fn default_landmark_min_moving() -> u32 {
    3
}
fn default_ir_texture_min_stddev() -> f32 {
    10.0
}
fn default_frame_variance_max_similarity() -> f32 {
    crate::types::DEFAULT_FRAME_VARIANCE_MAX_SIMILARITY
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Configuration for the polkit authentication agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolkitConfig {
    /// polkit `action_id`s for which face authentication may be offered.
    ///
    /// A single face match must NOT authorize every polkit action (pkexec,
    /// package install, disk mount, user admin). Any action not in this list
    /// is declined by the agent so polkit falls through to the password
    /// dialog handled by another agent — it is never denied outright.
    ///
    /// The default is a small vetted set of low/moderate-sensitivity actions.
    /// Users may extend it deliberately (like a fingerprint reader's reach),
    /// but high-risk actions are excluded by default.
    #[serde(default = "default_face_eligible_actions")]
    pub face_eligible_actions: Vec<String>,
}

impl Default for PolkitConfig {
    fn default() -> Self {
        Self {
            face_eligible_actions: default_face_eligible_actions(),
        }
    }
}

impl PolkitConfig {
    /// Whether the given polkit `action_id` may be authorized by face auth.
    pub fn is_face_eligible(&self, action_id: &str) -> bool {
        self.face_eligible_actions.iter().any(|a| a == action_id)
    }
}

/// Where `facelock pam` looks for PAM service files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PamConfig {
    /// The PAM configuration directories, in search order — Linux-PAM's own
    /// precedence, earliest wins.
    ///
    /// **The first entry is the override directory: every write lands there,
    /// and every later entry is read-only.** A service that resolves only in a
    /// later directory is copied into the first one before the facelock line
    /// is inserted, because the later ones are package-owned — an edit there
    /// is clobbered on the next upgrade and makes the package manager report a
    /// modified file. Putting a package-owned directory first would therefore
    /// make facelock edit package files; do not.
    ///
    /// The default covers what Linux-PAM itself reads on a distribution that
    /// enables the vendor directory (Arch, Fedora): `/etc/pam.d` first, then
    /// `/usr/lib/pam.d`. There is no way to ask Linux-PAM at run time which
    /// vendor directory it was compiled with, which is what this key is for —
    /// auto-detection is the default and configuration is never required.
    #[serde(default = "default_pam_config_dirs")]
    pub config_dirs: Vec<String>,
}

impl Default for PamConfig {
    fn default() -> Self {
        Self {
            config_dirs: default_pam_config_dirs(),
        }
    }
}

/// `/etc/pam.d` then `/usr/lib/pam.d`. The single copy of the list: the CLI's
/// PAM writer takes its defaults from here rather than keeping its own.
fn default_pam_config_dirs() -> Vec<String> {
    vec!["/etc/pam.d".to_string(), "/usr/lib/pam.d".to_string()]
}

/// Default polkit actions eligible for face authentication.
///
/// Deliberately small and low-risk. High-risk actions — pkexec
/// (`org.freedesktop.policykit.exec`), PackageKit install/remove, udisks
/// mount, and accounts-service user admin — are intentionally EXCLUDED so a
/// single face match cannot become a universal root key.
fn default_face_eligible_actions() -> Vec<String> {
    vec!["org.freedesktop.login1.lock-sessions".to_string()]
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

    /// Whether enrollment may proceed under the current encryption policy.
    ///
    /// Refuses to store a plaintext biometric template (`encryption.method =
    /// "none"`) unless the operator has explicitly opted in via
    /// `security.allow_plaintext`. This is a config-time guard surfaced as an
    /// enroll error — it never affects the auth fall-through to password and is
    /// never a lockout. Returns the reason string on refusal.
    pub fn ensure_enroll_encryption_allowed(&self) -> Result<(), String> {
        if self.encryption.method == EncryptionMethod::None && !self.security.allow_plaintext {
            return Err(
                "refusing to enroll: encryption.method = \"none\" would store your face \
                 template as plaintext biometric data. Enable encryption (the default \
                 \"keyfile\"/\"tpm\"), or, to intentionally store plaintext, set \
                 security.allow_plaintext = true."
                    .into(),
            );
        }
        Ok(())
    }

    /// Server-side enrollment deadline in seconds: 3x the auth timeout
    /// (floored at 5s) because enrollment needs multiple good captures.
    ///
    /// Single source of truth for both the daemon's enrollment loop and the
    /// CLI's D-Bus Enroll method timeout — the client timeout must exceed
    /// this deadline (plus margin) or it aborts while the daemon is still
    /// enrolling (see docs/contracts.md §IPC Protocol).
    pub fn enroll_timeout_secs(&self) -> u64 {
        (self.recognition.timeout_secs as u64).max(5) * 3
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
        if let Some(ref path) = self.device.path
            && path.is_empty()
        {
            return Err(ConfigError::Validation(
                "device.path must not be empty when specified".into(),
            ));
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
        if !(0.0..=1.0).contains(&self.security.frame_variance_max_similarity) {
            return Err(ConfigError::Validation(format!(
                "security.frame_variance_max_similarity must be between 0.0 and 1.0, got {}",
                self.security.frame_variance_max_similarity
            )));
        }
        if self.security.ir_texture_min_stddev < 0.0 {
            return Err(ConfigError::Validation(format!(
                "security.ir_texture_min_stddev must be >= 0.0, got {}",
                self.security.ir_texture_min_stddev
            )));
        }
        if let Some(ref sha256) = self.recognition.detector_sha256
            && !is_sha256_hex(sha256)
        {
            return Err(ConfigError::Validation(format!(
                "recognition.detector_sha256 must be a 64-character hex SHA256, got {}",
                sha256
            )));
        }
        if let Some(ref sha256) = self.recognition.embedder_sha256
            && !is_sha256_hex(sha256)
        {
            return Err(ConfigError::Validation(format!(
                "recognition.embedder_sha256 must be a 64-character hex SHA256, got {}",
                sha256
            )));
        }
        Ok(())
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
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

    /// D9 drift pin: a `[notification]` section present with every key
    /// omitted must parse identically to no section at all. This was two
    /// different code paths (serde field defaults vs `Default for
    /// NotificationConfig`) and `notify_on_failure` had already drifted
    /// between them (serde said `true`, `Default` and the shipped template
    /// said `false`). The container-level `#[serde(default)]` now makes them
    /// the same path; this stays as the regression guard for that.
    #[test]
    fn notification_section_present_with_keys_omitted_equals_default() {
        let present = Config::parse("[notification]\n").unwrap().notification;
        let absent = Config::parse("").unwrap().notification;
        let default = NotificationConfig::default();
        for parsed in [&present, &absent] {
            assert_eq!(parsed.mode, default.mode);
            assert_eq!(parsed.notify_prompt, default.notify_prompt);
            assert_eq!(parsed.notify_on_success, default.notify_on_success);
            assert_eq!(parsed.notify_on_failure, default.notify_on_failure);
        }
    }

    /// `NotificationMode::default()` is unused but deliberately kept — see
    /// its own docs for why, and for why PAM's mirror deleted theirs. Keeping
    /// it leaves two answers to "what is the default mode", and this impl is
    /// the one that drifted to `Both` in a shipped release, so pin them
    /// equal. Also covers what a deletion could not: if `#[serde(default)]`
    /// is ever re-added to the `mode` field, the enum's answer becomes
    /// authoritative again and this catches it disagreeing.
    #[test]
    fn notification_mode_default_agrees_with_the_section_default() {
        assert_eq!(
            NotificationMode::default(),
            NotificationConfig::default().mode
        );
    }

    /// The same drift class, generalized past one section: every `Config`
    /// field is `#[serde(default)]`, so an empty document must produce
    /// exactly `Config::default()`. Any section that grows a field default
    /// disagreeing with its `Default` impl fails here, not in production.
    #[test]
    fn empty_document_parses_to_default() {
        assert_eq!(
            Config::parse("").expect("an empty config document must always parse"),
            Config::default()
        );
    }

    #[test]
    fn enroll_timeout_is_three_times_auth_timeout_with_floor() {
        // Default timeout_secs = 5 -> 15s enrollment deadline.
        let config = Config::parse("").unwrap();
        assert_eq!(config.enroll_timeout_secs(), 15);

        let config = Config::parse("[recognition]\ntimeout_secs = 10\n").unwrap();
        assert_eq!(config.enroll_timeout_secs(), 30);

        // Values below the 5s floor still yield the 15s minimum deadline.
        let config = Config::parse("[recognition]\ntimeout_secs = 2\n").unwrap();
        assert_eq!(config.enroll_timeout_secs(), 15);
    }

    /// ADR 008 §3. Table-driven over the three cases the key has: the
    /// default, a value that outruns `timeout_secs`, and the off switch.
    #[test]
    fn no_face_timeout_defaults_to_two_and_clamps_to_the_overall_timeout() {
        // (toml, expected effective no-face timeout in seconds)
        let cases: &[(&str, Option<u64>)] = &[
            // Default: 2s of an empty chair, well inside the 5s default timeout.
            ("", Some(2)),
            // Clamped, not rejected: a config written before this key existed
            // may already carry a timeout shorter than the new default.
            ("[recognition]\ntimeout_secs = 1\n", Some(1)),
            (
                "[recognition]\ntimeout_secs = 3\nno_face_timeout_secs = 10\n",
                Some(3),
            ),
            // Under the timeout, the value is used as written.
            (
                "[recognition]\ntimeout_secs = 30\nno_face_timeout_secs = 4\n",
                Some(4),
            ),
            // 0 disables the early exit entirely.
            ("[recognition]\nno_face_timeout_secs = 0\n", None),
        ];

        for (toml, expected) in cases {
            let config = Config::parse(toml)
                .unwrap_or_else(|e| panic!("{toml:?} must load — this key never rejects: {e}"));
            assert_eq!(
                config.recognition.effective_no_face_timeout(),
                expected.map(Duration::from_secs),
                "wrong effective no-face timeout for {toml:?}"
            );
        }

        assert_eq!(
            Config::default().recognition.no_face_timeout_secs,
            2,
            "the documented default"
        );
    }

    /// ADR 008 §3, added on maintainer request. The key is purely opt-in:
    /// absent it is `0`, which is the behavior every install already has —
    /// a success releases the camera with the reply. Table-driven over the
    /// ways a config can decline to mention it, plus the one that asks.
    #[test]
    fn the_success_hold_is_off_unless_a_config_asks_for_it() {
        // (toml, expected success hold, expected failure hold) — the second
        // column is here because the two are separate budgets: writing one
        // must never move the other.
        let cases: &[(&str, u32, u32)] = &[
            ("", 0, 3),
            ("[device]\n", 0, 3),
            ("[device]\ncamera_release_secs = 10\n", 0, 10),
            ("[device]\ncamera_release_after_success_secs = 5\n", 5, 3),
            // `0` written out means the same as omitting it.
            ("[device]\ncamera_release_after_success_secs = 0\n", 0, 3),
        ];
        for (toml, success_secs, failure_secs) in cases {
            let config = Config::parse(toml)
                .unwrap_or_else(|e| panic!("{toml:?} must load — this key never rejects: {e}"));
            assert_eq!(
                config.device.camera_release_after_success_secs, *success_secs,
                "wrong success hold for {toml:?}"
            );
            assert_eq!(
                config.device.camera_release_secs, *failure_secs,
                "wrong failure hold for {toml:?}"
            );
        }

        assert_eq!(
            Config::default().device.camera_release_after_success_secs,
            0,
            "the documented default: a success ends the interaction"
        );
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
    fn recognition_sha256_fields_default_to_none() {
        let config = RecognitionConfig::default();
        assert!(config.detector_sha256.is_none());
        assert!(config.embedder_sha256.is_none());
    }

    #[test]
    fn recognition_sha256_fields_validate_format() {
        let toml = r#"
[device]
path = "/dev/video0"
[recognition]
detector_sha256 = "not-a-sha256"
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn recognition_sha256_fields_accept_valid_hex() {
        let toml = r#"
[device]
path = "/dev/video0"
[recognition]
detector_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
embedder_sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(
            config.recognition.detector_sha256.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(
            config.recognition.embedder_sha256.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn recognition_sha256_fields_accept_uppercase_hex() {
        let toml = r#"
[device]
path = "/dev/video0"
[recognition]
detector_sha256 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(
            config.recognition.detector_sha256.as_deref(),
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        );
    }

    #[test]
    fn recognition_sha256_validation_message_matches_allowed_format() {
        let toml = r#"
[device]
path = "/dev/video0"
[recognition]
detector_sha256 = "not-a-sha256"
"#;
        let err = Config::parse(toml).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("64-character hex SHA256"));
        assert!(!msg.contains("lowercase"));
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
        assert_eq!(config.device.warmup_frames, 2);
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
        // Encrypt-by-default (finding #8): a config with no [encryption] section
        // now defaults to keyfile encryption, not plaintext.
        assert_eq!(config.encryption.method, super::EncryptionMethod::Keyfile);
        assert_eq!(config.encryption.key_path, "/etc/facelock/encryption.key");
    }

    #[test]
    fn encryption_method_default_is_keyfile() {
        assert_eq!(
            super::EncryptionMethod::default(),
            super::EncryptionMethod::Keyfile
        );
    }

    #[test]
    fn security_plaintext_and_aad_default_off() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert!(!config.security.allow_plaintext);
        assert!(!config.security.bind_device_aad);
    }

    #[test]
    fn enroll_refused_when_plaintext_not_allowed() {
        let toml = r#"
[device]
path = "/dev/video0"
[encryption]
method = "none"
"#;
        let config = Config::parse(toml).unwrap();
        // method=none without the explicit opt-in must be refused at enroll.
        assert!(config.ensure_enroll_encryption_allowed().is_err());
    }

    #[test]
    fn enroll_allowed_plaintext_with_optin() {
        let toml = r#"
[device]
path = "/dev/video0"
[encryption]
method = "none"
[security]
allow_plaintext = true
"#;
        let config = Config::parse(toml).unwrap();
        assert!(config.ensure_enroll_encryption_allowed().is_ok());
    }

    #[test]
    fn enroll_allowed_when_encrypted() {
        // Default (keyfile) enrollment is always permitted.
        let config = Config::parse("[device]\npath = \"/dev/video0\"\n").unwrap();
        assert!(config.ensure_enroll_encryption_allowed().is_ok());
    }

    /// The authentication-side derivation: off by default, and under opt-in
    /// a row with no device id decrypts without AAD. That `None` is what
    /// keeps a legacy unbound row authenticating after hard binding is
    /// enabled; it is not a licence to enroll one (see `require_device_aad`).
    #[test]
    fn device_aad_follows_opt_in_and_leaves_legacy_rows_unbound() {
        let mut config = Config::parse("[device]\npath = \"/dev/video0\"\n").unwrap();
        // Off by default → no AAD even with a device id.
        assert_eq!(config.security.device_aad(Some("046d:085e:X")), None);
        // Opt-in → AAD derived from the device id.
        config.security.bind_device_aad = true;
        assert_eq!(
            config.security.device_aad(Some("046d:085e:X")),
            crate::types::device_binding_aad(Some("046d:085e:X"))
        );
        // Opt-in, legacy row with no device id → decrypts unbound.
        assert_eq!(config.security.device_aad(None), None);
        assert_eq!(
            config.security.classify_device_binding(None),
            crate::types::DeviceBinding::LegacyUnbound
        );
    }

    fn hard_binding() -> SecurityConfig {
        SecurityConfig {
            bind_device_aad: true,
            ..SecurityConfig::default()
        }
    }

    /// #312: the enrollment-side derivation fails closed. With hard binding
    /// on, a missing or empty device id is a refusal naming the key, never an
    /// unbound template.
    #[test]
    fn require_device_aad_refuses_without_a_device_id() {
        for device_id in [None, Some("")] {
            let err = hard_binding().require_device_aad(device_id).unwrap_err();
            assert!(
                err.contains("security.bind_device_aad"),
                "must name the key: {err}"
            );
            assert!(
                err.contains("bind_device_aad = false"),
                "must name the remedy: {err}"
            );
        }
    }

    #[test]
    fn require_device_aad_binds_a_stable_id() {
        assert_eq!(
            hard_binding().require_device_aad(Some("046d:085e:X")),
            Ok(crate::types::device_binding_aad(Some("046d:085e:X")))
        );
    }

    /// Ordinary encryption: hard binding off means no AAD, id or not.
    #[test]
    fn require_device_aad_is_absent_when_hard_binding_is_off() {
        let ordinary = SecurityConfig::default();
        assert_eq!(ordinary.require_device_aad(None), Ok(None));
        assert_eq!(ordinary.require_device_aad(Some("046d:085e:X")), Ok(None));
    }

    /// The enrollment precondition carries the same rule, keyed on the live
    /// fingerprint: no usable identity under hard binding is refused before
    /// any model write; an identified camera passes.
    #[test]
    fn hard_binding_refuses_enrollment_of_an_unidentifiable_camera() {
        let policy = hard_binding();
        for fp in [
            crate::types::DeviceFingerprint::default(),
            crate::types::DeviceFingerprint {
                vid: Some("046d".into()),
                ..Default::default()
            },
        ] {
            let err = policy.ensure_enrollment_binding_allowed(&fp).unwrap_err();
            assert!(err.contains("security.bind_device_aad"), "{err}");
        }
        assert_eq!(
            policy.ensure_enrollment_binding_allowed(&camera_with_serial(None)),
            Ok(())
        );
    }

    #[test]
    fn classify_device_binding_reports_only_id_less_rows_under_hard_binding() {
        use crate::types::DeviceBinding::{Bound, LegacyUnbound};
        let policy = hard_binding();
        assert_eq!(policy.classify_device_binding(None), LegacyUnbound);
        assert_eq!(policy.classify_device_binding(Some("")), LegacyUnbound);
        assert_eq!(policy.classify_device_binding(Some("046d:085e:")), Bound);
        // With hard binding off nothing is asked of any row.
        let ordinary = SecurityConfig::default();
        assert_eq!(ordinary.classify_device_binding(None), Bound);
    }

    /// A same-model camera fingerprint with the given serial field.
    fn camera_with_serial(serial: Option<&str>) -> crate::types::DeviceFingerprint {
        crate::types::DeviceFingerprint {
            vid: Some("046d".into()),
            pid: Some("085e".into()),
            serial: serial.map(String::from),
            by_path: None,
        }
    }

    fn binding_at(granularity: crate::types::DeviceMatchGranularity) -> SecurityConfig {
        SecurityConfig {
            device_match_granularity: granularity,
            ..SecurityConfig::default()
        }
    }

    /// #309: a `unit`-bound template can only ever match by serial, so a
    /// camera without one (missing or empty) is refused before any model
    /// exists. The error names the key that caused it and the way out.
    #[test]
    fn unit_binding_refuses_enrollment_without_a_serial() {
        let unit = binding_at(crate::types::DeviceMatchGranularity::Unit);
        for serial in [None, Some("")] {
            let err = unit
                .ensure_enrollment_binding_allowed(&camera_with_serial(serial))
                .unwrap_err();
            assert_eq!(
                err,
                EnrollmentBindingError::UnitNeedsSerial {
                    identity: "046d:085e:".into()
                }
            );
        }
        let text = EnrollmentBindingError::UnitNeedsSerial {
            identity: "046d:085e:".into(),
        }
        .to_string();
        assert!(
            text.contains("security.device_match_granularity = \"model\""),
            "must name the key and the remedy: {text}"
        );
    }

    #[test]
    fn unit_binding_allows_enrollment_with_a_serial() {
        let unit = binding_at(crate::types::DeviceMatchGranularity::Unit);
        assert_eq!(
            unit.ensure_enrollment_binding_allowed(&camera_with_serial(Some("SER"))),
            Ok(())
        );
    }

    #[test]
    fn model_binding_allows_enrollment_whatever_the_serial() {
        let model = binding_at(crate::types::DeviceMatchGranularity::Model);
        for serial in [None, Some(""), Some("SER")] {
            assert_eq!(
                model.ensure_enrollment_binding_allowed(&camera_with_serial(serial)),
                Ok(()),
                "serial {serial:?}"
            );
        }
    }

    /// The documented `model` default for a camera with no readable identity:
    /// enrollment proceeds and stores NULL, governed by the legacy policy.
    /// Pinned so the `Model` arm cannot silently start refusing.
    #[test]
    fn model_binding_allows_an_unidentifiable_camera_as_legacy() {
        let model = binding_at(crate::types::DeviceMatchGranularity::Model);
        let unknown = crate::types::DeviceFingerprint::default();
        assert!(!model.bind_device_aad, "hard binding is off by default");
        assert_eq!(model.ensure_enrollment_binding_allowed(&unknown), Ok(()));
        assert_eq!(unknown.canonical_for_storage(), None);
    }

    /// #309 at `model`: with coupling on and legacy rows barred, a camera
    /// with no usable identity would store a NULL row that can never
    /// authenticate. Refused, naming the key that bars it. With coupling off
    /// the legacy policy is never consulted, so the same camera enrolls.
    #[test]
    fn model_binding_refuses_an_unidentifiable_camera_when_legacy_rows_cannot_authenticate() {
        let mut strict = binding_at(crate::types::DeviceMatchGranularity::Model);
        strict.bind_legacy_templates = false;
        let unknown = crate::types::DeviceFingerprint::default();
        let err = strict
            .ensure_enrollment_binding_allowed(&unknown)
            .unwrap_err();
        assert_eq!(
            err,
            EnrollmentBindingError::LegacyBarredNeedsIdentity {
                identity: "::".into()
            }
        );
        assert!(
            err.to_string()
                .contains("security.bind_legacy_templates = true"),
            "must name the key and the remedy: {err}"
        );
        // An identified camera is unaffected.
        assert_eq!(
            strict.ensure_enrollment_binding_allowed(&camera_with_serial(None)),
            Ok(())
        );
        // Coupling off: the legacy policy never applies, nothing to refuse.
        strict.bind_templates_to_device = false;
        assert_eq!(strict.ensure_enrollment_binding_allowed(&unknown), Ok(()));
    }

    /// A camera with no readable identity at all has no serial either. It
    /// would be stored NULL and authenticate on any camera under the legacy
    /// policy, the opposite of what `unit` asks for.
    #[test]
    fn unit_binding_refuses_an_unidentifiable_camera() {
        let unit = binding_at(crate::types::DeviceMatchGranularity::Unit);
        assert_eq!(
            unit.ensure_enrollment_binding_allowed(&crate::types::DeviceFingerprint::default()),
            Err(EnrollmentBindingError::UnitNeedsSerial {
                identity: "::".into()
            })
        );
    }

    /// A serial on a camera missing its product id is the shape that slipped
    /// past the serial check alone: it would be stored NULL and bind to
    /// nothing under the strictest policy. Refused, naming the key and the
    /// `"model"` way out.
    #[test]
    fn unit_binding_refuses_a_serial_without_a_full_identity() {
        let unit = binding_at(crate::types::DeviceMatchGranularity::Unit);
        let half = crate::types::DeviceFingerprint {
            vid: Some("046d".into()),
            pid: None,
            serial: Some("SER".into()),
            by_path: None,
        };
        assert_eq!(half.canonical_for_storage(), None, "the shape under test");
        let err = unit.ensure_enrollment_binding_allowed(&half).unwrap_err();
        assert_eq!(
            err,
            EnrollmentBindingError::UnitNeedsIdentity {
                identity: "046d::SER".into()
            }
        );
        assert!(
            err.to_string()
                .contains("security.device_match_granularity = \"model\""),
            "must name the key and the remedy: {err}"
        );
    }

    /// The precondition is about the id that gets persisted, not about
    /// whether the auth path consults it today: `bind_templates_to_device`
    /// can be turned on later, and a template stored under `unit` must
    /// match then too.
    #[test]
    fn unit_binding_precondition_holds_while_coupling_is_off() {
        let mut unit = binding_at(crate::types::DeviceMatchGranularity::Unit);
        unit.bind_templates_to_device = false;
        assert!(matches!(
            unit.ensure_enrollment_binding_allowed(&camera_with_serial(None)),
            Err(EnrollmentBindingError::UnitNeedsSerial { .. })
        ));
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
    fn antispoof_thresholds_default() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.security.ir_texture_min_stddev, 10.0);
        assert_eq!(
            config.security.frame_variance_max_similarity,
            crate::types::DEFAULT_FRAME_VARIANCE_MAX_SIMILARITY
        );
    }

    #[test]
    fn antispoof_thresholds_custom() {
        let toml = r#"
[device]
path = "/dev/video0"
[security]
ir_texture_min_stddev = 15.0
frame_variance_max_similarity = 0.95
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(config.security.ir_texture_min_stddev, 15.0);
        assert_eq!(config.security.frame_variance_max_similarity, 0.95);
    }

    #[test]
    fn reject_out_of_range_frame_variance_max_similarity() {
        let toml = r#"
[device]
path = "/dev/video0"
[security]
frame_variance_max_similarity = 1.5
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn reject_negative_ir_texture_min_stddev() {
        let toml = r#"
[device]
path = "/dev/video0"
[security]
ir_texture_min_stddev = -1.0
"#;
        let err = Config::parse(toml).unwrap_err();
        assert!(matches!(err, ConfigError::Validation(_)));
    }

    #[test]
    fn device_binding_defaults_on_at_model_granularity() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert!(config.security.bind_templates_to_device);
        assert_eq!(
            config.security.device_match_granularity,
            crate::types::DeviceMatchGranularity::Model
        );
        assert!(config.security.bind_legacy_templates);

        let policy = config.security.device_binding_policy();
        assert!(policy.enabled);
        assert!(policy.allow_legacy);
        assert_eq!(
            policy.granularity,
            crate::types::DeviceMatchGranularity::Model
        );
    }

    #[test]
    fn device_binding_custom_values_parse() {
        let toml = r#"
[device]
path = "/dev/video0"
[security]
bind_templates_to_device = false
device_match_granularity = "unit"
bind_legacy_templates = false
"#;
        let config = Config::parse(toml).unwrap();
        assert!(!config.security.bind_templates_to_device);
        assert_eq!(
            config.security.device_match_granularity,
            crate::types::DeviceMatchGranularity::Unit
        );
        assert!(!config.security.bind_legacy_templates);
    }

    #[test]
    fn device_match_granularity_rejects_unknown() {
        let toml = r#"
[device]
path = "/dev/video0"
[security]
device_match_granularity = "bogus"
"#;
        assert!(Config::parse(toml).is_err());
    }

    #[test]
    fn polkit_config_default_allowlist_is_low_risk() {
        let config = PolkitConfig::default();
        // Default must offer face only for the vetted low-risk action.
        assert_eq!(
            config.face_eligible_actions,
            vec!["org.freedesktop.login1.lock-sessions".to_string()]
        );
        assert!(config.is_face_eligible("org.freedesktop.login1.lock-sessions"));
    }

    #[test]
    fn polkit_config_default_excludes_high_risk_actions() {
        let config = PolkitConfig::default();
        // High-risk actions must NOT be face-eligible by default.
        for action in [
            "org.freedesktop.policykit.exec",
            "org.freedesktop.packagekit.package-install",
            "org.freedesktop.udisks2.filesystem-mount",
            "org.freedesktop.accounts.user-administration",
        ] {
            assert!(
                !config.is_face_eligible(action),
                "{action} must not be face-eligible by default"
            );
        }
    }

    #[test]
    fn polkit_config_defaults_when_section_absent() {
        let toml = r#"
[device]
path = "/dev/video0"
"#;
        let config = Config::parse(toml).unwrap();
        assert_eq!(
            config.polkit.face_eligible_actions,
            vec!["org.freedesktop.login1.lock-sessions".to_string()]
        );
    }

    #[test]
    fn polkit_config_allowlist_is_configurable() {
        let toml = r#"
[device]
path = "/dev/video0"
[polkit]
face_eligible_actions = [
    "org.freedesktop.login1.lock-sessions",
    "org.freedesktop.udisks2.filesystem-mount",
]
"#;
        let config = Config::parse(toml).unwrap();
        assert!(
            config
                .polkit
                .is_face_eligible("org.freedesktop.udisks2.filesystem-mount")
        );
        assert!(
            config
                .polkit
                .is_face_eligible("org.freedesktop.login1.lock-sessions")
        );
        // Still excludes anything the user did not add.
        assert!(
            !config
                .polkit
                .is_face_eligible("org.freedesktop.policykit.exec")
        );
    }

    #[test]
    fn polkit_config_empty_allowlist_declines_everything() {
        let toml = r#"
[device]
path = "/dev/video0"
[polkit]
face_eligible_actions = []
"#;
        let config = Config::parse(toml).unwrap();
        // An explicitly empty list means "never offer face" — no fail-open.
        assert!(
            !config
                .polkit
                .is_face_eligible("org.freedesktop.login1.lock-sessions")
        );
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
