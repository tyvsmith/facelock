//! `facelock status` — a renderer over [`Health`] (map §5).
//!
//! The command is two halves with nothing hidden between them:
//! [`Health::probe`] gathers every fact (root, real system), and [`render`]
//! turns a `Health` into the report, pure and byte-deterministic. Every
//! judgment this file used to make inline — "is that an error or an empty
//! list?" — now arrives pre-decided as a [`Fact`], and the renderer's one
//! obligation is N4's: an unknown fact renders as *unknown*, never as a
//! guessed value. "The daemon is unreachable" must never read as "no faces
//! enrolled".
//!
//! Output contract: the English fallback of every message here is
//! byte-identical to the historical inline strings (D10), and
//! `healthy_report_renders_byte_exact` pins the whole report.
//!
//! `--json` renders the same `Health` through a second, independent renderer
//! ([`crate::commands::status_json`]), which consults no catalog — machine
//! output is the seam's documented exclusion (D2). The two are held together
//! by `the_two_renderers_agree_about_every_section` over there, not by shared
//! code: a section that reaches one renderer and not the other fails the
//! build.

use crate::backend::DaemonPing;
use crate::health::{
    CameraHealth, ConfigOutcome, EncryptionHealth, EnrollmentHealth, Health, InterrogatedCamera,
    KeyMaterial, MarkerDiagnostic, NotificationHealth, OneshotFallback, PamWiring, SecurityHealth,
};
use crate::ipc_client;
use crate::message::{Message, StatusMessage};
use crate::resolved::{EpStatus, ExecutionProviderFact, Fact, ModelFiles};

pub fn run(loaded: crate::resolved::ConfigLoad, json: bool) -> anyhow::Result<()> {
    // DEC-6/N4: everything this report reads is root's — the daemon's
    // root-only methods, the 0600 database, other users' markers. Check
    // before the first line of output (C6). `--json` does not soften it:
    // the document is built from the same root-only probes.
    ipc_client::require_root("sudo facelock status")?;

    let health = Health::probe(&loaded);
    if json {
        // Through the payload sink, so `--quiet` reaches it without this
        // command knowing the flag exists. The human report still prints
        // directly; #140 tracks moving it onto the seam.
        crate::message::payload(&crate::commands::status_json::render(&health)?);
    } else {
        print!("{}", render(&health));
    }
    Ok(())
}

/// The report, as a string. Pure: no probing, no I/O, no branching on the
/// live system — a synthetic [`Health`] renders in a unit test with zero
/// root, camera, or bus.
pub fn render(health: &Health) -> String {
    let mut out = String::new();
    out.push_str(&StatusMessage::StatusHeader.localized());
    out.push_str("\n\n");

    render_config(&mut out, health);
    render_daemon(&mut out, &health.daemon);
    render_fallback(&mut out, &health.oneshot_fallback);
    render_camera(&mut out, &health.camera);
    render_models(&mut out, &health.models);
    render_execution_provider(&mut out, &health.execution_provider);
    render_encryption(&mut out, &health.encryption);
    render_enrolled(&mut out, &health.user, &health.enrolled);
    render_security(&mut out, &health.security);
    render_notifications(&mut out, &health.notifications);
    render_pam(&mut out, &health.pam);
    out
}

// ---------------------------------------------------------------------------
// Report skeleton. The prefixes are structure, not prose: `[ok]`/`[!!]` are
// grepped by the container tests and stay untranslated by construction.
// ---------------------------------------------------------------------------

fn item(out: &mut String, label: &StatusMessage, value: &str) {
    let label = label.localized();
    if value.is_empty() {
        out.push_str(&format!("  {label}:\n"));
    } else {
        out.push_str(&format!("  {label}: {value}\n"));
    }
}

fn result(out: &mut String, ok: bool, msg: &StatusMessage) {
    let indicator = if ok { "[ok]" } else { "[!!]" };
    out.push_str(&format!("    {indicator} {}\n", msg.localized()));
}

fn detail(out: &mut String, key: &str, value: &str) {
    out.push_str(&format!("    - {key}: {value}\n"));
}

/// The N4 rule in one place: a fact that could not be determined renders as
/// exactly that.
fn unknown(out: &mut String, why: &str) {
    result(
        out,
        false,
        &StatusMessage::StatusUnknown { why: why.into() },
    );
}

