//! `facelock status --json` — the machine renderer over [`Health`].
//!
//! The second renderer of the same domain value. [`commands::status::render`]
//! writes the report a person reads; this writes the document a script parses,
//! and both are pure functions of one [`Health`] — so
//! `the_two_renderers_agree_about_every_section` can feed one fixture to both
//! and fail the build when they disagree about a verdict, or when a section
//! reaches one renderer and not the other.
//!
//! [`commands::status::render`]: crate::commands::status::render
//!
//! # Why the wire types live here and not in `health.rs`
//!
//! Nothing in [`crate::health`] derives `Serialize`, deliberately. Field names
//! in *this* file are a published contract (`docs/contracts.md`,
//! "facelock status Semantics"); Rust names in the domain are not, and must
//! stay free to be renamed, split or reshaped by the next probe that needs it.
//! Deriving on the domain would silently fuse the two, and the first
//! `sudo_configured` → `configured` rename would have been a breaking change to
//! a document nobody meant to touch. So every type below is built *from* a
//! `Health` and owes it nothing but the facts.
//!
//! # Unknown is a value here, not an absence
//!
//! `health.rs`'s rule (N4) is harder to keep in JSON than in prose, because
//! `null` and `false` read as answers. So no fact is ever rendered as a bare
//! `null` or a defaulted `false`: every one carries a [`State`] of `ok`,
//! `problem` or `unknown`, and the reason lives beside it. The nested facts
//! that can be undetermined — the camera interrogation, the embedding counts,
//! the enrollment marker, the PAM scan — are [`Section`]s of their own for
//! exactly that reason.
//!
//! # No gettext, ever
//!
//! This document is machine output, so it does not enter the message seam
//! (`message/mod.rs`, "What must NOT come through here") and nothing in the
//! renderer calls the catalog.
//!
//! That is a rule about **catalog** text, not a promise that every `reason` is
//! a fixed string. Two of them embed the diagnostic their probe captured, and
//! deliberately: `enrollment.reason` forwards
//! `database not accessible: {store error}`, and `enrollment.marker.reason`
//! forwards the marker file's own read failure. An `io::Error` rendered through
//! the C library follows `LC_MESSAGES` like any other `strerror` string, which
//! is the same locale-dependent-diagnostic class `pam`'s `error` field already
//! documents. What must never appear is *our* catalog's output — see
//! [`unknown_reason`] for the one place that costs something, and for the pins
//! that keep it true.
//!
//! One field was dropped rather than pinned. The `daemon` section carries **no**
//! free-text diagnostic, because the transport's error string is an `anyhow`
//! chain that `ipc_client` deliberately localizes before `status` ever sees it;
//! [`daemon_section`] has the reasoning. The rule that generalizes from it: a
//! `String` copied into this document from a `facelock-cli` module that no pin
//! covers is the shape to distrust, whether it lands in a `reason` or an
//! `error`.

use serde::Serialize;

use crate::backend::DaemonPing;
use crate::health::{
    CameraHealth, ConfigHealth, ConfigOutcome, ConfiguredService, EncryptionHealth,
    EnrollmentHealth, Health, InterrogatedCamera, KeyMaterial, MarkerDiagnostic, NotChecked,
    NotificationHealth, OneshotFallback, PamWiring, SecurityHealth,
};
use crate::resolved::{EpStatus, ExecutionProviderFact, Fact, ModelFiles};

/// The well-known bus name, as the human report's item line prints it.
///
/// A literal rather than a reference to the zbus proxy's attribute: the report
/// states this even when the config did not parse and no client was ever
/// built, and it is the *name*, not the connection, that is being reported.
const BUS_NAME: &str = "org.facelock.Daemon";

/// The `reason` every config-dependent fact carries when the config file did
/// not parse.
///
/// The same words the catalog's `StatusWhyConfigNotAvailable` renders in the C
/// locale, deliberately duplicated as a literal instead of being read from the
/// catalog: this document must not localize, and a `translate()` call here
/// would put a French sentence in a `--json` payload the moment the operator
/// set `LC_MESSAGES`.
const CONFIG_NOT_AVAILABLE: &str = "config not available";

// ---------------------------------------------------------------------------
// The verdict every fact carries
// ---------------------------------------------------------------------------

/// How a fact stands: known-good, known-bad, or not established.
///
/// **New words may be added**, so a consumer tolerates one it does not know
/// rather than treating it as an error — the rule the `pam --json` `action`
/// vocabulary already sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Established, and nothing is wrong with it.
    Ok,
    /// Established, and it is a finding: the thing is missing, unreachable or
    /// misconfigured.
    Problem,
    /// **Not** a finding and **not** a `false`: the probe could not determine
    /// the answer, and `reason` says what stood in the way.
    Unknown,
}

/// A [`State`] and, when it is not [`State::Ok`], why.
///
/// The constructors are the enforcement: there is no way to build a `problem`
/// or an `unknown` without a reason, and no way to attach one to an `ok`.
#[derive(Debug, Clone, Serialize)]
struct Verdict {
    state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl Verdict {
    fn ok() -> Self {
        Verdict {
            state: State::Ok,
            reason: None,
        }
    }

    fn problem(reason: impl Into<String>) -> Self {
        Verdict {
            state: State::Problem,
            reason: Some(reason.into()),
        }
    }

    fn unknown(reason: impl Into<String>) -> Self {
        Verdict {
            state: State::Unknown,
            reason: Some(reason.into()),
        }
    }

    /// `ok` when the condition holds, `problem` with this reason otherwise.
    fn ok_or(ok: bool, reason: impl Into<String>) -> Self {
        if ok {
            Verdict::ok()
        } else {
            Verdict::problem(reason)
        }
    }
}

/// One section of the document: a verdict, flattened, plus whatever detail the
/// probe managed to gather.
///
/// The detail is separate from the verdict rather than repeated into each
/// state's variant, because most sections keep reporting facts across a
/// verdict change — a camera that is not present still has a configured path,
/// a broken key file still has a path and a method. `None` is for the facts a
/// failed probe never learned, and the document omits the keys rather than
/// emitting them as `null`.
#[derive(Debug, Clone, Serialize)]
struct Section<T> {
    #[serde(flatten)]
    verdict: Verdict,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    detail: Option<T>,
}

impl<T> Section<T> {
    fn new(verdict: Verdict) -> Self {
        Section {
            verdict,
            detail: None,
        }
    }

