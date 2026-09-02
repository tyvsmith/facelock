//! The Health domain (map §5): what is knowable about the system, gathered
//! into one value.
//!
//! [`Health`] is everything `facelock status` reports, as plain data: each
//! section is a [`Fact`] populated by an isolated probe, and the report
//! itself is a pure renderer over the struct (`commands::status`). The split
//! is what makes the report testable — a `Health` can be constructed in a
//! test with zero root, camera, or bus — and what enforces N4's surviving
//! rule: *"the daemon is unreachable" and "the database could not be read"
//! are distinct values from "this user has no models"*, so the renderer
//! cannot collapse unknown into false.
//!
//! Privilege model (DEC-6/DEC-7): binary. `is-enrolled` is Tier 0 — one fact,
//! its own 0600 marker, no bus, no store, no camera — and **never comes
//! here**; its isolation is the contract (see `commands/is_enrolled.rs`).
//! `status` is Tier 2 — root, everything — which is why these probes may read
//! the 0600 database and other users' markers directly. The two commands
//! share the enrollment *fact* (the store count vs the marker), not code: the
//! marker comparison below is `status`'s diagnostic for the marker
//! `is-enrolled` answers from.

use std::path::{Path, PathBuf};

use facelock_core::Config;
use facelock_core::config::{EncryptionMethod, NotificationMode};
use facelock_store::{FaceStore, StoreError};

use crate::backend::{self, DaemonPing};
use crate::commands::enrollment_marker::{self, MarkerState};
use crate::resolved::{CameraPresence, ConfigLoad, ExecutionProviderFact, Fact, ModelFiles};

/// The oneshot auth binary the PAM module spawns when the daemon path fails.
/// Mirrors `AUTH_BIN` in `pam-facelock` (hardcoded there by N9; a config key
/// naming the binary PAM executes as root would be an escalation knob).
pub const AUTH_BIN: &str = "/usr/bin/facelock";

/// Where the PAM module may be installed; the first entry is the primary
/// path named in the report's item line.
///
/// Re-exported rather than declared: `commands::pam` owns the probe order,
/// because it is what refuses to write when the module is missing, and two
/// lists that drift apart is exactly the bug `status` would then fail to
/// report (#170).
pub use crate::commands::pam::PAM_MODULE_PATHS;
pub use crate::commands::pam::{ConfiguredService, NotChecked};

use crate::commands::pam::{PamDirs, configured_scan};

/// The `why` shared by every fact that cannot be probed without a parsed
/// config.
///
/// Sourced from the message catalog rather than written here: it is composed
/// into the localized "cannot determine: {why}" line, so an English literal
/// would leave half that sentence untranslatable.
fn config_not_available() -> String {
    crate::message::Message::localized(&crate::message::StatusMessage::StatusWhyConfigNotAvailable)
}

/// Everything `status` reports, as data. Constructed by [`Health::probe`] on
/// the real system (root), or literally in tests.
#[derive(Debug, Clone)]
pub struct Health {
    /// The config parse outcome is always known — running the parse *is* the
    /// probe — so this is not a [`Fact`].
    pub config: ConfigHealth,
    pub daemon: Fact<DaemonPing>,
    pub oneshot_fallback: Fact<OneshotFallback>,
    pub camera: Fact<CameraHealth>,
    pub models: Fact<ModelFiles>,
    pub execution_provider: Fact<ExecutionProviderFact>,
    pub encryption: Fact<EncryptionHealth>,
    /// The user whose enrollment the report describes (`--user` semantics of
    /// `resolve_user`: SUDO_USER/DOAS_USER aware, so `sudo facelock status`
    /// reports on the invoking human, not root).
    pub user: String,
    pub enrolled: Fact<EnrollmentHealth>,
    pub security: Fact<SecurityHealth>,
    pub notifications: Fact<NotificationHealth>,
    /// Config-independent: probed even when the config did not parse.
    pub pam: PamWiring,
}