fn yes_no(value: bool) -> String {
    if value {
        StatusMessage::StatusYes.localized()
    } else {
        StatusMessage::StatusNo.localized()
    }
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn render_config(out: &mut String, health: &Health) {
    item(
        out,
        &StatusMessage::StatusLabelConfigFile,
        &health.config.path,
    );
    match &health.config.outcome {
        ConfigOutcome::Valid { device_path } => {
            result(out, true, &StatusMessage::StatusConfigValid);
            detail(
                out,
                "device.path",
                device_path.as_deref().unwrap_or("(auto-detect)"),
            );
        }
        ConfigOutcome::NotFound => result(out, false, &StatusMessage::StatusConfigNotFound),
        ConfigOutcome::Invalid { error } => result(
            out,
            false,
            &StatusMessage::StatusConfigInvalid {
                error: error.clone(),
            },
        ),
    }
}

fn render_daemon(out: &mut String, daemon: &Fact<DaemonPing>) {
    // The bus name is a static fact about the design, valid even when the
    // config (and so the mode) is unknown.
    item(
        out,
        &StatusMessage::StatusLabelDaemon,
        "org.facelock.Daemon (D-Bus system bus)",
    );
    match daemon {
        Fact::Unknown { why } => unknown(out, why),
        Fact::Probed(ping) | Fact::Claimed(ping) => match ping {
            DaemonPing::NotConfigured => result(out, true, &StatusMessage::StatusDaemonOneshot),
            DaemonPing::Responding => result(out, true, &StatusMessage::StatusDaemonResponding),
            DaemonPing::NotResponding { error } => result(
                out,
                false,
                &StatusMessage::StatusDaemonNotResponding {
                    error: error.clone(),
                },
            ),
        },
    }
}

fn render_fallback(out: &mut String, fallback: &Fact<OneshotFallback>) {
    match fallback {
        Fact::Unknown { why } => {
            item(out, &StatusMessage::StatusLabelOneshotFallback, "");
            unknown(out, why);
        }
        Fact::Probed(fallback) | Fact::Claimed(fallback) => {
            item(
                out,
                &StatusMessage::StatusLabelOneshotFallback,
                &fallback.auth_bin,
            );
            if fallback.prerequisites_present() {
                result(out, true, &StatusMessage::StatusFallbackUsable);
            } else {
                let missing = StatusMessage::StatusMissing.localized();
                for (key, present) in [
                    ("binary", fallback.auth_bin_present),
                    ("models", fallback.models_present),
                    ("database", fallback.database_present),
                ] {
                    if !present {
                        detail(out, key, &missing);
                    }
                }
                result(out, false, &StatusMessage::StatusFallbackNotUsable);
            }
        }
    }
}

fn render_camera(out: &mut String, camera: &Fact<CameraHealth>) {
    match camera {
        Fact::Unknown { why } => {
            item(out, &StatusMessage::StatusLabelCameraDevice, "");
            unknown(out, why);
        }
        Fact::Probed(camera) | Fact::Claimed(camera) => match camera {
            CameraHealth::Configured {
                path,
                present,
                interrogated,
            } => {
                item(out, &StatusMessage::StatusLabelCameraDevice, path);
                if *present {
                    result(out, true, &StatusMessage::StatusCameraDeviceExists);
                } else {
                    result(out, false, &StatusMessage::StatusCameraDeviceNotFound);
                }
                if let Some(camera) = interrogated {
                    camera_details(out, camera);
                }
            }
            CameraHealth::AutoDetect { resolved } => {
                item(
                    out,
                    &StatusMessage::StatusLabelCameraDevice,
                    "(auto-detect)",
                );
                result(out, true, &StatusMessage::StatusCameraAutoDetect);
                if let Some(camera) = resolved {
                    detail(out, "device", &format!("{} ({})", camera.path, camera.name));
                    camera_details(out, camera);
                }
            }
        },
    }
}

/// The D8 payoff: "why did this camera behave oddly" is answerable from
/// `status` — the interrogated IR classification and any quirks-DB entries
/// that will shape how the device is opened.
fn camera_details(out: &mut String, camera: &InterrogatedCamera) {
    detail(out, "ir", &yes_no(camera.is_ir));
    if !camera.applied_quirks.is_empty() {
        detail(out, "quirks", &camera.applied_quirks.join(", "));
    }
}

fn render_models(out: &mut String, models: &Fact<ModelFiles>) {
    match models {
        Fact::Unknown { why } => {
            item(out, &StatusMessage::StatusLabelModelDirectory, "");
            unknown(out, why);
        }
        Fact::Probed(models) | Fact::Claimed(models) => {
            item(
                out,
                &StatusMessage::StatusLabelModelDirectory,
                &models.dir.display().to_string(),
            );
            if !models.dir_present {
                result(out, false, &StatusMessage::StatusModelsDirNotFound);
                return;
            }
            for (purpose, file) in [
                ("detector", &models.detector),
                ("embedder", &models.embedder),
            ] {
                let filename = file.path.file_name().unwrap_or_default().to_string_lossy();
                let presence = if file.present {
                    StatusMessage::StatusPresent
                } else {
                    StatusMessage::StatusMissing
                };
                detail(
                    out,
                    &format!("{purpose} ({filename})"),
                    &presence.localized(),
                );
            }
            if models.all_present() {
                result(out, true, &StatusMessage::StatusModelsAllPresent);
            } else {
                result(out, false, &StatusMessage::StatusModelsSomeMissing);
            }
        }
    }
}

fn render_execution_provider(out: &mut String, ep: &Fact<ExecutionProviderFact>) {
    match ep {
        Fact::Unknown { why } => {
            item(out, &StatusMessage::StatusLabelExecutionProvider, "");
            unknown(out, why);
        }
        Fact::Probed(ep) | Fact::Claimed(ep) => {
            let label = match ep.configured.as_str() {
                "cpu" => "CPU",
                "cuda" => "CUDA (NVIDIA GPU)",
                "rocm" => "ROCm (AMD GPU)",
                "openvino" => "OpenVINO (Intel)",
                other => other,
            };
            item(out, &StatusMessage::StatusLabelExecutionProvider, label);
            match &ep.status {
                EpStatus::Available => result(out, true, &StatusMessage::StatusEpSupported),
                EpStatus::NotBuiltIn => result(out, false, &StatusMessage::StatusEpNotBuiltIn),
                EpStatus::UnknownName => result(out, false, &StatusMessage::StatusEpUnknownName),
                EpStatus::Unqueryable(error) => result(
                    out,
                    false,
                    &StatusMessage::StatusEpUnqueryable {
                        error: error.clone(),
                    },
                ),
            }
        }
    }
}

fn render_encryption(out: &mut String, encryption: &Fact<EncryptionHealth>) {
    match encryption {
        Fact::Unknown { why } => {
            item(out, &StatusMessage::StatusLabelEncryption, "");
            unknown(out, why);
        }
        Fact::Probed(encryption) | Fact::Claimed(encryption) => {
            use facelock_core::config::EncryptionMethod;
            let method = match encryption.method {
                EncryptionMethod::Tpm => "AES-256-GCM (TPM-sealed key)",
                EncryptionMethod::Keyfile => "AES-256-GCM (keyfile)",
                EncryptionMethod::None => "none",
            };
            item(out, &StatusMessage::StatusLabelEncryption, method);
            match &encryption.material {
                KeyMaterial::Tpm {
                    sealed_key_path,
                    sealed_key_present,
                    device_path,
                    device_present,
                } => {
                    if *sealed_key_present && *device_present {
                        result(
                            out,
                            true,
                            &StatusMessage::StatusSealedKey {
                                path: sealed_key_path.clone(),
                            },
                        );
                    } else if !sealed_key_present {
                        result(
                            out,
                            false,
                            &StatusMessage::StatusSealedKeyMissing {
                                path: sealed_key_path.clone(),
                            },
                        );
                    } else {
                        result(
                            out,
                            false,
                            &StatusMessage::StatusTpmDeviceMissing {
                                path: device_path.clone(),
                            },
                        );
                    }
                }
                KeyMaterial::Keyfile { key_path, present } => {
                    if *present {
                        result(
                            out,
                            true,
                            &StatusMessage::StatusKeyFile {
                                path: key_path.clone(),
                            },
                        );
                    } else {
                        result(
                            out,
                            false,
                            &StatusMessage::StatusKeyFileMissing {
                                path: key_path.clone(),
                            },
                        );
                    }
                }
                KeyMaterial::None => {
                    result(out, false, &StatusMessage::StatusPlaintextEmbeddings);
                }
            }
            if let Some((sealed, plaintext)) = encryption.sealed_counts
                && sealed + plaintext > 0
            {
                detail(out, "encrypted", &sealed.to_string());
                detail(out, "plaintext", &plaintext.to_string());
            }
        }
    }
}

fn render_enrolled(out: &mut String, user: &str, enrolled: &Fact<EnrollmentHealth>) {
    item(out, &StatusMessage::StatusLabelEnrolledFaces, user);
    match enrolled {
        // The money case. An unreadable store renders as *cannot determine*
        // — never as "no faces enrolled" (N4).
        Fact::Unknown { why } => unknown(out, why),
        Fact::Probed(enrolled) | Fact::Claimed(enrolled) => {
            if enrolled.models.is_empty() {
                result(out, false, &StatusMessage::StatusNoFacesEnrolled);
            } else {
                result(
                    out,
                    true,
                    &StatusMessage::StatusModelCount {
                        count: enrolled.models.len(),
                    },
                );
                for model in &enrolled.models {
                    detail(out, &format!("#{}", model.id), &model.label);
                }
            }
            match &enrolled.marker {
                MarkerDiagnostic::Consistent => {}
                MarkerDiagnostic::Mismatch { marker, store } => detail(
                    out,
                    "marker",
                    &StatusMessage::StatusMarkerMismatch {
                        marker: *marker,
                        store: *store,
                    }
                    .localized(),
                ),
                MarkerDiagnostic::Unreadable { why } => detail(
                    out,
                    "marker",
                    &StatusMessage::StatusMarkerUnreadable { why: why.clone() }.localized(),
                ),
            }
        }
    }
}

fn render_security(out: &mut String, security: &Fact<SecurityHealth>) {
    item(out, &StatusMessage::StatusLabelSecurity, "");
    match security {
        Fact::Unknown { why } => unknown(out, why),
        Fact::Probed(security) | Fact::Claimed(security) => {
            if security.disabled {
                result(out, false, &StatusMessage::StatusSecurityDisabled);
                return;
            }
            detail(out, "require_ir", &yes_no(security.require_ir));
            detail(
                out,
                "liveness (frame variance)",
                &yes_no(security.require_frame_variance),
            );
            detail(
                out,
                "liveness (landmark movement)",
                &yes_no(security.require_landmark_liveness),
            );
            detail(
                out,
                "min_auth_frames",
                &security.min_auth_frames.to_string(),
            );
        }
    }
}

fn render_notifications(out: &mut String, notifications: &Fact<NotificationHealth>) {
    match notifications {
        Fact::Unknown { why } => {
            item(out, &StatusMessage::StatusLabelNotifications, "");
            unknown(out, why);
        }
        Fact::Probed(notifications) | Fact::Claimed(notifications) => {
            use facelock_core::config::NotificationMode;
            let mode = match notifications.mode {
                NotificationMode::Off => StatusMessage::StatusNotifyOff,
                NotificationMode::Terminal => StatusMessage::StatusNotifyTerminal,
                NotificationMode::Desktop => StatusMessage::StatusNotifyDesktop,
                NotificationMode::Both => StatusMessage::StatusNotifyBoth,
            };
            item(
                out,
                &StatusMessage::StatusLabelNotifications,
                &mode.localized(),
            );
            if notifications.mode != NotificationMode::Off {
                detail(out, "prompt", &yes_no(notifications.notify_prompt));
                detail(out, "on success", &yes_no(notifications.notify_on_success));
                detail(out, "on failure", &yes_no(notifications.notify_on_failure));
            }
        }
    }
}

fn render_pam(out: &mut String, pam: &PamWiring) {
    item(out, &StatusMessage::StatusLabelPamModule, &pam.module_path);
    match &pam.installed_at {
        Some(path) if *path == pam.module_path => {
            result(out, true, &StatusMessage::StatusPamInstalled)
        }
        Some(path) => result(
            out,
            true,
            &StatusMessage::StatusPamInstalledAt { path: path.clone() },
        ),
        None => result(out, false, &StatusMessage::StatusPamNotInstalled),
    }
    // One line, not the whole listing: `facelock pam status --all` is where
    // the per-service detail lives, and this report has ten other sections.
    // What it must never do is render "could not look" as "nothing there" —
    // so the empty case has two spellings, decided by whether anything went
    // unchecked.
    let names: Vec<&str> = pam
        .configured
        .iter()
        .map(|service| service.service.as_str())
        .collect();
    let overrides = pam
        .configured
        .iter()
        .filter(|service| service.shadows.is_some())
        .count();

    let value = match (names.is_empty(), pam.not_checked.is_empty(), overrides) {
        (true, true, _) => StatusMessage::StatusPamNoServices,
        (true, false, _) => StatusMessage::StatusPamNotChecked,
        (false, _, 0) => StatusMessage::StatusPamServices {
            services: names.join(", "),
            count: names.len().to_string(),
        },
        (false, _, overrides) => StatusMessage::StatusPamServicesWithOverrides {
            services: names.join(", "),
            count: names.len().to_string(),
            overrides: overrides.to_string(),
        },
    };
    detail(out, "PAM services", &value.localized());

    // Named one per line rather than joined: each is a path and an errno, and
    // a reader has to be able to act on the one that concerns them.
    for unchecked in &pam.not_checked {
        detail(
            out,
            "not checked",
            &StatusMessage::StatusPamNotCheckedAt {
                path: unchecked.path.clone(),
                error: unchecked.error.clone(),
            }
            .localized(),
        );
    }
}

/// `pub(crate)` so `commands::status_json`'s tests render the *same*
/// [`healthy`](tests::healthy) fixture. One fixture, two renderers: a second
/// copy over there could drift, and the agreement test would then be comparing
/// two different machines.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::path::PathBuf;

    use facelock_core::config::NotificationMode;

    use crate::health::{ConfigHealth, ConfiguredService, ModelSummary, NotChecked};
    use crate::resolved::ProbedFile;

    fn model_files(dir: &str, detector_present: bool, embedder_present: bool) -> ModelFiles {
        let dir = PathBuf::from(dir);
        ModelFiles {
            detector: ProbedFile {
                path: dir.join("det.onnx"),
                present: detector_present,
            },
            embedder: ProbedFile {
                path: dir.join("emb.onnx"),
                present: embedder_present,
            },
            dir_present: true,
            dir,
        }
    }

    /// A fully healthy system, built literally — no root, camera, or bus.
    pub(crate) fn healthy() -> Health {
        Health {
            config: ConfigHealth {
                path: "/etc/facelock/config.toml".into(),
                outcome: ConfigOutcome::Valid {
                    device_path: Some("/dev/video2".into()),
                },
            },
            daemon: Fact::probed(DaemonPing::Responding),
            oneshot_fallback: Fact::probed(OneshotFallback {
                auth_bin: "/usr/bin/facelock".into(),
                auth_bin_present: true,
                models_present: true,
                database_present: true,
            }),
            camera: Fact::probed(CameraHealth::Configured {
                path: "/dev/video2".into(),
                present: true,
                interrogated: Some(InterrogatedCamera {
                    path: "/dev/video2".into(),
                    name: "Integrated IR Camera".into(),
                    is_ir: true,
                    applied_quirks: vec![],
                }),
            }),
            models: Fact::probed(model_files("/usr/share/facelock/models", true, true)),
            execution_provider: Fact::probed(ExecutionProviderFact {
                configured: "cpu".into(),
                status: EpStatus::Available,
            }),
            encryption: Fact::probed(EncryptionHealth {
                method: facelock_core::config::EncryptionMethod::Tpm,
                material: KeyMaterial::Tpm {
                    sealed_key_path: "/etc/facelock/sealed.key".into(),
                    sealed_key_present: true,
                    device_path: "/dev/tpmrm0".into(),
                    device_present: true,
                },
                sealed_counts: Some((2, 0)),
            }),
            user: "alice".into(),
            enrolled: Fact::probed(EnrollmentHealth {
                models: vec![
                    ModelSummary {
                        id: 1,
                        label: "front".into(),
                    },
                    ModelSummary {
                        id: 2,
                        label: "side".into(),
                    },
                ],
                marker: MarkerDiagnostic::Consistent,
            }),
            security: Fact::claimed(SecurityHealth {
                disabled: false,
                require_ir: true,
                require_frame_variance: true,
                require_landmark_liveness: false,
                min_auth_frames: 3,
            }),
            notifications: Fact::claimed(NotificationHealth {
                mode: NotificationMode::Both,
                notify_prompt: true,
                notify_on_success: true,
                notify_on_failure: false,
            }),
            pam: PamWiring {
                module_path: "/lib/security/pam_facelock.so".into(),
                installed_at: Some("/lib/security/pam_facelock.so".into()),
                configured: vec![
                    ConfiguredService {
                        service: "sudo".into(),
                        path: "/etc/pam.d/sudo".into(),
                        shadows: None,
                    },
                    ConfiguredService {
                        service: "polkit-1".into(),
                        path: "/etc/pam.d/polkit-1".into(),
                        shadows: Some("/usr/lib/pam.d/polkit-1".into()),
                    },
                ],
                not_checked: Vec::new(),
            },
        }
    }

    /// The whole healthy report, byte for byte. This is the renderer's
    /// contract with the container tests and the docs — most importantly the
    /// grepped `    [ok] responding` line.
    #[test]
    fn healthy_report_renders_byte_exact() {
        let expected = "\
facelock system status

  Config file: /etc/facelock/config.toml
    [ok] valid
    - device.path: /dev/video2
  Daemon: org.facelock.Daemon (D-Bus system bus)
    [ok] responding
  Oneshot fallback: /usr/bin/facelock
    [ok] prerequisites present (binary, models and database in place for daemon-less auth)
  Camera device: /dev/video2
    [ok] device exists
    - ir: yes
  Model directory: /usr/share/facelock/models
    - detector (det.onnx): present
    - embedder (emb.onnx): present
    [ok] all configured models present
  Execution provider: CPU
    [ok] supported by the installed ONNX Runtime
  Encryption: AES-256-GCM (TPM-sealed key)
    [ok] sealed key: /etc/facelock/sealed.key
    - encrypted: 2
    - plaintext: 0
  Enrolled faces: alice
    [ok] 2 model(s)
    - #1: front
    - #2: side
  Security:
    - require_ir: yes
    - liveness (frame variance): yes
    - liveness (landmark movement): no
    - min_auth_frames: 3
  Notifications: terminal + desktop
    - prompt: yes
    - on success: yes
    - on failure: no
  PAM module: /lib/security/pam_facelock.so
    [ok] installed
    - PAM services: sudo, polkit-1 (2 configured, 1 shadowing a vendor file)
";
        assert_eq!(render(&healthy()), expected);
    }

    /// N4, the money case: a daemon that is unreachable and a store that
    /// could not be read render as exactly that — the report must never
    /// claim "no faces enrolled" for a user it could not check.
    #[test]
    fn unreachable_daemon_and_unknown_store_never_render_as_not_enrolled() {
        let mut health = healthy();
        health.daemon = Fact::probed(DaemonPing::NotResponding {
            error: "D-Bus Ping call failed".into(),
        });
        health.enrolled = Fact::unknown("database not accessible: disk I/O error");

        let report = render(&health);
        assert!(
            report.contains("    [!!] not responding: D-Bus Ping call failed\n"),
            "{report}"
        );
        assert!(
            report.contains("    [!!] cannot determine: database not accessible: disk I/O error\n"),
            "{report}"
        );
        assert!(
            !report.contains("no faces enrolled"),
            "unknown must not collapse into 'not enrolled':\n{report}"
        );
        assert!(
            !report.contains("model(s)"),
            "unknown must not collapse into a model count:\n{report}"
        );
    }

    /// The converse pin: a *known* empty enrollment (provably absent store,
    /// C7) is the one state allowed to say "no faces enrolled".
    #[test]
    fn known_empty_enrollment_renders_no_faces_enrolled() {
        let mut health = healthy();
        health.enrolled = Fact::probed(EnrollmentHealth {
            models: vec![],
            marker: MarkerDiagnostic::Consistent,
        });
        let report = render(&health);
        assert!(
            report.contains("    [!!] no faces enrolled (run 'facelock enroll')\n"),
            "{report}"
        );
    }

    /// A config that did not parse leaves every dependent section honestly
    /// unknown — no section goes silent, none guesses.
    #[test]
    fn broken_config_renders_every_dependent_section_as_unknown() {
        let health = Health {
            config: ConfigHealth {
                path: "/etc/facelock/config.toml".into(),
                outcome: ConfigOutcome::Invalid {
                    error: "expected `]` at line 1".into(),
                },
            },
            daemon: Fact::unknown("config not available"),
            oneshot_fallback: Fact::unknown("config not available"),
            camera: Fact::unknown("config not available"),
            models: Fact::unknown("config not available"),
            execution_provider: Fact::unknown("config not available"),
            encryption: Fact::unknown("config not available"),
            user: "alice".into(),
            enrolled: Fact::unknown("config not available"),
            security: Fact::unknown("config not available"),
            notifications: Fact::unknown("config not available"),
            pam: PamWiring {
                module_path: "/lib/security/pam_facelock.so".into(),
                installed_at: None,
                configured: Vec::new(),
                not_checked: Vec::new(),
            },
        };

        let report = render(&health);
        assert!(
            report.contains("    [!!] invalid: expected `]` at line 1\n"),
            "{report}"
        );
        assert_eq!(
            report
                .matches("    [!!] cannot determine: config not available\n")
                .count(),
            9,
            "all nine config-dependent sections must render as unknown:\n{report}"
        );
        // PAM wiring is config-independent and still answers.
        assert!(report.contains("    [!!] not installed\n"), "{report}");
    }

    /// D8/H3: applied quirks are rendered, so "why does this camera behave
    /// that way" is answerable from status.
    #[test]
    fn applied_quirks_render_as_a_camera_detail() {
        let mut health = healthy();
        health.camera = Fact::probed(CameraHealth::Configured {
            path: "/dev/video2".into(),
            present: true,
            interrogated: Some(InterrogatedCamera {
                path: "/dev/video2".into(),
                name: "Logitech BRIO".into(),
                is_ir: true,
                applied_quirks: vec!["046d:085e (Logitech BRIO)".into(), "name:(?i)brio".into()],
            }),
        });
        let report = render(&health);
        assert!(
            report.contains("    - quirks: 046d:085e (Logitech BRIO), name:(?i)brio\n"),
            "{report}"
        );
        assert!(report.contains("    - ir: yes\n"), "{report}");
    }

    #[test]
    fn auto_detect_renders_the_device_it_would_select() {
        let mut health = healthy();
        health.camera = Fact::probed(CameraHealth::AutoDetect {
            resolved: Some(InterrogatedCamera {
                path: "/dev/video0".into(),
                name: "Integrated IR Camera".into(),
                is_ir: true,
                applied_quirks: vec![],
            }),
        });
        let report = render(&health);
        assert!(
            report.contains("  Camera device: (auto-detect)\n"),
            "{report}"
        );
        assert!(
            report.contains("    [ok] auto-detect enabled\n"),
            "{report}"
        );
        assert!(
            report.contains("    - device: /dev/video0 (Integrated IR Camera)\n"),
            "{report}"
        );
    }

    /// F7: the oneshot fallback line, both verdicts, with the missing
    /// prerequisites named.
    #[test]
    fn oneshot_fallback_renders_usable_and_names_whats_missing() {
        let report = render(&healthy());
        assert!(
            report.contains(
                "    [ok] prerequisites present (binary, models and database in place for daemon-less auth)\n"
            ),
            "{report}"
        );

        let mut health = healthy();
        health.oneshot_fallback = Fact::probed(OneshotFallback {
            auth_bin: "/usr/bin/facelock".into(),
            auth_bin_present: true,
            models_present: false,
            database_present: false,
        });
        let report = render(&health);
        assert!(report.contains("    - models: MISSING\n"), "{report}");
        assert!(report.contains("    - database: MISSING\n"), "{report}");
        assert!(!report.contains("    - binary: MISSING\n"), "{report}");
        assert!(
            report.contains(
                "    [!!] prerequisites missing (PAM would fall through to the next auth method if the daemon is unreachable)\n"
            ),
            "{report}"
        );
    }

    /// The EP fact renders one verdict per status, resolved against the
    /// installed ONNX Runtime (H2's fact, this renderer's words).
    #[test]
    fn execution_provider_renders_each_status() {
        let cases = [
            (
                EpStatus::Available,
                "    [ok] supported by the installed ONNX Runtime\n",
            ),
            (
                EpStatus::NotBuiltIn,
                "    [!!] not built into the installed ONNX Runtime — inference will fall back to CPU\n",
            ),
            (
                EpStatus::UnknownName,
                "    [!!] unknown execution provider (valid: cpu, cuda, rocm, openvino)\n",
            ),
            (
                EpStatus::Unqueryable("no dylib".into()),
                "    [!!] ONNX Runtime not loadable: no dylib\n",
            ),
        ];
        for (status, expected) in cases {
            let mut health = healthy();
            health.execution_provider = Fact::probed(ExecutionProviderFact {
                configured: "cuda".into(),
                status,
            });
            let report = render(&health);
            assert!(
                report.contains(expected),
                "missing {expected:?} in:\n{report}"
            );
            assert!(
                report.contains("  Execution provider: CUDA (NVIDIA GPU)\n"),
                "{report}"
            );
        }
    }

    #[test]
    fn oneshot_mode_renders_no_daemon_as_ok() {
        let mut health = healthy();
        health.daemon = Fact::claimed(DaemonPing::NotConfigured);
        let report = render(&health);
        assert!(
            report.contains("    [ok] oneshot mode (no daemon)\n"),
            "{report}"
        );
    }

    /// DEC-7: the marker diagnostic renders only when it has something to
    /// say, and says it as a detail — the store stays the authority.
    #[test]
    fn stale_marker_renders_as_a_diagnostic_detail() {
        let consistent = render(&healthy());
        assert!(!consistent.contains("- marker:"), "{consistent}");

        let mut health = healthy();
        health.enrolled = Fact::probed(EnrollmentHealth {
            models: vec![ModelSummary {
                id: 1,
                label: "front".into(),
            }],
            marker: MarkerDiagnostic::Mismatch {
                marker: 3,
                store: 1,
            },
        });
        let report = render(&health);
        assert!(
            report.contains(
                "    - marker: out of date (marker says 3, database has 1) — run 'sudo facelock setup' to reconcile\n"
            ),
            "{report}"
        );

        let mut health = healthy();
        health.enrolled = Fact::probed(EnrollmentHealth {
            models: vec![],
            marker: MarkerDiagnostic::Unreadable {
                why: "malformed marker".into(),
            },
        });
        let report = render(&health);
        assert!(
            report.contains("    - marker: unreadable: malformed marker\n"),
            "{report}"
        );
    }

    #[test]
    fn plaintext_encryption_renders_the_warning() {
        let mut health = healthy();
        health.encryption = Fact::probed(EncryptionHealth {
            method: facelock_core::config::EncryptionMethod::None,
            material: KeyMaterial::None,
            sealed_counts: Some((0, 2)),
        });
        let report = render(&health);
        assert!(report.contains("  Encryption: none\n"), "{report}");
        assert!(
            report.contains(
                "    [!!] embeddings stored as plaintext (run 'facelock setup' to enable encryption)\n"
            ),
            "{report}"
        );
        assert!(report.contains("    - encrypted: 0\n"), "{report}");
        assert!(report.contains("    - plaintext: 2\n"), "{report}");
    }

    #[test]
    fn disabled_security_renders_the_alarm_and_no_knobs() {
        let mut health = healthy();
        health.security = Fact::claimed(SecurityHealth {
            disabled: true,
            require_ir: true,
            require_frame_variance: true,
            require_landmark_liveness: true,
            min_auth_frames: 3,
        });
        let report = render(&health);
        assert!(
            report.contains("    [!!] ALL SECURITY CHECKS DISABLED\n"),
            "{report}"
        );
        assert!(!report.contains("require_ir"), "{report}");
    }

    #[test]
    fn pam_module_at_the_alternate_path_names_it() {
        let mut health = healthy();
        health.pam = PamWiring {
            module_path: "/lib/security/pam_facelock.so".into(),
            installed_at: Some("/usr/lib/security/pam_facelock.so".into()),
            configured: Vec::new(),
            not_checked: Vec::new(),
        };
        let report = render(&health);
        assert!(
            report.contains("    [ok] installed at /usr/lib/security/pam_facelock.so\n"),
            "{report}"
        );
        assert!(
            report.contains("    - PAM services: none configured\n"),
            "{report}"
        );
    }

    /// **The distinction the report exists to make.** A machine where nothing
    /// is configured and a machine the scan could not read must not render the
    /// same line — that ambiguity is what made a broken lock stack and a
    /// healthy one look identical for an hour.
    #[test]
    fn nothing_configured_and_nothing_checked_are_different_lines() {
        let mut health = healthy();
        health.pam.configured = Vec::new();
        health.pam.not_checked = Vec::new();
        assert!(
            render(&health).contains("    - PAM services: none configured\n"),
            "nothing configured, and the scan saw everywhere it meant to"
        );

        health.pam.not_checked = vec![NotChecked {
            path: "/usr/lib/pam.d".into(),
            error: "Permission denied (os error 13)".into(),
        }];
        let report = render(&health);
        assert!(
            report.contains("    - PAM services: not checked\n"),
            "an unread directory must never render as 'none configured': {report}"
        );
        assert!(
            report
                .contains("    - not checked: /usr/lib/pam.d (Permission denied (os error 13))\n"),
            "{report}"
        );
    }

    /// A service configured through a local copy of a package's file is
    /// counted as configured and says so, because the copy will not follow the
    /// package's updates.
    #[test]
    fn a_local_override_is_counted_and_named() {
        let mut health = healthy();
        health.pam.configured = vec![ConfiguredService {
            service: "sudo".into(),
            path: "/etc/pam.d/sudo".into(),
            shadows: None,
        }];
        assert!(
            render(&health).contains("    - PAM services: sudo (1 configured)\n"),
            "no override, no note"
        );
    }

    #[test]
    fn missing_models_render_per_file_and_point_at_setup() {
        let mut health = healthy();
        health.models = Fact::probed(model_files("/usr/share/facelock/models", true, false));
        let report = render(&health);
        assert!(
            report.contains("    - detector (det.onnx): present\n"),
            "{report}"
        );
        assert!(
            report.contains("    - embedder (emb.onnx): MISSING\n"),
            "{report}"
        );
        assert!(
            report.contains("    [!!] some models missing (run 'facelock setup')\n"),
            "{report}"
        );
    }
}