    fn with(mut self, detail: T) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// The `reason` for a [`Fact::Unknown`], as machine output.
///
/// `why` cannot be forwarded verbatim, because exactly one producer of it is
/// localized: `health::config_not_available`, the reason every
/// config-dependent fact carries when the file did not parse. Emitting that
/// string would put catalog output in a `--json` document.
///
/// The substitution is exact rather than a guess. `Health::probe` returns the
/// all-unknown shape **only** from the `loaded.config()` is-`None` branch, and
/// `ConfigHealth::from_load` reports [`ConfigOutcome::Valid`] exactly when that
/// same `Result` is `Ok` — so "the config did not parse" and "every unknown in
/// this value is the localized one" are the same condition. Every other `why`
/// in `health.rs` is a `format!` of an English literal and a store error, which
/// is the same locale-dependent-diagnostic class as `pam`'s `error` field, and
/// is passed through.
///
/// `health_has_exactly_one_localized_why` pins the premise: a second
/// `localized(` call in `health.rs` fails the build here rather than shipping a
/// translated `reason`.
fn unknown_reason(config: &ConfigHealth, why: &str) -> String {
    match config.outcome {
        ConfigOutcome::Valid { .. } => why.to_string(),
        ConfigOutcome::NotFound | ConfigOutcome::Invalid { .. } => CONFIG_NOT_AVAILABLE.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// `facelock status --json`, as data. One key per section of the human report,
/// in the order the report renders them.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    config: Section<ConfigDetail>,
    daemon: Section<DaemonDetail>,
    oneshot_fallback: Section<FallbackDetail>,
    camera: Section<CameraDetail>,
    models: Section<ModelsDetail>,
    execution_provider: Section<ProviderDetail>,
    encryption: Section<EncryptionDetail>,
    enrollment: Section<EnrollmentDetail>,
    security: Section<SecurityDetail>,
    notifications: Section<NotificationsDetail>,
    pam: Section<PamDetail>,
}

/// Render a [`Health`] as the `--json` document.
///
/// Fallible only in signature: every type below serializes plain scalars,
/// strings and sequences, so there is no input that makes `serde_json` fail.
/// It is still a `Result` rather than an `unwrap` — library code does not
/// panic, and the caller already returns one.
pub fn render(health: &Health) -> serde_json::Result<String> {
    serde_json::to_string(&StatusReport::from_health(health))
}

impl StatusReport {
    fn from_health(health: &Health) -> Self {
        let config = &health.config;
        StatusReport {
            config: config_section(config),
            daemon: daemon_section(config, &health.daemon),
            oneshot_fallback: fallback_section(config, &health.oneshot_fallback),
            camera: camera_section(config, &health.camera),
            models: models_section(config, &health.models),
            execution_provider: provider_section(config, &health.execution_provider),
            encryption: encryption_section(config, &health.encryption),
            enrollment: enrollment_section(config, &health.user, &health.enrolled),
            security: security_section(config, &health.security),
            notifications: notifications_section(config, &health.notifications),
            pam: pam_section(&health.pam),
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct ConfigDetail {
    path: String,
    outcome: ConfigWord,
    /// The parser's own message. A diagnostic, not a contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// What the file claims about the camera; absent when the file did not
    /// parse and there is no claim to report.
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<DeviceSelection>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConfigWord {
    Valid,
    NotFound,
    Invalid,
}

/// `device.path`, as the claim it is: a path, or auto-detection.
///
/// Internally tagged rather than a nullable `device_path`, because `null`
/// there would be a second spelling of "not established" for a value that is
/// perfectly well established — the operator configured nothing, on purpose.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "selection", rename_all = "snake_case")]
enum DeviceSelection {
    Configured { path: String },
    AutoDetect,
}

fn config_section(config: &ConfigHealth) -> Section<ConfigDetail> {
    let (verdict, outcome, error, device) = match &config.outcome {
        ConfigOutcome::Valid { device_path } => (
            Verdict::ok(),
            ConfigWord::Valid,
            None,
            Some(match device_path {
                Some(path) => DeviceSelection::Configured { path: path.clone() },
                None => DeviceSelection::AutoDetect,
            }),
        ),
        ConfigOutcome::NotFound => (
            Verdict::problem("config file not found"),
            ConfigWord::NotFound,
            None,
            None,
        ),
        ConfigOutcome::Invalid { error } => (
            Verdict::problem("config file did not parse"),
            ConfigWord::Invalid,
            Some(error.clone()),
            None,
        ),
    };
    Section::new(verdict).with(ConfigDetail {
        path: config.path.clone(),
        outcome,
        error,
        device,
    })
}

// ---------------------------------------------------------------------------
// Daemon
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct DaemonDetail {
    /// Reported even when the fact is unknown: the bus name is a property of
    /// the design, not of this machine, exactly as the human report's item
    /// line treats it.
    bus_name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reachability: Option<Reachability>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Reachability {
    Responding,
    NotResponding,
    /// `daemon.mode = "oneshot"`: the bus was never asked, and that is the
    /// configured behaviour rather than a failure.
    NotConfigured,
}

/// **This section deliberately carries no free-text diagnostic.**
///
/// `DaemonPing::NotResponding.error` is the transport's `anyhow` chain
/// rendered, and `ipc_client`'s `add_access_denied_hint` attaches a
/// **localized** context to it on a D-Bus `AccessDenied` — a hint written for a
/// human reading stderr, which `to_string()` then renders *instead of* the
/// underlying error. Forwarding it would put catalog output in a `--json`
/// payload on any machine with a translation installed, and a denied bus policy
/// is exactly the broken install `status` exists to diagnose.
///
/// Stripping the hint at the source belongs to the transport, which is behind
/// the #106 boundary, so the field is dropped instead. Nothing is lost that a
/// machine can act on: `reachability` says the round trip failed and `reason`
/// says so in fixed words. The human still gets the hint, on the report and on
/// stderr, which is who it was written for.
fn daemon_section(config: &ConfigHealth, daemon: &Fact<DaemonPing>) -> Section<DaemonDetail> {
    let (verdict, reachability) = match daemon {
        Fact::Unknown { why } => (Verdict::unknown(unknown_reason(config, why)), None),
        Fact::Probed(ping) | Fact::Claimed(ping) => match ping {
            DaemonPing::Responding => (Verdict::ok(), Some(Reachability::Responding)),
            DaemonPing::NotConfigured => (Verdict::ok(), Some(Reachability::NotConfigured)),
            DaemonPing::NotResponding { .. } => (
                Verdict::problem("daemon not responding"),
                Some(Reachability::NotResponding),
            ),
        },
    };
    Section::new(verdict).with(DaemonDetail {
        bus_name: BUS_NAME,
        reachability,
    })
}

// ---------------------------------------------------------------------------
// Oneshot fallback
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct FallbackDetail {
    auth_bin: String,
    binary_present: bool,
    models_present: bool,
    database_present: bool,
}

fn fallback_section(
    config: &ConfigHealth,
    fallback: &Fact<OneshotFallback>,
) -> Section<FallbackDetail> {
    match fallback {
        Fact::Unknown { why } => Section::new(Verdict::unknown(unknown_reason(config, why))),
        Fact::Probed(fallback) | Fact::Claimed(fallback) => Section::new(Verdict::ok_or(
            fallback.prerequisites_present(),
            "oneshot fallback prerequisites missing",
        ))
        .with(FallbackDetail {
            auth_bin: fallback.auth_bin.clone(),
            binary_present: fallback.auth_bin_present,
            models_present: fallback.models_present,
            database_present: fallback.database_present,
        }),
    }
}

// ---------------------------------------------------------------------------
// Camera
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct CameraDetail {
    selection: CameraSelection,
    /// The configured node path; absent under auto-detection, where the
    /// config names none.
    #[serde(skip_serializing_if = "Option::is_none")]
    configured_path: Option<String>,
    /// Whether the configured node exists; absent under auto-detection, where
    /// there is no node to stat.
    #[serde(skip_serializing_if = "Option::is_none")]
    present: Option<bool>,
    /// What interrogating the device found — a fact of its own, because a
    /// device that is present and refuses to be opened is a different answer
    /// from one that was never there.
    device: Section<InterrogatedDevice>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CameraSelection {
    Configured,
    AutoDetect,
}

#[derive(Debug, Clone, Serialize)]
struct InterrogatedDevice {
    path: String,
    name: String,
    /// The interrogated IR classification (D8), not the config's claim.
    ir: bool,
    /// Quirks-database entries that will shape how the device is opened.
    quirks: Vec<String>,
}

impl From<&InterrogatedCamera> for InterrogatedDevice {
    fn from(camera: &InterrogatedCamera) -> Self {
        InterrogatedDevice {
            path: camera.path.clone(),
            name: camera.name.clone(),
            ir: camera.is_ir,
            quirks: camera.applied_quirks.clone(),
        }
    }
}

fn camera_section(config: &ConfigHealth, camera: &Fact<CameraHealth>) -> Section<CameraDetail> {
    let camera = match camera {
        Fact::Unknown { why } => {
            return Section::new(Verdict::unknown(unknown_reason(config, why)));
        }
        Fact::Probed(camera) | Fact::Claimed(camera) => camera,
    };

    match camera {
        CameraHealth::Configured {
            path,
            present,
            interrogated,
        } => {
            let device = match (present, interrogated) {
                (_, Some(camera)) => Section::new(Verdict::ok()).with(camera.into()),
                (false, None) => Section::new(Verdict::problem("device node not present")),
                // Present, and it would not answer. Not a `false` anywhere:
                // the capabilities of this camera are genuinely unestablished.
                (true, None) => Section::new(Verdict::unknown("device could not be interrogated")),
            };
            Section::new(Verdict::ok_or(
                *present,
                "configured camera device not present",
            ))
            .with(CameraDetail {
                selection: CameraSelection::Configured,
                configured_path: Some(path.clone()),
                present: Some(*present),
                device,
            })
        }
        // Detection *ran*; that it found nothing is an observation about this
        // machine, which is why the section stays `ok` — auto-detect is
        // enabled and working — while the device fact reports the absence.
        CameraHealth::AutoDetect { resolved } => Section::new(Verdict::ok()).with(CameraDetail {
            selection: CameraSelection::AutoDetect,
            configured_path: None,
            present: None,
            device: match resolved {
                Some(camera) => Section::new(Verdict::ok()).with(camera.into()),
                None => Section::new(Verdict::problem("auto-detection found no camera")),
            },
        }),
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct ModelsDetail {
    dir: String,
    dir_present: bool,
    /// The configured model files; absent when the directory itself is not
    /// there, which is where the human report stops too.
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<ModelFile>>,
}

#[derive(Debug, Clone, Serialize)]
struct ModelFile {
    purpose: ModelPurpose,
    path: String,
    present: bool,
}

/// Which of the two configured ONNX files a row is about.
///
/// An enum rather than a `&'static str` so a third model kind is a deliberate
/// edit here, the way every other vocabulary in this document is.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelPurpose {
    Detector,
    Embedder,
}

fn models_section(config: &ConfigHealth, models: &Fact<ModelFiles>) -> Section<ModelsDetail> {
    let models = match models {
        Fact::Unknown { why } => {
            return Section::new(Verdict::unknown(unknown_reason(config, why)));
        }
        Fact::Probed(models) | Fact::Claimed(models) => models,
    };

    let dir = models.dir.display().to_string();
    if !models.dir_present {
        return Section::new(Verdict::problem("model directory not found")).with(ModelsDetail {
            dir,
            dir_present: false,
            files: None,
        });
    }

    let files = [
        (ModelPurpose::Detector, &models.detector),
        (ModelPurpose::Embedder, &models.embedder),
    ]
    .into_iter()
    .map(|(purpose, file)| ModelFile {
        purpose,
        path: file.path.display().to_string(),
        present: file.present,
    })
    .collect();

    Section::new(Verdict::ok_or(
        models.all_present(),
        "configured model files missing",
    ))
    .with(ModelsDetail {
        dir,
        dir_present: true,
        files: Some(files),
    })
}

// ---------------------------------------------------------------------------
// Execution provider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct ProviderDetail {
    /// The provider name as the config spells it, verbatim — including a name
    /// the registry does not know, which is the misconfiguration this section
    /// exists to surface.
    configured: String,
    availability: Availability,
    /// The ONNX Runtime's own load/query failure. A diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Availability {
    Available,
    /// A valid name the installed runtime was not built with: ORT falls back
    /// to CPU silently at session creation.
    NotBuiltIn,
    /// Not a name the provider registry knows.
    ///
    /// Spelled `unrecognized` rather than `unknown` on purpose: `unknown` is
    /// the tri-state word for "not established", and this is established — the
    /// name is wrong.
    Unrecognized,
    /// The runtime could not be loaded or queried, which is itself a finding
    /// about the install rather than an undetermined fact.
    Unqueryable,
}

fn provider_section(
    config: &ConfigHealth,
    ep: &Fact<ExecutionProviderFact>,
) -> Section<ProviderDetail> {
    let ep = match ep {
        Fact::Unknown { why } => {
            return Section::new(Verdict::unknown(unknown_reason(config, why)));
        }
        Fact::Probed(ep) | Fact::Claimed(ep) => ep,
    };

    let (verdict, availability, error) = match &ep.status {
        EpStatus::Available => (Verdict::ok(), Availability::Available, None),
        EpStatus::NotBuiltIn => (
            Verdict::problem("execution provider not built into the installed ONNX Runtime"),
            Availability::NotBuiltIn,
            None,
        ),
        EpStatus::UnknownName => (
            Verdict::problem("unrecognized execution provider"),
            Availability::Unrecognized,
            None,
        ),
        EpStatus::Unqueryable(error) => (
            Verdict::problem("ONNX Runtime could not be queried"),
            Availability::Unqueryable,
            Some(error.clone()),
        ),
    };

    Section::new(verdict).with(ProviderDetail {
        configured: ep.configured.clone(),
        availability,
        error,
    })
}

// ---------------------------------------------------------------------------
// Encryption
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct EncryptionDetail {
    key: KeyProtection,
    /// How many stored embeddings are sealed and how many are not — a fact of
    /// its own, because a database that could not be counted must not report
    /// zero of each.
    embeddings: Section<EmbeddingCounts>,
}

/// The key material, tagged by the method that produced it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum KeyProtection {
    Tpm {
        sealed_key_path: String,
        sealed_key_present: bool,
        tpm_device_path: String,
        tpm_device_present: bool,
    },
    Keyfile {
        key_path: String,
        present: bool,
    },
    /// Embeddings are stored as plaintext.
    None,
}

#[derive(Debug, Clone, Serialize)]
struct EmbeddingCounts {
    encrypted: u32,
    plaintext: u32,
}

fn encryption_section(
    config: &ConfigHealth,
    encryption: &Fact<EncryptionHealth>,
) -> Section<EncryptionDetail> {
    let encryption = match encryption {
        Fact::Unknown { why } => {
            return Section::new(Verdict::unknown(unknown_reason(config, why)));
        }
        Fact::Probed(encryption) | Fact::Claimed(encryption) => encryption,
    };

    let (verdict, key) = match &encryption.material {
        KeyMaterial::Tpm {
            sealed_key_path,
            sealed_key_present,
            device_path,
            device_present,
        } => {
            let verdict = if !sealed_key_present {
                Verdict::problem("sealed key missing")
            } else if !device_present {
                Verdict::problem("TPM device missing")
            } else {
                Verdict::ok()
            };
            (
                verdict,
                KeyProtection::Tpm {
                    sealed_key_path: sealed_key_path.clone(),
                    sealed_key_present: *sealed_key_present,
                    tpm_device_path: device_path.clone(),
                    tpm_device_present: *device_present,
                },
            )
        }
        KeyMaterial::Keyfile { key_path, present } => (
            Verdict::ok_or(*present, "key file missing"),
            KeyProtection::Keyfile {
                key_path: key_path.clone(),
                present: *present,
            },
        ),
        KeyMaterial::None => (
            Verdict::problem("embeddings are stored as plaintext"),
            KeyProtection::None,
        ),
    };

    // `None` here is "the database could not be counted", never "there are
    // none" — the one place this section could have turned an unread store
    // into a confident zero.
    let embeddings = match encryption.sealed_counts {
        Some((encrypted, plaintext)) => Section::new(Verdict::ok()).with(EmbeddingCounts {
            encrypted,
            plaintext,
        }),
        None => Section::new(Verdict::unknown("embedding counts could not be read")),
    };

    Section::new(verdict).with(EncryptionDetail { key, embeddings })
}

// ---------------------------------------------------------------------------
// Enrollment
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct EnrollmentDetail {
    /// The user the section describes — `--user` semantics, so a
    /// `sudo facelock status` reports on the invoking human. Present even when
    /// the enrollment itself is unknown, exactly as the report's item line is.
    user: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    models: Option<Vec<EnrolledModel>>,
    /// Whether the `is-enrolled` marker agrees with the database.
    #[serde(skip_serializing_if = "Option::is_none")]
    marker: Option<Section<MarkerCounts>>,
}

#[derive(Debug, Clone, Serialize)]
struct EnrolledModel {
    id: u32,
    label: String,
}

#[derive(Debug, Clone, Serialize)]
struct MarkerCounts {
    marker_models: u32,
    store_models: u32,
}

fn enrollment_section(
    config: &ConfigHealth,
    user: &str,
    enrolled: &Fact<EnrollmentHealth>,
) -> Section<EnrollmentDetail> {
    let enrolled = match enrolled {
        // The money case, in wire form: an unreadable store leaves `models`
        // absent, never an empty array a consumer would read as "not
        // enrolled".
        Fact::Unknown { why } => {
            return Section::new(Verdict::unknown(unknown_reason(config, why))).with(
                EnrollmentDetail {
                    user: user.to_string(),
                    models: None,
                    marker: None,
                },
            );
        }
        Fact::Probed(enrolled) | Fact::Claimed(enrolled) => enrolled,
    };

    let marker = match &enrolled.marker {
        MarkerDiagnostic::Consistent => Section::new(Verdict::ok()),
        MarkerDiagnostic::Mismatch { marker, store } => Section::new(Verdict::problem(
            "marker disagrees with the database",
        ))
        .with(MarkerCounts {
            marker_models: *marker,
            store_models: *store,
        }),
        MarkerDiagnostic::Unreadable { why } => Section::new(Verdict::unknown(why.clone())),
    };

    Section::new(Verdict::ok_or(
        !enrolled.models.is_empty(),
        "no faces enrolled",
    ))
    .with(EnrollmentDetail {
        user: user.to_string(),
        models: Some(
            enrolled
                .models
                .iter()
                .map(|model| EnrolledModel {
                    id: model.id,
                    label: model.label.clone(),
                })
                .collect(),
        ),
        marker: Some(marker),
    })
}

// ---------------------------------------------------------------------------
// Security and notification posture
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct SecurityDetail {
    disabled: bool,
    /// The individual checks; absent when everything is disabled, because a
    /// `require_ir: true` on a machine that runs no checks would be read as a
    /// promise the config is not keeping. The human report drops them for the
    /// same reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    checks: Option<SecurityChecks>,
}

#[derive(Debug, Clone, Serialize)]
struct SecurityChecks {
    require_ir: bool,
    require_frame_variance: bool,
    require_landmark_liveness: bool,
    min_auth_frames: u32,
}

fn security_section(
    config: &ConfigHealth,
    security: &Fact<SecurityHealth>,
) -> Section<SecurityDetail> {
    let security = match security {
        Fact::Unknown { why } => {
            return Section::new(Verdict::unknown(unknown_reason(config, why)));
        }
        Fact::Probed(security) | Fact::Claimed(security) => security,
    };

    if security.disabled {
        return Section::new(Verdict::problem("all security checks disabled")).with(
            SecurityDetail {
                disabled: true,
                checks: None,
            },
        );
    }

    Section::new(Verdict::ok()).with(SecurityDetail {
        disabled: false,
        checks: Some(SecurityChecks {
            require_ir: security.require_ir,
            require_frame_variance: security.require_frame_variance,
            require_landmark_liveness: security.require_landmark_liveness,
            min_auth_frames: security.min_auth_frames,
        }),
    })
}

#[derive(Debug, Clone, Serialize)]
struct NotificationsDetail {
    mode: NotificationModeWord,
    /// When each notification is sent; absent when the mode is `off` and
    /// nothing is sent at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<NotificationDelivery>,
}

/// Spelled here rather than reusing the config type's serde names: the config
/// schema and this document are separate contracts and must be free to move
/// apart.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum NotificationModeWord {
    Off,
    Terminal,
    Desktop,
    Both,
}

#[derive(Debug, Clone, Serialize)]
struct NotificationDelivery {
    prompt: bool,
    on_success: bool,
    on_failure: bool,
}

fn notifications_section(
    config: &ConfigHealth,
    notifications: &Fact<NotificationHealth>,
) -> Section<NotificationsDetail> {
    use facelock_core::config::NotificationMode;

    let notifications = match notifications {
        Fact::Unknown { why } => {
            return Section::new(Verdict::unknown(unknown_reason(config, why)));
        }
        Fact::Probed(notifications) | Fact::Claimed(notifications) => notifications,
    };

    let mode = match notifications.mode {
        NotificationMode::Off => NotificationModeWord::Off,
        NotificationMode::Terminal => NotificationModeWord::Terminal,
        NotificationMode::Desktop => NotificationModeWord::Desktop,
        NotificationMode::Both => NotificationModeWord::Both,
    };

    // Never a finding: how loudly a machine announces itself is a preference,
    // and the human report renders no verdict line here either.
    Section::new(Verdict::ok()).with(NotificationsDetail {
        mode,
        delivery: (notifications.mode != NotificationMode::Off).then_some(NotificationDelivery {
            prompt: notifications.notify_prompt,
            on_success: notifications.notify_on_success,
            on_failure: notifications.notify_on_failure,
        }),
    })
}

// ---------------------------------------------------------------------------
// PAM wiring
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct PamDetail {
    /// The primary path, whether or not the module is there.
    module_path: String,
    /// Where the module was actually found; absent when it was found nowhere,
    /// which the verdict already says.
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_at: Option<String>,
    /// What the `/etc/pam.d` scan found, in `pam status --json`'s vocabulary.
    services: Section<PamServices>,
}

#[derive(Debug, Clone, Serialize)]
struct PamServices {
    /// Every service that carries the facelock line. Trustworthy even when
    /// the surrounding verdict is `unknown`: what was found *was* found, and
    /// the verdict is about what the scan could not reach.
    configured: Vec<PamServiceRow>,
    /// Every place the scan could not get an answer from.
    not_checked: Vec<NotCheckedRow>,
}

/// One configured service, shaped like a `pam status --json` service object so
/// a consumer can read either document with one code path.
///
/// `backup` is deliberately not here: `facelock status` never looks for a
/// `.facelock-backup`, and emitting a `null` for a file nobody stat-ed would be
/// a claim rather than a field.
#[derive(Debug, Clone, Serialize)]
struct PamServiceRow {
    service: String,
    path: String,
    action: PamRowAction,
    /// The vendor file this local copy hides. Absent — not `null` — when
    /// nothing is shadowed, which is the rule `pam status --json` already set.
    #[serde(skip_serializing_if = "Option::is_none")]
    shadows: Option<String>,
}

/// The one `action` word a `status` row can carry.
///
/// The vocabulary is `pam status --json`'s and may grow there; here only
/// `present` is reachable, because a service that does not carry the line is
/// not in `configured` at all. Typed rather than a literal so adding a second
/// reachable word is a deliberate edit.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum PamRowAction {
    Present,
}

#[derive(Debug, Clone, Serialize)]
struct NotCheckedRow {
    path: String,
    /// The listing or read failure. A diagnostic.
    error: String,
}

fn pam_section(pam: &PamWiring) -> Section<PamDetail> {
    let configured: Vec<PamServiceRow> = pam.configured.iter().map(PamServiceRow::from).collect();
    let not_checked: Vec<NotCheckedRow> = pam.not_checked.iter().map(NotCheckedRow::from).collect();

    // An unread directory or an unreadable service file makes the list
    // incomplete, whether or not anything was found — so the fact is
    // `unknown`, and "nothing is configured" is only ever said about a scan
    // that saw everywhere it meant to.
    let services = Section::new(if not_checked.is_empty() {
        Verdict::ok()
    } else {
        Verdict::unknown("some directories or service files could not be read")
    })
    .with(PamServices {
        configured,
        not_checked,
    });

    Section::new(Verdict::ok_or(
        pam.installed_at.is_some(),
        "PAM module not installed",
    ))
    .with(PamDetail {
        module_path: pam.module_path.clone(),
        installed_at: pam.installed_at.clone(),
        services,
    })
}

impl From<&ConfiguredService> for PamServiceRow {
    fn from(service: &ConfiguredService) -> Self {
        PamServiceRow {
            service: service.service.clone(),
            path: service.path.clone(),
            action: PamRowAction::Present,
            shadows: service.shadows.clone(),
        }
    }
}

impl From<&NotChecked> for NotCheckedRow {
    fn from(not_checked: &NotChecked) -> Self {
        NotCheckedRow {
            path: not_checked.path.clone(),
            error: not_checked.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::{Value, json};

    use crate::commands::status::{render as render_report, tests::healthy};
    use crate::commands::status_json::render as render_json;
    use crate::health::{ConfigHealth, ModelSummary, PamWiring};
    use crate::message::{Message, StatusMessage};
    use crate::resolved::{EpStatus, ProbedFile};

    fn document(health: &Health) -> Value {
        let rendered = render_json(health).expect("the document serializes");
        serde_json::from_str(&rendered).expect("the document is JSON")
    }

    /// What a translated build puts in a `why`.
    ///
    /// The fixture below used to say `"config not available"`, which is the
    /// string [`unknown_reason`] *produces* — so the substitution and a bare
    /// pass-through were indistinguishable, and replacing the function body
    /// with `why.to_string()` left the whole suite green. In the C locale the
    /// catalog renders that same literal, so no realistic English `why` can
    /// tell the two apart; only a `why` that differs from the output can. This
    /// stands in for the French one a `LC_MESSAGES=fr_FR` machine would carry.
    const CATALOG_SENTINEL: &str = "TEXTE DU CATALOGUE TRADUIT";

    /// The shape `Health::probe` returns when the config file did not parse:
    /// every config-dependent fact unknown, PAM still answering.
    ///
    /// The `why` is [`CATALOG_SENTINEL`] because that is what `Health::probe`
    /// really puts there — `health::config_not_available()` output, which is
    /// catalog text and is only English by accident of locale.
    fn all_unknown() -> Health {
        let why = CATALOG_SENTINEL;
        let mut health = healthy();
        health.config = ConfigHealth {
            path: "/etc/facelock/config.toml".into(),
            outcome: ConfigOutcome::Invalid {
                error: "expected `]` at line 1".into(),
            },
        };
        health.daemon = Fact::unknown(why);
        health.oneshot_fallback = Fact::unknown(why);
        health.camera = Fact::unknown(why);
        health.models = Fact::unknown(why);
        health.execution_provider = Fact::unknown(why);
        health.encryption = Fact::unknown(why);
        health.enrolled = Fact::unknown(why);
        health.security = Fact::unknown(why);
        health.notifications = Fact::unknown(why);
        health.pam = PamWiring {
            module_path: "/lib/security/pam_facelock.so".into(),
            installed_at: None,
            configured: Vec::new(),
            not_checked: Vec::new(),
        };
        health
    }

    /// Every section a finding, one way or another.
    fn degraded() -> Health {
        let mut health = healthy();
        health.config.outcome = ConfigOutcome::NotFound;
        health.daemon = Fact::probed(DaemonPing::NotResponding {
            error: "D-Bus Ping call failed".into(),
        });
        health.oneshot_fallback = Fact::probed(OneshotFallback {
            auth_bin: "/usr/bin/facelock".into(),
            auth_bin_present: true,
            models_present: false,
            database_present: false,
        });
        health.camera = Fact::probed(CameraHealth::Configured {
            path: "/dev/video9".into(),
            present: false,
            interrogated: None,
        });
        health.models = Fact::probed(ModelFiles {
            dir: "/usr/share/facelock/models".into(),
            dir_present: false,
            detector: ProbedFile {
                path: "/usr/share/facelock/models/det.onnx".into(),
                present: false,
            },
            embedder: ProbedFile {
                path: "/usr/share/facelock/models/emb.onnx".into(),
                present: false,
            },
        });
        health.execution_provider = Fact::probed(ExecutionProviderFact {
            configured: "cuda".into(),
            status: EpStatus::NotBuiltIn,
        });
        health.encryption = Fact::probed(EncryptionHealth {
            method: facelock_core::config::EncryptionMethod::None,
            material: KeyMaterial::None,
            sealed_counts: None,
        });
        health.enrolled = Fact::probed(EnrollmentHealth {
            models: Vec::new(),
            marker: MarkerDiagnostic::Mismatch {
                marker: 3,
                store: 0,
            },
        });
        health.security = Fact::claimed(SecurityHealth {
            disabled: true,
            require_ir: true,
            require_frame_variance: true,
            require_landmark_liveness: true,
            min_auth_frames: 3,
        });
        health.notifications = Fact::claimed(NotificationHealth {
            mode: facelock_core::config::NotificationMode::Off,
            notify_prompt: false,
            notify_on_success: false,
            notify_on_failure: false,
        });
        health.pam = PamWiring {
            module_path: "/lib/security/pam_facelock.so".into(),
            installed_at: None,
            configured: Vec::new(),
            not_checked: vec![NotChecked {
                path: "/usr/lib/pam.d".into(),
                error: "Permission denied (os error 13)".into(),
            }],
        };
        health
    }

    /// The states that are neither healthy nor plainly broken: a device that
    /// will not answer, a runtime that will not load, a marker that will not
    /// read, a scan that is missing a corner.
    fn undetermined() -> Health {
        let mut health = healthy();
        health.camera = Fact::probed(CameraHealth::Configured {
            path: "/dev/video2".into(),
            present: true,
            interrogated: None,
        });
        health.execution_provider = Fact::probed(ExecutionProviderFact {
            configured: "cuda".into(),
            status: EpStatus::Unqueryable("no dylib".into()),
        });
        health.encryption = Fact::probed(EncryptionHealth {
            method: facelock_core::config::EncryptionMethod::Keyfile,
            material: KeyMaterial::Keyfile {
                key_path: "/etc/facelock/aes.key".into(),
                present: false,
            },
            sealed_counts: None,
        });
        health.enrolled = Fact::probed(EnrollmentHealth {
            models: vec![ModelSummary {
                id: 1,
                label: "front".into(),
                unbound: false,
            }],
            marker: MarkerDiagnostic::Unreadable {
                why: "malformed marker".into(),
            },
        });
        health.pam.not_checked = vec![NotChecked {
            path: "/etc/pam.d/sshd".into(),
            error: "Permission denied (os error 13)".into(),
        }];
        health
    }

    /// Auto-detection that found nothing, and a machine deliberately running
    /// without a daemon.
    fn oneshot_and_autodetect() -> Health {
        let mut health = healthy();
        health.daemon = Fact::claimed(DaemonPing::NotConfigured);
        health.camera = Fact::probed(CameraHealth::AutoDetect { resolved: None });
        health.enrolled = Fact::unknown("database not accessible: disk I/O error");
        health
    }

    fn every_shape() -> Vec<(&'static str, Health)> {
        vec![
            ("healthy", healthy()),
            ("all_unknown", all_unknown()),
            ("degraded", degraded()),
            ("undetermined", undetermined()),
            ("oneshot_and_autodetect", oneshot_and_autodetect()),
        ]
    }

    // -----------------------------------------------------------------------
    // The payload, pinned against the fixture
    // -----------------------------------------------------------------------

    /// The whole healthy document, field for field — the counterpart to
    /// `status.rs`'s `healthy_report_renders_byte_exact`, and the document
    /// `docs/contracts.md` prints.
    ///
    /// Compared as parsed values rather than as bytes on purpose: key order is
    /// documented *not* to be part of the contract, so pinning the bytes would
    /// pin something this promises not to.
    #[test]
    fn the_healthy_document_is_pinned() {
        let expected = json!({
            "config": {
                "state": "ok",
                "path": "/etc/facelock/config.toml",
                "outcome": "valid",
                "device": { "selection": "configured", "path": "/dev/video2" }
            },
            "daemon": {
                "state": "ok",
                "bus_name": "org.facelock.Daemon",
                "reachability": "responding"
            },
            "oneshot_fallback": {
                "state": "ok",
                "auth_bin": "/usr/bin/facelock",
                "binary_present": true,
                "models_present": true,
                "database_present": true
            },
            "camera": {
                "state": "ok",
                "selection": "configured",
                "configured_path": "/dev/video2",
                "present": true,
                "device": {
                    "state": "ok",
                    "path": "/dev/video2",
                    "name": "Integrated IR Camera",
                    "ir": true,
                    "quirks": []
                }
            },
            "models": {
                "state": "ok",
                "dir": "/usr/share/facelock/models",
                "dir_present": true,
                "files": [
                    { "purpose": "detector", "path": "/usr/share/facelock/models/det.onnx", "present": true },
                    { "purpose": "embedder", "path": "/usr/share/facelock/models/emb.onnx", "present": true }
                ]
            },
            "execution_provider": {
                "state": "ok",
                "configured": "cpu",
                "availability": "available"
            },
            "encryption": {
                "state": "ok",
                "key": {
                    "method": "tpm",
                    "sealed_key_path": "/etc/facelock/sealed.key",
                    "sealed_key_present": true,
                    "tpm_device_path": "/dev/tpmrm0",
                    "tpm_device_present": true
                },
                "embeddings": { "state": "ok", "encrypted": 2, "plaintext": 0 }
            },
            "enrollment": {
                "state": "ok",
                "user": "alice",
                "models": [
                    { "id": 1, "label": "front" },
                    { "id": 2, "label": "side" }
                ],
                "marker": { "state": "ok" }
            },
            "security": {
                "state": "ok",
                "disabled": false,
                "checks": {
                    "require_ir": true,
                    "require_frame_variance": true,
                    "require_landmark_liveness": false,
                    "min_auth_frames": 3
                }
            },
            "notifications": {
                "state": "ok",
                "mode": "both",
                "delivery": { "prompt": true, "on_success": true, "on_failure": false }
            },
            "pam": {
                "state": "ok",
                "module_path": "/lib/security/pam_facelock.so",
                "installed_at": "/lib/security/pam_facelock.so",
                "services": {
                    "state": "ok",
                    "configured": [
                        { "service": "sudo", "path": "/etc/pam.d/sudo", "action": "present" },
                        {
                            "service": "polkit-1",
                            "path": "/etc/pam.d/polkit-1",
                            "action": "present",
                            "shadows": "/usr/lib/pam.d/polkit-1"
                        }
                    ],
                    "not_checked": []
                }
            }
        });
        assert_eq!(document(&healthy()), expected);
    }

    /// The document `docs/contracts.md` prints is the document this build
    /// emits. A schema that drifts from the section that publishes it is worse
    /// than an undocumented one, because a consumer wrote code against the
    /// prose.
    #[test]
    fn the_documented_example_is_the_document_this_build_emits() {
        const CONTRACTS: &str = include_str!("../../../../docs/contracts.md");

        let section = CONTRACTS
            .split("### facelock status Semantics")
            .nth(1)
            .expect("docs/contracts.md has a `### facelock status Semantics` section");
        let block = section
            .split("```json\n")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .expect("that section prints a ```json example document");

        let documented: Value =
            serde_json::from_str(block).expect("the documented example must be valid JSON");
        assert_eq!(
            documented,
            document(&healthy()),
            "the example in docs/contracts.md no longer matches the emitted document"
        );
    }

    // -----------------------------------------------------------------------
    // One fixture, two renderers
    // -----------------------------------------------------------------------

    /// The label that opens each section of the human report, in render order,
    /// beside the document key that answers for it.
    ///
    /// Read from the catalog rather than written as English literals, so this
    /// keeps holding under a locale with a `.mo` installed.
    fn sections() -> Vec<(&'static str, String)> {
        vec![
            ("config", StatusMessage::StatusLabelConfigFile.localized()),
            ("daemon", StatusMessage::StatusLabelDaemon.localized()),
            (
                "oneshot_fallback",
                StatusMessage::StatusLabelOneshotFallback.localized(),
            ),
            ("camera", StatusMessage::StatusLabelCameraDevice.localized()),
            (
                "models",
                StatusMessage::StatusLabelModelDirectory.localized(),
            ),
            (
                "execution_provider",
                StatusMessage::StatusLabelExecutionProvider.localized(),
            ),
            (
                "encryption",
                StatusMessage::StatusLabelEncryption.localized(),
            ),
            (
                "enrollment",
                StatusMessage::StatusLabelEnrolledFaces.localized(),
            ),
            ("security", StatusMessage::StatusLabelSecurity.localized()),
            (
                "notifications",
                StatusMessage::StatusLabelNotifications.localized(),
            ),
            ("pam", StatusMessage::StatusLabelPamModule.localized()),
        ]
    }

    /// The document's top-level keys, **in the order they were emitted**.
    ///
    /// Read off the serialized bytes rather than a parsed `Value`, whose map is
    /// a `BTreeMap` and hands back alphabetical order — which would make a
    /// swapped pair in [`sections`] compare equal to itself, since sorting the
    /// same eleven strings cannot tell two orderings apart.
    ///
    /// A key is a string at depth 1, which is unambiguous here because every
    /// top-level value is an object — the caller asserts that premise. String
    /// *contents* are consumed whatever the depth: a `toml` parse error reads
    /// ``expected `]` at line 1``, and a bracket inside a value would otherwise
    /// move the depth counter and desynchronize the whole scan.
    fn top_level_keys(document: &str) -> Vec<String> {
        let mut keys = Vec::new();
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        let mut text = String::new();

        for ch in document.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                    if depth == 1 {
                        keys.push(std::mem::take(&mut text));
                    } else {
                        text.clear();
                    }
                } else {
                    text.push(ch);
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        keys
    }

    /// The human report, split into `(label, verdict lines)` by its own
    /// indentation: two spaces opens a section, four spaces and a marker is a
    /// verdict, four spaces and a dash is a detail.
    fn human_sections(report: &str) -> Vec<(String, Vec<String>)> {
        let mut sections: Vec<(String, Vec<String>)> = Vec::new();
        for line in report.lines() {
            match line.strip_prefix("  ") {
                Some(rest) if !rest.starts_with(' ') => {
                    let label = rest.split(':').next().unwrap_or(rest);
                    sections.push((label.to_string(), Vec::new()));
                }
                _ => {
                    if let Some(rest) = line.strip_prefix("    ")
                        && (rest.starts_with("[ok] ") || rest.starts_with("[!!] "))
                        && let Some((_, verdicts)) = sections.last_mut()
                    {
                        verdicts.push(rest.to_string());
                    }
                }
            }
        }
        sections
    }

    /// The `cannot determine: ` prefix, from the catalog, so an `[!!]` line
    /// that means *unknown* is told apart from one that means *problem*
    /// without matching English.
    fn undetermined_prefix() -> String {
        StatusMessage::StatusUnknown { why: String::new() }.localized()
    }

    /// **The agreement.** One `Health`, two renderers, and every section
    /// reaching both with the same verdict.
    ///
    /// This is what makes the JSON document trustworthy: it is not a second
    /// implementation that happens to look right, it is the same facts,
    /// checked against the report a person reads. A section added to one
    /// renderer and forgotten in the other fails here, and so does a verdict
    /// that says `[!!]` in prose and `ok` on the wire.
    #[test]
    fn the_two_renderers_agree_about_every_section() {
        let undetermined = undetermined_prefix();

        for (name, health) in every_shape() {
            let report = render_report(&health);
            let serialized = render_json(&health).expect("the document serializes");
            let rendered: Value = serde_json::from_str(&serialized).expect("the document is JSON");
            let object = rendered.as_object().expect("the document is an object");
            let human = human_sections(&report);
            let expected = sections();

            // The mapping's `key` half is otherwise a hand-written claim that
            // nothing checks: swapping two keys whose sections happen to share
            // a verdict in every fixture would pass every assertion below.
            // `StatusReport`'s field order is the independent authority, and
            // serde emits struct fields in declaration order.
            assert!(
                object.values().all(Value::is_object),
                "{name}: every top-level value must be an object — `top_level_keys` \
                 reads keys by depth and relies on it"
            );
            assert_eq!(
                top_level_keys(&serialized),
                expected.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
                "{name}: the document's keys, in emission order, must be the keys \
                 `sections()` maps — a pair swapped there answers for the wrong \
                 section"
            );

            assert_eq!(
                human.len(),
                expected.len(),
                "{name}: the report renders {} sections, {} are mapped:\n{report}",
                human.len(),
                expected.len()
            );
            assert_eq!(
                object.len(),
                expected.len(),
                "{name}: the document carries {} keys, {} are mapped: {:?}",
                object.len(),
                expected.len(),
                object.keys().collect::<Vec<_>>()
            );

            for ((key, label), (rendered_label, verdicts)) in expected.iter().zip(&human) {
                assert_eq!(
                    label, rendered_label,
                    "{name}: section order differs between the mapping and the report"
                );
                let section = object
                    .get(*key)
                    .unwrap_or_else(|| panic!("{name}: the document has no `{key}` section"));
                assert!(
                    verdicts.len() <= 1,
                    "{name}: `{label}` renders {} verdict lines; one section, one verdict",
                    verdicts.len()
                );

                let state = section["state"].as_str().unwrap_or_default();
                let want = match verdicts.first() {
                    // No verdict line at all is the report's way of saying
                    // "nothing to report" — security's knobs, notifications'
                    // modes. Anything else there would be a finding the
                    // document invented.
                    None => "ok",
                    Some(line) if line.starts_with("[ok] ") => "ok",
                    Some(line) if line.starts_with(&format!("[!!] {undetermined}")) => "unknown",
                    Some(_) => "problem",
                };
                assert_eq!(
                    state, want,
                    "{name}: `{label}` is `{want}` in the report and `{state}` on the wire:\n{report}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Unknown is a value, never a null and never a false
    // -----------------------------------------------------------------------

    /// Walk every object in the document, so a rule holds for the nested facts
    /// as well as the sections.
    fn objects(value: &Value, out: &mut Vec<serde_json::Map<String, Value>>) {
        match value {
            Value::Object(object) => {
                out.push(object.clone());
                for nested in object.values() {
                    objects(nested, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    objects(item, out);
                }
            }
            _ => {}
        }
    }

    /// `null` is how an undetermined fact would sneak back in wearing a value's
    /// clothes. A field that does not apply is **absent**; a fact that could
    /// not be established is `"state": "unknown"` with a reason.
    #[test]
    fn no_field_is_ever_null() {
        for (name, health) in every_shape() {
            let mut found = Vec::new();
            objects(&document(&health), &mut found);
            for object in found {
                for (key, value) in &object {
                    assert!(
                        !value.is_null(),
                        "{name}: `{key}` is null; absent or `unknown`, never null"
                    );
                }
            }
        }
    }

    /// `reason` exists exactly when there is something to explain, everywhere
    /// in the document — and every `state` is one of the three words.
    #[test]
    fn a_reason_accompanies_every_state_that_is_not_ok() {
        for (name, health) in every_shape() {
            let mut found = Vec::new();
            objects(&document(&health), &mut found);
            for object in found {
                let Some(state) = object.get("state").and_then(Value::as_str) else {
                    assert!(
                        !object.contains_key("reason"),
                        "{name}: a `reason` without a `state`: {object:?}"
                    );
                    continue;
                };
                assert!(
                    matches!(state, "ok" | "problem" | "unknown"),
                    "{name}: `{state}` is not one of the three verdict words"
                );
                assert_eq!(
                    object.contains_key("reason"),
                    state != "ok",
                    "{name}: `{state}` must carry a reason exactly when it is not ok: {object:?}"
                );
            }
        }
    }

    /// **The money case.** A store that could not be read leaves `models`
    /// absent and the section `unknown` — never an empty array, which a
    /// consumer would read as "this user has no faces enrolled".
    #[test]
    fn an_unreadable_store_is_unknown_not_an_empty_enrollment() {
        let mut health = healthy();
        health.enrolled = Fact::unknown("database not accessible: disk I/O error");
        let enrollment = document(&health)["enrollment"].clone();

        assert_eq!(enrollment["state"], "unknown");
        assert_eq!(
            enrollment["reason"],
            "database not accessible: disk I/O error"
        );
        assert!(
            enrollment.get("models").is_none(),
            "an unread store must not render a model list at all: {enrollment}"
        );
        // The user is still known — it came from argv, not from the store.
        assert_eq!(enrollment["user"], "alice");

        // The converse: a *known* empty enrollment is allowed to say so.
        let mut health = healthy();
        health.enrolled = Fact::probed(EnrollmentHealth {
            models: Vec::new(),
            marker: MarkerDiagnostic::Consistent,
        });
        let enrollment = document(&health)["enrollment"].clone();
        assert_eq!(enrollment["state"], "problem");
        assert_eq!(enrollment["reason"], "no faces enrolled");
        assert_eq!(enrollment["models"], json!([]));
    }

    /// A database that could not be counted must not report zero encrypted and
    /// zero plaintext embeddings — which is also a perfectly plausible true
    /// answer, and therefore the worst possible guess.
    #[test]
    fn uncounted_embeddings_are_unknown_not_zero() {
        let mut health = healthy();
        let Fact::Probed(encryption) = &mut health.encryption else {
            panic!("the fixture probes encryption");
        };
        encryption.sealed_counts = None;

        let embeddings = document(&health)["encryption"]["embeddings"].clone();
        assert_eq!(embeddings["state"], "unknown");
        assert_eq!(embeddings["reason"], "embedding counts could not be read");
        assert!(embeddings.get("encrypted").is_none(), "{embeddings}");
        assert!(embeddings.get("plaintext").is_none(), "{embeddings}");
    }

    /// A camera that is present and will not answer is `unknown`; one that was
    /// never there is a `problem`. Collapsing the two is how "the device
    /// refused to open" becomes "you have no IR camera".
    #[test]
    fn a_present_camera_that_will_not_answer_is_unknown_not_absent() {
        let mut health = healthy();
        health.camera = Fact::probed(CameraHealth::Configured {
            path: "/dev/video2".into(),
            present: true,
            interrogated: None,
        });
        let camera = document(&health)["camera"].clone();
        assert_eq!(camera["state"], "ok", "the node is there: {camera}");
        assert_eq!(camera["device"]["state"], "unknown");
        assert!(
            camera["device"].get("ir").is_none(),
            "an uninterrogated camera must not report an IR classification: {camera}"
        );

        let mut health = healthy();
        health.camera = Fact::probed(CameraHealth::Configured {
            path: "/dev/video9".into(),
            present: false,
            interrogated: None,
        });
        let camera = document(&health)["camera"].clone();
        assert_eq!(camera["state"], "problem");
        assert_eq!(camera["present"], false);
        assert_eq!(camera["device"]["state"], "problem");
    }

    /// A scan that could not read everywhere reports `unknown` — with what it
    /// *did* find still listed, because those services really are configured.
    #[test]
    fn an_unread_pam_place_makes_the_service_list_unknown() {
        let complete = document(&healthy())["pam"]["services"].clone();
        assert_eq!(complete["state"], "ok");
        assert_eq!(complete["not_checked"], json!([]));

        let mut health = healthy();
        health.pam.not_checked = vec![NotChecked {
            path: "/usr/lib/pam.d".into(),
            error: "Permission denied (os error 13)".into(),
        }];
        let services = document(&health)["pam"]["services"].clone();
        assert_eq!(services["state"], "unknown");
        assert_eq!(
            services["not_checked"],
            json!([{ "path": "/usr/lib/pam.d", "error": "Permission denied (os error 13)" }])
        );
        assert_eq!(
            services["configured"].as_array().map(Vec::len),
            Some(2),
            "what the scan did find is still reported: {services}"
        );

        // Nothing found and nothing unread is a complete answer, and says so
        // with an empty list rather than with an `unknown`.
        let mut health = healthy();
        health.pam.configured = Vec::new();
        let services = document(&health)["pam"]["services"].clone();
        assert_eq!(services["state"], "ok");
        assert_eq!(services["configured"], json!([]));
    }

    /// A configured service reads the same in this document as it does in
    /// `pam status --json`: same keys, same `action` word, `shadows` present
    /// only when there is a vendor file to shadow.
    #[test]
    fn a_pam_row_is_shaped_like_a_pam_status_row() {
        let services = document(&healthy())["pam"]["services"]["configured"].clone();
        let rows = services.as_array().expect("configured is an array");

        assert_eq!(
            rows[0],
            json!({ "service": "sudo", "path": "/etc/pam.d/sudo", "action": "present" })
        );
        assert!(
            rows[0].get("shadows").is_none(),
            "a row shadowing nothing carries no `shadows` key: {}",
            rows[0]
        );
        assert_eq!(rows[1]["shadows"], "/usr/lib/pam.d/polkit-1");
        assert_eq!(rows[1]["action"], "present");
    }

    // -----------------------------------------------------------------------
    // The document never localizes
    // -----------------------------------------------------------------------

    /// The one `why` facelock authors itself is the one that is translated, so
    /// it is the one the document replaces with a C-locale literal.
    #[test]
    fn a_broken_config_reasons_in_the_c_locale() {
        let rendered = document(&all_unknown());
        let object = rendered.as_object().expect("the document is an object");

        let unknown: Vec<&String> = object
            .iter()
            .filter(|(_, section)| section["state"] == "unknown")
            .map(|(key, _)| key)
            .collect();
        assert_eq!(
            unknown.len(),
            9,
            "all nine config-dependent sections are unknown, got {unknown:?}"
        );
        for key in unknown {
            assert_eq!(
                object[key]["reason"], CONFIG_NOT_AVAILABLE,
                "`{key}`'s reason must be the C-locale literal, not the catalog's"
            );
        }

        // `config` reports the parse failure, and `pam` — which does not need
        // the config — still answers.
        assert_eq!(object["config"]["outcome"], "invalid");
        assert_eq!(object["config"]["error"], "expected `]` at line 1");
        assert_eq!(object["pam"]["state"], "problem");
    }

    /// **The daemon section carries no free-text diagnostic, in any state.**
    ///
    /// `DaemonPing::NotResponding.error` is an `anyhow` chain that
    /// `ipc_client::add_access_denied_hint` localizes before `status` sees it,
    /// so forwarding it would ship catalog output in a payload. The field is
    /// gone; this is what keeps it gone, and it asserts the stronger property —
    /// the transport's string reaches **no** part of the document, not merely
    /// no field called `error`.
    #[test]
    fn the_daemon_section_carries_no_free_text_diagnostic() {
        // `degraded()` is the shape that has one: a probed NotResponding.
        let health = degraded();
        let Fact::Probed(DaemonPing::NotResponding { error }) = &health.daemon else {
            panic!("the degraded fixture must carry a transport error to keep out");
        };
        let transport_error = error.clone();

        let serialized = render_json(&health).expect("the document serializes");
        assert!(
            !serialized.contains(&transport_error),
            "the transport's error text reached the document:\n{serialized}"
        );

        let daemon = document(&health)["daemon"].clone();
        assert_eq!(daemon["state"], "problem");
        assert_eq!(daemon["reason"], "daemon not responding");
        assert_eq!(daemon["reachability"], "not_responding");
        assert!(
            daemon.get("error").is_none(),
            "the daemon section must carry no `error` field: {daemon}"
        );

        // The human report is where that text belongs, and still has it.
        assert!(
            render_report(&health).contains(&transport_error),
            "the hint is written for a human and must keep reaching one"
        );
    }

    /// **The enforcement point.** A `why` that came from the catalog is
    /// replaced, and no fragment of it survives anywhere in the document.
    ///
    /// The whole rendered string is searched rather than the nine `reason`
    /// fields, because the claim is about the payload a script receives, not
    /// about the fields this test happened to think of.
    #[test]
    fn a_translated_why_never_reaches_the_document() {
        let rendered = render_json(&all_unknown()).expect("the document serializes");
        assert!(
            !rendered.contains(CATALOG_SENTINEL),
            "catalog text reached a --json payload:\n{rendered}"
        );

        // ...and what replaced it is the literal, on every one of them.
        let document: Value = serde_json::from_str(&rendered).expect("the document is JSON");
        let object = document.as_object().expect("the document is an object");
        let substituted = object
            .values()
            .filter(|section| section["state"] == "unknown")
            .filter(|section| section["reason"] == CONFIG_NOT_AVAILABLE)
            .count();
        assert_eq!(
            substituted, 9,
            "all nine config-dependent reasons must be the C-locale literal"
        );

        // The human report is the other renderer's problem and is *supposed*
        // to carry the catalog string; asserting that here is what keeps this
        // test about the wire rather than about gettext.
        assert!(
            render_report(&all_unknown()).contains(CATALOG_SENTINEL),
            "the human report renders the localized why verbatim; if it stopped, \
             this test is no longer proving the two renderers differ here"
        );
    }

    /// A probe's own `why` is English by construction and is passed through,
    /// because "database not accessible: disk I/O error" is the sentence worth
    /// having.
    #[test]
    fn a_probe_failure_keeps_its_own_reason() {
        let mut health = healthy();
        health.enrolled = Fact::unknown("error reading models: no such column");
        assert_eq!(
            document(&health)["enrollment"]["reason"],
            "error reading models: no such column"
        );
    }

    /// The premise [`unknown_reason`] rests on: `health.rs` has exactly one
    /// localized `why`, and it is the config-not-available one. A second would
    /// put catalog text in a `--json` payload, silently, on a translated
    /// machine only — so it fails the build here instead.
    /// A source file's **code**: its test module dropped and its comments
    /// stripped.
    ///
    /// Every catalog pin below reads a file that talks about `localized()` at
    /// length, and quoting a call is not making one — this file's own module
    /// header names it four times. The test module goes for the same reason:
    /// the assertions here deliberately read labels from the catalog, to stay
    /// locale-independent. Lines are rejoined with newlines rather than
    /// concatenated, so a needle cannot be assembled out of two adjacent lines.
    ///
    /// **The split is on the last `#[cfg(test)]`, and `must_contain` is why
    /// that matters.** Splitting on the first would truncate at any test-only
    /// item declared above the test module — `commands/pam.rs` has exactly
    /// one, `pub(crate) fn only`, and cutting there would drop every
    /// `Outcome::Unknown` site below it and leave the pins passing on an empty
    /// string. A `!contains(...)` assertion cannot notice that it was handed
    /// nothing, so each caller names a token its own production half must
    /// still carry and truncation fails here instead.
    fn code_of(source: &str, must_contain: &str) -> String {
        let production = match source.rfind("\n#[cfg(test)]\n") {
            Some(at) => &source[..at],
            None => source,
        };
        let code = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code.contains(must_contain),
            "the scanned code no longer contains `{must_contain}`, so this pin is \
             reading a truncated file and would pass on anything"
        );
        code
    }

    /// The source text of every `constructor(…)` payload, from the constructor
    /// to the end of its statement.
    ///
    /// Statement-bounded rather than paren-bounded, which is the honest limit
    /// of it: a `;` inside a closure argument would cut a window short. It is
    /// enough for the edit it guards against — a catalog call written where a
    /// C-locale one belongs, on the next line — and it over-scopes rather than
    /// under-scopes everywhere else, so its errors are false alarms, not false
    /// confidence.
    fn constructed_payloads<'a>(source: &'a str, constructor: &str) -> Vec<&'a str> {
        source
            .match_indices(constructor)
            .map(|(at, _)| {
                let rest = &source[at..];
                &rest[..rest.find(';').unwrap_or(rest.len())]
            })
            .collect()
    }

    #[test]
    fn health_has_exactly_one_localized_why() {
        let health = code_of(include_str!("../health.rs"), "fn config_not_available()");

        assert_eq!(
            health.matches("localized(").count(),
            1,
            "`health.rs` grew a second localized string. Every `Fact::Unknown` \
             reason it produces is forwarded verbatim into `status --json` \
             unless the config failed to parse; a translated one there breaks \
             D2. Either keep it out of a `why`, or teach `unknown_reason` \
             about it."
        );
        assert!(
            health.contains("fn config_not_available()"),
            "the one localized `why` is `config_not_available`; if it was \
             renamed, `unknown_reason`'s reasoning needs re-reading"
        );
    }

    /// The other file that authors a `reason`, held to the same rule.
    ///
    /// `enrollment.marker.reason` is `MarkerState::Unreadable`'s payload, built
    /// in `commands/enrollment_marker.rs` and forwarded here verbatim —
    /// `unknown_reason` never sees it, so nothing else would stop a
    /// `localized()` added there from reaching a `--json` payload.
    ///
    /// **The exposure is every `String` copied into the document, not just the
    /// two files that author a `reason`.** An earlier version of this comment
    /// claimed those two were the whole of it, which was wrong twice over: it
    /// reasoned about `reason` producers and never considered `error` ones, and
    /// `daemon.error` used to forward an `anyhow` context that `ipc_client`
    /// localizes on purpose. That field is gone (see [`daemon_section`]); the
    /// generalization is what stops the next one.
    ///
    /// What survives that audit, and why:
    ///
    /// - `config.error` — a `toml` parse message, and
    ///   `execution_provider.error` — the ONNX Runtime's own load failure.
    ///   Neither crate has a catalog.
    /// - `pam.services.not_checked[].error` — an `io::Error` Display, or one of
    ///   G3's C-locale identifiers. G3 pins the *values of those three
    ///   constants*, which is not the same as forbidding a fourth producer
    ///   written with the catalog, so
    ///   [`the_pam_scan_never_puts_catalog_text_in_an_error`] adds that half.
    /// - `enrollment.reason`, `enrollment.marker.reason` — `facelock-store` and
    ///   `facelock-core` errors, which cannot localize: the seam lives in
    ///   `facelock-cli` (`pam-facelock` keeps its own copy), and gettext needs
    ///   `libc`, which is in neither crate's dependency contract.
    ///
    /// A `String` reaching the wire from a **`facelock-cli`** module that is not
    /// pinned here is the shape to distrust; `daemon` was one.
    #[test]
    fn the_marker_reason_is_not_authored_through_the_catalog() {
        let marker = code_of(
            include_str!("enrollment_marker.rs"),
            "MarkerState::Unreadable",
        );

        for forbidden in ["localized(", "translate(", "crate::message"] {
            assert!(
                !marker.contains(forbidden),
                "`{forbidden}` in enrollment_marker.rs: `MarkerState::Unreadable`'s \
                 payload is rendered straight into `status --json`'s \
                 `enrollment.marker.reason`, so catalog text there ships in a \
                 payload"
            );
        }
    }

    /// `commands/pam.rs` is the one module on this document's path that writes
    /// to **both** sinks, so it gets its own pin.
    ///
    /// `NotChecked.error` — which becomes `pam.services.not_checked[].error` —
    /// is an `Outcome::Unknown` or `DirState::Unreadable` payload, and those are
    /// built two lines from a `sink.error(&PamMessage::…)` that localizes by
    /// design. Writing `Outcome::Unknown(PamMessage::PamStatusUnknown{..}
    /// .localized())` there is a plausible edit, and none of the other three
    /// pins looks at that file: they scan modules that never touch the catalog
    /// at all, whereas this one legitimately calls it eighteen times, so a
    /// whole-file rule would be wrong. What is checkable is narrower and is the
    /// thing that matters — the *payloads of those two constructors* must not
    /// come from the catalog.
    #[test]
    fn the_pam_scan_never_puts_catalog_text_in_an_error() {
        let pam = code_of(include_str!("pam.rs"), "fn configured_scan");

        for constructor in ["Outcome::Unknown(", "DirState::Unreadable("] {
            let payloads = constructed_payloads(&pam, constructor);
            assert!(
                !payloads.is_empty(),
                "no `{constructor}` site found — the constructor was renamed and \
                 this pin is now checking nothing"
            );
            for payload in payloads {
                for forbidden in ["localized", "translate("] {
                    assert!(
                        !payload.contains(forbidden),
                        "`{forbidden}` in a `{constructor}` payload: that string \
                         becomes `status --json`'s \
                         `pam.services.not_checked[].error`, which must stay \
                         C-locale. The human line beside it is where the \
                         localized text belongs.\n{payload}"
                    );
                }
            }
        }
    }

    /// Nothing in the machine renderer reaches for the catalog. The seam's
    /// rule is "machine output does not come through here", and the cheapest
    /// way to keep it is to have no call site at all.
    #[test]
    fn the_machine_renderer_calls_no_catalog() {
        let renderer = code_of(include_str!("status_json.rs"), "fn unknown_reason");

        for forbidden in ["localized", "translate(", "crate::message"] {
            assert!(
                !renderer.contains(forbidden),
                "`{forbidden}` in the machine renderer: a --json document must \
                 not consult the message catalog"
            );
        }
    }
}