impl Health {
    /// Probe the live system. Root path: reads the database, other users'
    /// markers, the ONNX runtime, and pings the daemon (activating — see
    /// [`backend::probe_daemon`]).
    pub fn probe(loaded: &ConfigLoad) -> Health {
        let user = crate::ipc_client::resolve_user(None);
        let config_health = ConfigHealth::from_load(loaded);

        let Some(config) = loaded.config() else {
            return Health {
                config: config_health,
                daemon: Fact::unknown(config_not_available()),
                oneshot_fallback: Fact::unknown(config_not_available()),
                camera: Fact::unknown(config_not_available()),
                models: Fact::unknown(config_not_available()),
                execution_provider: Fact::unknown(config_not_available()),
                encryption: Fact::unknown(config_not_available()),
                user,
                enrolled: Fact::unknown(config_not_available()),
                security: Fact::unknown(config_not_available()),
                notifications: Fact::unknown(config_not_available()),
                pam: probe_pam(loaded),
            };
        };

        // The model files are probed as a *value*, and the fallback's
        // `models_present` is derived from it here, before it is lifted into
        // a `Fact`. Deriving it from the lifted fact instead would fold an
        // undetermined answer into `false` (N4) — see
        // [`probe_oneshot_fallback_at`].
        let models = ModelFiles::probe(config);
        let fallback = probe_oneshot_fallback_at(
            Path::new(AUTH_BIN),
            Path::new(&config.storage.db_path),
            &models,
        );
        Health {
            config: config_health,
            daemon: backend::probe_daemon(&config.daemon.mode),
            oneshot_fallback: Fact::probed(fallback),
            camera: probe_camera(config),
            execution_provider: Fact::probed(ExecutionProviderFact::probe(config)),
            encryption: Fact::probed(probe_encryption(config)),
            enrolled: probe_enrolled(config, &user),
            security: Fact::claimed(SecurityHealth::from_config(config)),
            notifications: Fact::claimed(NotificationHealth::from_config(config)),
            pam: probe_pam(loaded),
            models: Fact::probed(models),
            user,
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConfigHealth {
    pub path: String,
    pub outcome: ConfigOutcome,
}

#[derive(Debug, Clone)]
pub enum ConfigOutcome {
    Valid {
        /// `device.path`, `None` for auto-detect — surfaced beside "valid"
        /// because it is the setting people misconfigure most.
        device_path: Option<String>,
    },
    NotFound,
    Invalid {
        error: String,
    },
}

impl ConfigHealth {
    pub fn from_load(loaded: &ConfigLoad) -> Self {
        let outcome = match &loaded.result {
            Ok(config) => ConfigOutcome::Valid {
                device_path: config.device.path.clone(),
            },
            Err(facelock_core::config::ConfigError::NotFound(_)) => ConfigOutcome::NotFound,
            Err(e) => ConfigOutcome::Invalid {
                error: e.to_string(),
            },
        };
        ConfigHealth {
            path: loaded.path.display().to_string(),
            outcome,
        }
    }
}

// ---------------------------------------------------------------------------
// Oneshot fallback (F7)
// ---------------------------------------------------------------------------

/// F7: whether the three things PAM's oneshot fallback needs from *this*
/// section are on disk, if the daemon were unreachable at auth time.
///
/// The claim is deliberately narrower than "auth would succeed", because the
/// evidence is narrower. What is checked is presence: [`AUTH_BIN`] at its
/// hardcoded path, both configured model files, and the database. Whether
/// *this user* would then match is the Enrolled section's answer — a missing
/// enrollment fails one user, a missing binary fails the mechanism.
///
/// Two gaps keep this from being a verdict on authentication, and both are
/// reported elsewhere rather than folded in here:
///
/// - A camera and (under encryption) key material are equally required, and
///   equally fail the mechanism. They have their own sections, with their own
///   probes; duplicating their conclusions into this line would mean two
///   places to keep true.
/// - Presence is not loadability. A model file whose SHA256 does not match
///   is present and still refuses to load, so even all three checks passing
///   does not prove `facelock auth` would run.
#[derive(Debug, Clone)]
pub struct OneshotFallback {
    pub auth_bin: String,
    pub auth_bin_present: bool,
    pub models_present: bool,
    pub database_present: bool,
}

impl OneshotFallback {
    /// All three files this section checks are present. Named for what it
    /// tests: it is not a prediction that authentication would succeed — see
    /// the type's docs for what it deliberately does not cover.
    pub fn prerequisites_present(&self) -> bool {
        self.auth_bin_present && self.models_present && self.database_present
    }
}

/// Takes the [`ModelFiles`] **value**, not the [`Fact`] it is later lifted
/// into. A `bool` argument let the caller write
/// `models.known().is_some_and(|m| m.all_present())`, which answers a
/// confident `false` for a fact nobody established — the report would then
/// state "models: missing / fallback not usable" on the strength of a probe
/// that did not run. Taking the value makes that call unrepresentable rather
/// than merely unreachable.
fn probe_oneshot_fallback_at(
    auth_bin: &Path,
    db_path: &Path,
    models: &ModelFiles,
) -> OneshotFallback {
    OneshotFallback {
        auth_bin: auth_bin.display().to_string(),
        auth_bin_present: auth_bin.is_file(),
        models_present: models.all_present(),
        database_present: db_path.is_file(),
    }
}

// ---------------------------------------------------------------------------
// Camera
// ---------------------------------------------------------------------------

/// The camera section: presence (H2's [`CameraPresence`]) plus, when a device
/// is reachable, the interrogated capabilities — including `applied_quirks`,
/// so "why did this camera behave oddly" is answerable from `status` (D8).
#[derive(Debug, Clone)]
pub enum CameraHealth {
    /// No path configured. `resolved` is what auto-detection would pick
    /// right now, when it finds something.
    AutoDetect {
        resolved: Option<InterrogatedCamera>,
    },
    Configured {
        path: String,
        present: bool,
        /// `None` when the node is absent or interrogation failed.
        interrogated: Option<InterrogatedCamera>,
    },
}

/// The slice of `ResolvedCamera` the report renders — plain data, so a
/// synthetic `Health` needs no camera crate machinery.
#[derive(Debug, Clone)]
pub struct InterrogatedCamera {
    pub path: String,
    pub name: String,
    pub is_ir: bool,
    pub applied_quirks: Vec<String>,
}

impl From<&facelock_camera::ResolvedCamera> for InterrogatedCamera {
    fn from(resolved: &facelock_camera::ResolvedCamera) -> Self {
        InterrogatedCamera {
            path: resolved.info.path.clone(),
            name: resolved.info.name.clone(),
            is_ir: resolved.caps.is_ir,
            applied_quirks: resolved.caps.applied_quirks.clone(),
        }
    }
}

fn probe_camera(config: &Config) -> Fact<CameraHealth> {
    // Presence via the shared probe (D7); interrogation via the camera
    // domain's one implementation (D8) — never re-derived here.
    camera_fact(CameraPresence::probe(config), || {
        crate::direct::resolve_camera_device(config)
            .ok()
            .map(|r| InterrogatedCamera::from(&r))
    })
}

/// The camera rule, split from the device interrogation so it is testable
/// without hardware (same shape as `backend::classify`).
///
/// Every arm is [`Fact::Probed`], including auto-detect finding nothing:
/// detection *ran*, and its coming back empty is an observation about this
/// machine, not an unverified reading of the config.
fn camera_fact(
    presence: CameraPresence,
    interrogate: impl FnOnce() -> Option<InterrogatedCamera>,
) -> Fact<CameraHealth> {
    match presence {
        CameraPresence::AutoDetect => Fact::probed(CameraHealth::AutoDetect {
            resolved: interrogate(),
        }),
        CameraPresence::Configured { path, present } => Fact::probed(CameraHealth::Configured {
            // An absent node has nothing to interrogate; don't open a device
            // to confirm what the stat already answered.
            interrogated: present.then(interrogate).flatten(),
            path,
            present,
        }),
    }
}

// ---------------------------------------------------------------------------
// Encryption
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EncryptionHealth {
    pub method: EncryptionMethod,
    pub material: KeyMaterial,
    /// `(sealed, plaintext)` embedding counts, when the database was
    /// readable. `None` is "could not count", not zero.
    pub sealed_counts: Option<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub enum KeyMaterial {
    Tpm {
        sealed_key_path: String,
        sealed_key_present: bool,
        device_path: String,
        device_present: bool,
    },
    Keyfile {
        key_path: String,
        present: bool,
    },
    None,
}

fn probe_encryption(config: &Config) -> EncryptionHealth {
    let material = match config.encryption.method {
        EncryptionMethod::Tpm => {
            let device_path = config
                .tpm
                .tcti
                .strip_prefix("device:")
                .unwrap_or(&config.tpm.tcti)
                .to_string();
            KeyMaterial::Tpm {
                sealed_key_present: Path::new(&config.encryption.sealed_key_path).exists(),
                sealed_key_path: config.encryption.sealed_key_path.clone(),
                device_present: Path::new(&device_path).exists(),
                device_path,
            }
        }
        EncryptionMethod::Keyfile => KeyMaterial::Keyfile {
            present: Path::new(&config.encryption.key_path).exists(),
            key_path: config.encryption.key_path.clone(),
        },
        EncryptionMethod::None => KeyMaterial::None,
    };

    let sealed_counts = FaceStore::open_readonly(Path::new(&config.storage.db_path))
        .ok()
        .and_then(|store| store.count_sealed().ok());

    EncryptionHealth {
        method: config.encryption.method.clone(),
        material,
        sealed_counts,
    }
}

// ---------------------------------------------------------------------------
// Enrollment (DEC-7: one fact, two renderers)
// ---------------------------------------------------------------------------

/// The target user's enrollment as the authoritative store reports it, plus
/// the marker diagnostic — `status` may read the store (root) and may note a
/// stale marker; `is-enrolled` reads only the marker and never comes here.
#[derive(Debug, Clone)]
pub struct EnrollmentHealth {
    pub models: Vec<ModelSummary>,
    pub marker: MarkerDiagnostic,
}

#[derive(Debug, Clone)]
pub struct ModelSummary {
    pub id: u32,
    pub label: String,
    /// Under hard device binding (`security.bind_device_aad`), this template
    /// carries no device id: it still authenticates, and only a re-enrollment
    /// binds it (#312). Always false with hard binding off.
    pub unbound: bool,
}

/// Does the `is-enrolled` marker agree with the database?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerDiagnostic {
    Consistent,
    /// The marker and the store disagree (an absent marker counts as 0) —
    /// `is-enrolled` is answering with stale data until a reconcile.
    Mismatch {
        marker: u32,
        store: u32,
    },
    Unreadable {
        why: String,
    },
}

/// The comparison rule, split from filesystem access for tests.
fn marker_diagnostic(marker: MarkerState, store_count: u32) -> MarkerDiagnostic {
    match marker {
        MarkerState::Enrolled(m) if m.models == store_count => MarkerDiagnostic::Consistent,
        MarkerState::Enrolled(m) => MarkerDiagnostic::Mismatch {
            marker: m.models,
            store: store_count,
        },
        MarkerState::Absent if store_count == 0 => MarkerDiagnostic::Consistent,
        MarkerState::Absent => MarkerDiagnostic::Mismatch {
            marker: 0,
            store: store_count,
        },
        MarkerState::Unreadable(why) => MarkerDiagnostic::Unreadable { why },
    }
}

/// The N4 discipline lives here: a store that cannot be read yields
/// [`Fact::Unknown`], never an empty model list. Only a provably absent
/// database ([`StoreError::Absent`], C7) is a *known* "nothing enrolled".
fn probe_enrolled(config: &Config, user: &str) -> Fact<EnrollmentHealth> {
    let store_count = match FaceStore::open_readonly(Path::new(&config.storage.db_path)) {
        Ok(store) => match store.list_models(user) {
            Ok(models) => Ok(models),
            Err(e) => Err(format!("error reading models: {e}")),
        },
        Err(StoreError::Absent { .. }) => Ok(Vec::new()),
        Err(e) => Err(format!("database not accessible: {e}")),
    };

    match store_count {
        Err(why) => Fact::unknown(why),
        Ok(models) => {
            let marker =
                enrollment_marker::read_marker_in(&enrollment_marker::marker_dir(config), user);
            Fact::probed(EnrollmentHealth {
                marker: marker_diagnostic(marker, models.len() as u32),
                models: models
                    .into_iter()
                    .map(|m| ModelSummary {
                        id: m.id,
                        unbound: config
                            .security
                            .classify_device_binding(m.device_id.as_deref())
                            == facelock_core::types::DeviceBinding::LegacyUnbound,
                        label: m.label,
                    })
                    .collect(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Security / notification posture (claims: rendered from config, unverified)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SecurityHealth {
    pub disabled: bool,
    pub require_ir: bool,
    pub require_frame_variance: bool,
    pub require_landmark_liveness: bool,
    pub min_auth_frames: u32,
}

impl SecurityHealth {
    pub fn from_config(config: &Config) -> Self {
        SecurityHealth {
            disabled: config.security.disabled,
            require_ir: config.security.require_ir,
            require_frame_variance: config.security.require_frame_variance,
            require_landmark_liveness: config.security.require_landmark_liveness,
            min_auth_frames: config.security.min_auth_frames,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NotificationHealth {
    pub mode: NotificationMode,
    pub notify_prompt: bool,
    pub notify_on_success: bool,
    pub notify_on_failure: bool,
}

impl NotificationHealth {
    pub fn from_config(config: &Config) -> Self {
        NotificationHealth {
            mode: config.notification.mode.clone(),
            notify_prompt: config.notification.notify_prompt,
            notify_on_success: config.notification.notify_on_success,
            notify_on_failure: config.notification.notify_on_failure,
        }
    }
}

// ---------------------------------------------------------------------------
// PAM wiring
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PamWiring {
    /// The primary path, named in the report's item line.
    pub module_path: String,
    /// Where the module was actually found, if anywhere.
    pub installed_at: Option<String>,
    /// Every service on the search path that carries the facelock line.
    ///
    /// A list rather than the single `sudo_configured: Option<bool>` this
    /// replaces: that field could not report a configured `polkit-1` or
    /// `omarchy-lock-face` at all, because it stat-ed `/etc/pam.d/sudo` and
    /// nothing else. Empty means nothing is configured **where the scan could
    /// look**, which is only the whole answer when [`PamWiring::not_checked`]
    /// is empty too.
    pub configured: Vec<ConfiguredService>,
    /// Where the scan could not get an answer: a directory it could not list,
    /// or a service file it could not read.
    ///
    /// The module header's rule, applied to PAM: an unknown is never rendered
    /// as a value. `configured` being empty while this is not means "I could
    /// not look", which is a different report line from "nothing is
    /// configured" — and rendering the two identically is what made a broken
    /// lock stack and a healthy one look the same.
    pub not_checked: Vec<NotChecked>,
}

fn probe_pam(loaded: &ConfigLoad) -> PamWiring {
    let paths: Vec<PathBuf> = PAM_MODULE_PATHS.iter().map(PathBuf::from).collect();
    // The config is already parsed here, so the search path comes off the
    // value rather than through `PamDirs::system`, which would open the file a
    // second time. A config that did not parse yields the default list, which
    // is what makes this section config-independent.
    let dirs = loaded
        .config()
        .map_or_else(PamDirs::default, PamDirs::from_config);
    probe_pam_at(&paths, &dirs)
}

fn probe_pam_at(module_paths: &[PathBuf], dirs: &PamDirs) -> PamWiring {
    // `commands::pam::configured_scan`, not a local reimplementation: it is
    // the same scan `pam status --all` runs, down to the row builder, so
    // `facelock status` cannot claim a service is configured where
    // `pam status` says it is not.
    let scan = configured_scan(dirs);
    PamWiring {
        module_path: module_paths
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        installed_at: module_paths
            .iter()
            .find(|p| p.exists())
            .map(|p| p.display().to_string()),
        configured: scan.services,
        not_checked: scan.not_checked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use crate::commands::enrollment_marker::Marker;
    use crate::resolved::ProbedFile;

    fn config_with(toml: &str) -> Config {
        Config::parse(toml).expect("test config parses")
    }

    // -----------------------------------------------------------------------
    // Camera: what was probed vs what was merely claimed
    // -----------------------------------------------------------------------

    /// Auto-detection running and finding nothing is a *probed* absence. It
    /// used to be tagged `Claimed`, which said the opposite — that the config
    /// had been taken at its word and nothing checked.
    #[test]
    fn auto_detect_finding_nothing_is_a_probed_absence() {
        let fact = camera_fact(CameraPresence::AutoDetect, || None);
        assert!(
            matches!(
                fact,
                Fact::Probed(CameraHealth::AutoDetect { resolved: None })
            ),
            "expected a probed absence, got {fact:?}"
        );
    }

    #[test]
    fn an_absent_configured_node_is_never_interrogated() {
        let fact = camera_fact(
            CameraPresence::Configured {
                path: "/dev/facelock-no-such-video".into(),
                present: false,
            },
            || panic!("an absent node must not be opened"),
        );
        assert!(
            matches!(
                fact,
                Fact::Probed(CameraHealth::Configured {
                    present: false,
                    interrogated: None,
                    ..
                })
            ),
            "{fact:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Enrollment: unknown is not false (N4)
    // -----------------------------------------------------------------------

    /// The money probe: an unreadable database is `Unknown`, never a known
    /// empty enrollment.
    #[test]
    fn unreadable_database_probes_to_unknown_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        fs::write(&db_path, b"this is not a sqlite database").unwrap();
        let config = config_with(&format!("[storage]\ndb_path = \"{}\"\n", db_path.display()));

        // SQLite defers content validation past open, so the corruption may
        // surface at either step; both must land in Unknown, never in a
        // known-empty enrollment.
        match probe_enrolled(&config, "alice") {
            Fact::Unknown { why } => {
                assert!(why.contains("database"), "{why}")
            }
            known => panic!("must not claim a known answer: {known:?}"),
        }
    }

    /// C7: a provably absent database is a *known* empty enrollment — and the
    /// probe must not create it.
    #[test]
    fn absent_database_probes_to_known_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        let config = config_with(&format!("[storage]\ndb_path = \"{}\"\n", db_path.display()));

        let fact = probe_enrolled(&config, "alice");
        let enrolled = fact.known().expect("absent store is a known answer");
        assert!(enrolled.models.is_empty());
        assert_eq!(enrolled.marker, MarkerDiagnostic::Consistent);
        assert!(!db_path.exists(), "the probe must not create the database");
    }

    /// The full root-path probe against a real store, marker included.
    #[test]
    fn enrolled_probe_reads_store_and_marker() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        {
            let store = FaceStore::create(&db_path).unwrap();
            store
                .add_model("alice", "front", &[0.5f32; 512], "embedder-a")
                .unwrap();
            store
                .add_model("alice", "side", &[0.4f32; 512], "embedder-a")
                .unwrap();
        }
        let config = config_with(&format!("[storage]\ndb_path = \"{}\"\n", db_path.display()));
        // The marker dir derives from db_path, so it lands in the tempdir.
        enrollment_marker::write_marker_in(
            &enrollment_marker::marker_dir(&config),
            "alice",
            2,
            None,
        )
        .unwrap();

        let fact = probe_enrolled(&config, "alice");
        let enrolled = fact.known().expect("readable store is a known answer");
        assert_eq!(enrolled.models.len(), 2);
        assert_eq!(enrolled.models[0].label, "front");
        assert!(
            !enrolled.models[0].unbound,
            "hard binding is off, so no row is unbound"
        );
        assert_eq!(enrolled.marker, MarkerDiagnostic::Consistent);

        // #312: the same id-less rows under hard binding are reported as
        // unbound, and still listed (they authenticate).
        let hard = config_with(&format!(
            "[storage]\ndb_path = \"{}\"\n[security]\nbind_device_aad = true\n",
            db_path.display()
        ));
        let fact = probe_enrolled(&hard, "alice");
        let enrolled = fact.known().unwrap();
        assert_eq!(enrolled.models.len(), 2);
        assert!(enrolled.models.iter().all(|m| m.unbound));

        // A user with no rows: known-empty, and their absent marker agrees.
        let fact = probe_enrolled(&config, "bob");
        let enrolled = fact.known().unwrap();
        assert!(enrolled.models.is_empty());
        assert_eq!(enrolled.marker, MarkerDiagnostic::Consistent);
    }

    // -----------------------------------------------------------------------
    // Marker diagnostic (DEC-7): the comparison rule
    // -----------------------------------------------------------------------

    #[test]
    fn marker_agreement_is_consistent() {
        let marker = MarkerState::Enrolled(Marker {
            models: 2,
            updated: "2026-08-14T00:00:00Z".into(),
        });
        assert_eq!(marker_diagnostic(marker, 2), MarkerDiagnostic::Consistent);
        assert_eq!(
            marker_diagnostic(MarkerState::Absent, 0),
            MarkerDiagnostic::Consistent
        );
    }

    #[test]
    fn marker_disagreement_is_a_mismatch_in_both_directions() {
        let stale_high = MarkerState::Enrolled(Marker {
            models: 3,
            updated: "2026-08-14T00:00:00Z".into(),
        });
        assert_eq!(
            marker_diagnostic(stale_high, 1),
            MarkerDiagnostic::Mismatch {
                marker: 3,
                store: 1
            }
        );
        // Enrolled in the store but no marker: is-enrolled is answering
        // "no" to an enrolled user.
        assert_eq!(
            marker_diagnostic(MarkerState::Absent, 2),
            MarkerDiagnostic::Mismatch {
                marker: 0,
                store: 2
            }
        );
    }

    #[test]
    fn broken_marker_reports_unreadable() {
        assert_eq!(
            marker_diagnostic(MarkerState::Unreadable("bad json".into()), 1),
            MarkerDiagnostic::Unreadable {
                why: "bad json".into()
            }
        );
    }

    // -----------------------------------------------------------------------
    // Oneshot fallback (F7)
    // -----------------------------------------------------------------------

    /// A `ModelFiles` for a directory that either holds both files or
    /// neither — the only distinction the fallback derivation reads.
    fn model_files(present: bool) -> ModelFiles {
        let dir = PathBuf::from("/usr/share/facelock/models");
        ModelFiles {
            detector: ProbedFile {
                path: dir.join("det.onnx"),
                present,
            },
            embedder: ProbedFile {
                path: dir.join("emb.onnx"),
                present,
            },
            dir_present: true,
            dir,
        }
    }

    #[test]
    fn fallback_is_usable_only_with_binary_models_and_database() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("facelock");
        let db = dir.path().join("facelock.db");
        fs::write(&bin, b"#!").unwrap();
        fs::write(&db, b"db").unwrap();

        let fallback = probe_oneshot_fallback_at(&bin, &db, &model_files(true));
        assert!(fallback.auth_bin_present);
        assert!(fallback.database_present);
        assert!(fallback.models_present);
        assert!(fallback.prerequisites_present());

        let no_models = probe_oneshot_fallback_at(&bin, &db, &model_files(false));
        assert!(!no_models.models_present);
        assert!(!no_models.prerequisites_present());

        let no_bin =
            probe_oneshot_fallback_at(&dir.path().join("missing"), &db, &model_files(true));
        assert!(!no_bin.auth_bin_present);
        assert!(!no_bin.prerequisites_present());

        let no_db =
            probe_oneshot_fallback_at(&bin, &dir.path().join("missing.db"), &model_files(true));
        assert!(!no_db.database_present);
        assert!(!no_db.prerequisites_present());
    }

    /// N4 at the derivation site. `models_present` must be read off the
    /// `ModelFiles` *value*; the shape this replaced passed
    /// `models.known().is_some_and(|m| m.all_present())`, and `Fact::known`
    /// answers `None` for an unknown fact — so an undetermined model probe
    /// would have rendered as a confident "models: missing / fallback not
    /// usable", a guessed false answer from a fact nobody established. That
    /// was unreachable only because `ModelFiles::probe` happens to be
    /// infallible today, which is a property of the implementation and not of
    /// the types. `probe_oneshot_fallback_at` now takes `&ModelFiles`, so the
    /// old expression does not type-check; this pin fails if a `Fact` is ever
    /// routed back through `known()` inside the constructor.
    /// `AUTH_BIN` is duplicated, not shared: `pam-facelock` may not depend on
    /// this crate (or any facelock crate), so the path PAM actually executes
    /// is declared there and mirrored here. A comment was the only thing
    /// coupling the two — and the failure mode is silent, since a diverged
    /// copy makes `status` report on a binary PAM never spawns. Read PAM's
    /// declaration and compare.
    #[test]
    fn auth_bin_matches_the_path_pam_actually_spawns() {
        let pam_src = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../pam-facelock/src/lib.rs"),
        )
        .expect("pam-facelock source must be readable from the workspace");

        // Assembled so this test's own prose is not what it matches on.
        let decl = format!("const AUTH_BIN: &str = {}", '"');
        let pam_value = pam_src
            .lines()
            .find_map(|l| l.trim().strip_prefix(&decl)?.split_once('"'))
            .map(|(value, _)| value.to_string())
            .expect("pam-facelock must declare AUTH_BIN as a string literal");

        assert_eq!(
            pam_value, AUTH_BIN,
            "health::AUTH_BIN ({AUTH_BIN}) and pam-facelock's AUTH_BIN \
             ({pam_value}) have diverged. `status` would then report on a \
             binary the PAM module never spawns. They are separate constants \
             because pam-facelock takes no facelock dependency; keep them equal."
        );
    }

    #[test]
    fn probe_derives_the_fallback_from_the_model_value_not_a_fact() {
        let source =
            fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/health.rs"))
                .unwrap();
        let body = source
            .split_once("pub fn probe(loaded: &ConfigLoad) -> Health {")
            .expect("Health::probe must exist")
            .1
            .split_once("\n    }\n")
            .expect("Health::probe must close at method indentation")
            .0;
        // Comment lines are ignored so the prose above may name the shape it
        // forbids (same rule as the pins in `resolved`).
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("known()"),
            "Health::probe must derive the fallback's models_present from the \
             ModelFiles value, never from a Fact — an Unknown fact collapses \
             to a confident false (N4). Body was:\n{body}"
        );
    }

    // -----------------------------------------------------------------------
    // Encryption
    // -----------------------------------------------------------------------

    #[test]
    fn keyfile_probe_reports_key_presence_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("facelock.key");
        let db_path = dir.path().join("facelock.db");
        {
            let store = FaceStore::create(&db_path).unwrap();
            store
                .add_model("alice", "front", &[0.5f32; 512], "e")
                .unwrap();
        }
        let config = config_with(&format!(
            "[encryption]\nmethod = \"keyfile\"\nkey_path = \"{}\"\n\n[storage]\ndb_path = \"{}\"\n",
            key.display(),
            db_path.display()
        ));

        let health = probe_encryption(&config);
        assert_eq!(health.method, EncryptionMethod::Keyfile);
        match &health.material {
            KeyMaterial::Keyfile { present, key_path } => {
                assert!(!present);
                assert_eq!(*key_path, key.display().to_string());
            }
            other => panic!("expected keyfile material, got {other:?}"),
        }
        // One plaintext embedding, zero sealed.
        assert_eq!(health.sealed_counts, Some((0, 1)));

        fs::write(&key, b"k").unwrap();
        let health = probe_encryption(&config);
        assert!(matches!(
            health.material,
            KeyMaterial::Keyfile { present: true, .. }
        ));
    }

    #[test]
    fn tpm_probe_reports_sealed_key_and_device_separately() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = dir.path().join("sealed.key");
        let device = dir.path().join("tpmrm0");
        fs::write(&device, b"").unwrap();
        let config = config_with(&format!(
            "[encryption]\nmethod = \"tpm\"\nsealed_key_path = \"{}\"\n\n[tpm]\ntcti = \"device:{}\"\n",
            sealed.display(),
            device.display()
        ));

        let health = probe_encryption(&config);
        match &health.material {
            KeyMaterial::Tpm {
                sealed_key_present,
                device_present,
                device_path,
                ..
            } => {
                assert!(!sealed_key_present);
                assert!(device_present, "tcti device: prefix must be stripped");
                assert_eq!(*device_path, device.display().to_string());
            }
            other => panic!("expected TPM material, got {other:?}"),
        }
        // No database: counts are unknown, not zero.
        assert_eq!(health.sealed_counts, None);
    }

    // -----------------------------------------------------------------------
    // PAM wiring
    // -----------------------------------------------------------------------

    #[test]
    fn pam_probe_finds_the_module_at_either_path() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("lib/security/pam_facelock.so");
        let alt = dir.path().join("usr/lib/security/pam_facelock.so");
        let paths = vec![primary.clone(), alt.clone()];
        let pam_d = dir.path().join("pam.d");
        fs::create_dir_all(&pam_d).unwrap();
        let dirs = PamDirs::new(vec![pam_d.clone()]);

        let wiring = probe_pam_at(&paths, &dirs);
        assert_eq!(wiring.module_path, primary.display().to_string());
        assert_eq!(wiring.installed_at, None);

        fs::create_dir_all(alt.parent().unwrap()).unwrap();
        fs::write(&alt, b"elf").unwrap();
        let wiring = probe_pam_at(&paths, &dirs);
        assert_eq!(wiring.installed_at, Some(alt.display().to_string()));

        fs::create_dir_all(primary.parent().unwrap()).unwrap();
        fs::write(&primary, b"elf").unwrap();
        let wiring = probe_pam_at(&paths, &dirs);
        assert_eq!(
            wiring.installed_at,
            Some(primary.display().to_string()),
            "the primary path wins when both exist"
        );
    }

    /// The defect this section replaces: the probe stat-ed `/etc/pam.d/sudo`
    /// and nothing else, so a configured `polkit-1` or `omarchy-lock-face` was
    /// invisible to `facelock status` however correctly it was wired up.
    #[test]
    fn the_pam_probe_reports_every_configured_service_not_just_sudo() {
        let dir = tempfile::tempdir().unwrap();
        let pam_d = dir.path().join("pam.d");
        fs::create_dir_all(&pam_d).unwrap();
        fs::write(pam_d.join("sudo"), b"auth sufficient pam_facelock.so\n").unwrap();
        fs::write(
            pam_d.join("omarchy-lock-face"),
            b"auth sufficient pam_facelock.so\n",
        )
        .unwrap();
        fs::write(pam_d.join("login"), b"auth include system-auth\n").unwrap();

        let wiring = probe_pam_at(&[], &PamDirs::new(vec![pam_d.clone()]));

        let names: Vec<&str> = wiring
            .configured
            .iter()
            .map(|service| service.service.as_str())
            .collect();
        assert_eq!(names, ["omarchy-lock-face", "sudo"]);
        assert_eq!(
            wiring.configured[1].path,
            pam_d.join("sudo").display().to_string()
        );
        assert!(wiring.not_checked.is_empty());
    }

    /// An `/etc` copy of a package's file is configured **and** carries the
    /// vendor path it hides, which is what says it will not follow the
    /// package's updates.
    #[test]
    fn a_local_override_carries_the_vendor_path_it_shadows() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        let vendor = dir.path().join("vendor");
        fs::create_dir_all(&etc).unwrap();
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("polkit-1"), b"auth include system-auth\n").unwrap();
        fs::write(etc.join("polkit-1"), b"auth sufficient pam_facelock.so\n").unwrap();

        let wiring = probe_pam_at(&[], &PamDirs::new(vec![etc.clone(), vendor.clone()]));

        assert_eq!(wiring.configured.len(), 1);
        assert_eq!(
            wiring.configured[0].shadows,
            Some(vendor.join("polkit-1").display().to_string())
        );
    }

    /// **Unknown is never guessed** (the module header's rule): a directory
    /// the probe could not read is reported as unchecked rather than folded
    /// into "nothing configured". Skipped as root, which ignores the mode
    /// bits.
    #[test]
    fn an_unreadable_directory_is_not_checked_rather_than_not_configured() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pam_d = dir.path().join("pam.d");
        fs::create_dir_all(&pam_d).unwrap();
        fs::set_permissions(&pam_d, fs::Permissions::from_mode(0o000)).unwrap();

        let wiring = probe_pam_at(&[], &PamDirs::new(vec![pam_d.clone()]));

        assert!(wiring.configured.is_empty());
        assert_eq!(wiring.not_checked.len(), 1);
        assert_eq!(wiring.not_checked[0].path, pam_d.display().to_string());
        assert!(!wiring.not_checked[0].error.is_empty());

        fs::set_permissions(&pam_d, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // -----------------------------------------------------------------------
    // Config outcome
    // -----------------------------------------------------------------------

    #[test]
    fn config_health_carries_the_parse_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let missing = ConfigHealth::from_load(&ConfigLoad {
            path: path.clone(),
            result: Config::load_from(&path),
        });
        assert!(matches!(missing.outcome, ConfigOutcome::NotFound));

        fs::write(&path, "[device\n").unwrap();
        let broken = ConfigHealth::from_load(&ConfigLoad {
            path: path.clone(),
            result: Config::load_from(&path),
        });
        assert!(matches!(broken.outcome, ConfigOutcome::Invalid { .. }));

        fs::write(&path, "[device]\npath = \"/dev/video2\"\n").unwrap();
        let valid = ConfigHealth::from_load(&ConfigLoad {
            path: path.clone(),
            result: Config::load_from(&path),
        });
        match valid.outcome {
            ConfigOutcome::Valid { device_path } => {
                assert_eq!(device_path.as_deref(), Some("/dev/video2"))
            }
            other => panic!("expected valid, got {other:?}"),
        }
    }
}
