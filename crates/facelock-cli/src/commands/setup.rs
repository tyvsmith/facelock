use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};
use dialoguer::{Confirm, MultiSelect, Select, theme::ColorfulTheme};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use facelock_core::Config;
use facelock_core::fs_security::{
    create_truncate_file, ensure_mode, ensure_private_dir, write_file,
};

use crate::message::{
    DeviceMessage, DownloadMessage, Message, PamMessage, SetupMessage, SystemMessage, Terminal,
    fail,
};
use crate::state_layout::{apply_dir, apply_file};

// The `/etc/pam.d` writer lives in `commands/pam.rs` (#174) — `facelock pam
// add|remove|status` is its command, and `setup --pam` is an alias onto it.
// What stays here is the wizard's *menu* (`PAM_CANDIDATES`, `candidates_in`):
// it is the list of services setup offers to configure, which is a property of
// the setup flow rather than of the writer, and moving it would have split the
// multi-select from the entries it renders for no gain.
/// The tempdir-as-search-path helper the writer's own tests use, so the
/// wizard's step 9 is driven exactly the way `pam add` is.
#[cfg(test)]
use super::pam::only;
use super::pam::{PAM_LINE, PAM_MODULE_PATHS, PamAction, PamDirs, PamRequest};

/// Embedded systemd unit file.
const SERVICE_UNIT: &str = include_str!("../../../../systemd/facelock-daemon.service");

/// Embedded D-Bus activation service file.
const DBUS_SERVICE: &str = include_str!("../../../../dbus/org.facelock.Daemon.service");

/// Embedded D-Bus policy configuration.
const DBUS_POLICY: &str = include_str!("../../../../dbus/org.facelock.Daemon.conf");

/// Embedded model manifest (same source as facelock-face).
const MANIFEST_TOML: &str = include_str!("../../../../models/manifest.toml");

/// Marker file written on successful setup completion.
pub const SETUP_COMPLETE_MARKER: &str = "/etc/facelock/.setup-complete";

/// PAM service targeted by `--pam` when `--service` is not given. Declared by
/// the writer and re-exported so `setup::DEFAULT_PAM_SERVICE` keeps resolving.
pub use super::pam::DEFAULT_PAM_SERVICE;

// ---------------------------------------------------------------------------
// CLI argument resolution
//
// `facelock setup` has enough flags that "which flag wins" needs to be a pure,
// testable function rather than a chain of `if`s in the dispatch. Everything in
// this section is a total function of the parsed CLI args: no root, no camera,
// no network, so the compatibility matrix in the plan is unit-testable.
// ---------------------------------------------------------------------------

/// Model quality preset for `--models`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ModelPreset {
    Standard,
    Balanced,
    High,
}

/// ONNX Runtime execution provider for `--execution-provider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ExecutionProviderChoice {
    Cpu,
    Cuda,
    Rocm,
    Openvino,
    Auto,
}

/// Embedding encryption method for `--encryption`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EncryptionChoice {
    Tpm,
    Keyfile,
    None,
    Auto,
}

/// Camera selection for `--camera`. `auto` re-derives from hardware rather than
/// meaning "the default" — omitting the flag already gives you the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraChoice {
    Path(String),
    Auto,
}

/// Which base setup flow runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseMode {
    Wizard,
    NonInteractive,
}

/// Systemd preference. `Ask` = wizard prompts (today's default); under a
/// non-interactive base `Ask` means "do nothing", exactly as today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemdPref {
    Ask,
    Install,
    Disable,
    Skip,
}

/// PAM preference. `Ask` behaves like [`SystemdPref::Ask`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PamPref {
    Ask,
    Install {
        service: Option<String>,
        if_present: bool,
    },
    Remove {
        service: String,
        if_present: bool,
    },
    Skip,
}

/// Raw `facelock setup` arguments, exactly as clap parsed them.
///
/// This mirrors the `Commands::Setup` variant field for field so that
/// [`resolve_setup_plan`] is the only place the precedence rules live.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupArgs {
    pub non_interactive: bool,
    pub yes: bool,
    pub pam: bool,
    pub no_pam: bool,
    pub systemd: bool,
    pub no_systemd: bool,
    pub enroll: bool,
    pub no_enroll: bool,
    pub disable: bool,
    pub service: Option<String>,
    pub remove: bool,
    pub if_present: bool,
    pub allow_sensitive: bool,
    pub camera: Option<String>,
    pub models: Option<ModelPreset>,
    pub execution_provider: Option<ExecutionProviderChoice>,
    pub encryption: Option<EncryptionChoice>,
}

/// Fully resolved `facelock setup` invocation. Pure function of the CLI args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPlan {
    /// `None` = standalone action mode: no base setup runs, only the systemd
    /// and/or PAM actions below — exactly today's `--systemd` / `--pam` behavior.
    pub base: Option<BaseMode>,
    pub systemd: SystemdPref,
    pub pam: PamPref,
    /// `None` = ask (wizard) / skip (non-interactive)
    pub enroll: Option<bool>,
    pub camera: Option<CameraChoice>,
    pub models: Option<ModelPreset>,
    pub execution_provider: Option<ExecutionProviderChoice>,
    pub encryption: Option<EncryptionChoice>,
    pub yes: bool,
    pub allow_sensitive: bool,
}

impl Default for SetupPlan {
    /// The full interactive wizard — what a bare `facelock setup` resolves to.
    fn default() -> Self {
        Self {
            base: Some(BaseMode::Wizard),
            systemd: SystemdPref::Ask,
            pam: PamPref::Ask,
            enroll: None,
            camera: None,
            models: None,
            execution_provider: None,
            encryption: None,
            yes: false,
            allow_sensitive: false,
        }
    }
}

/// Resolve raw CLI args into a [`SetupPlan`].
///
/// The load-bearing rule is `base_requested`: any flag that only makes sense
/// while the base setup runs forces the base to run. Without it, `setup
/// --camera=/dev/video2 --pam` would silently drop `--camera` the way the old
/// mutually-exclusive dispatch did.
pub fn resolve_setup_plan(args: SetupArgs) -> SetupPlan {
    let base_requested = args.non_interactive
        || args.no_pam
        || args.no_systemd
        || args.enroll
        || args.no_enroll
        || args.camera.is_some()
        || args.models.is_some()
        || args.execution_provider.is_some()
        || args.encryption.is_some();

    // `--pam` / `--systemd` on their own keep their historical meaning: perform
    // just that action, touch nothing else.
    let standalone = !base_requested && (args.pam || args.systemd);

    let base = if standalone {
        None
    } else if args.non_interactive {
        Some(BaseMode::NonInteractive)
    } else {
        Some(BaseMode::Wizard)
    };

    let systemd = if args.systemd && args.disable {
        SystemdPref::Disable
    } else if args.systemd {
        SystemdPref::Install
    } else if args.no_systemd {
        SystemdPref::Skip
    } else {
        SystemdPref::Ask
    };

    let pam = if args.pam && args.remove {
        // Removal always needs a concrete service, so apply the default now.
        PamPref::Remove {
            service: args
                .service
                .clone()
                .unwrap_or_else(|| DEFAULT_PAM_SERVICE.to_string()),
            if_present: args.if_present,
        }
    } else if args.pam {
        PamPref::Install {
            service: args.service.clone(),
            if_present: args.if_present,
        }
    } else if args.no_pam {
        PamPref::Skip
    } else {
        PamPref::Ask
    };

    let enroll = if args.enroll {
        Some(true)
    } else if args.no_enroll {
        Some(false)
    } else {
        None
    };

    let camera = args.camera.map(|s| {
        if s == "auto" {
            CameraChoice::Auto
        } else {
            CameraChoice::Path(s)
        }
    });

    SetupPlan {
        base,
        systemd,
        pam,
        enroll,
        camera,
        models: args.models,
        execution_provider: args.execution_provider,
        encryption: args.encryption,
        yes: args.yes,
        allow_sensitive: args.allow_sensitive,
    }
}

#[derive(Debug, serde::Deserialize)]
struct ModelManifest {
    models: Vec<ModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct ModelEntry {
    name: String,
    filename: String,
    purpose: String,
    size_mb: u64,
    sha256: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    #[allow(dead_code)] // parsed from manifest TOML; used in tests
    optional: bool,
}

impl ModelManifest {
    fn find(&self, filename: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.filename == filename)
    }
}

/// Check whether stdin is connected to an interactive terminal.
fn is_interactive() -> bool {
    std::io::stdin().is_terminal()
}

/// Entry point for a plain `facelock setup` / `facelock setup --non-interactive`.
///
/// Kept as a thin wrapper over [`run_with_plan`] because `enroll` calls it.
pub fn run(non_interactive: bool) -> anyhow::Result<()> {
    let base = if non_interactive {
        BaseMode::NonInteractive
    } else {
        BaseMode::Wizard
    };
    run_with_plan(SetupPlan {
        base: Some(base),
        ..SetupPlan::default()
    })
}

/// Whether to run the interactive root pre-check before executing a plan.
///
/// Only when a base setup runs. `ipc_client::require_root` prompts and re-execs
/// under `sudo` on a TTY, and standalone `--pam` / `--systemd` never did that:
/// they bail immediately from their own root checks (`commands::pam`,
/// `check_root`).
/// Escalating there would be a new, surprising behavior for exactly the
/// scripted invocations that must stay byte-compatible.
pub fn needs_root_precheck(plan: &SetupPlan) -> bool {
    plan.base.is_some()
}

/// The writer's two independent knobs, named.
///
/// A pair of `bool`s in a tuple is how `install_for_setup(services,
/// no_confirm, allow_sensitive)` and `install_one_in(base, service,
/// allow_sensitive, no_confirm)` came to disagree about their order — a swap
/// type-checks and silently unlocks the gate. Every hop from here to the
/// writer now names the field it is filling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PamKnobs {
    no_confirm: bool,
    allow_sensitive: bool,
}

/// How `setup`'s flags map onto the writer's two knobs.
///
/// A pure function rather than an expression inline in [`run_with_plan`]
/// because it *is* the security property: `--non-interactive` promises no
/// prompts, so it suppresses the per-file "Proceed?" confirmation, and it
/// deliberately does **not** bypass the sensitive-service gate. `setup --yes`
/// also suppresses prompts only. `--allow-sensitive` is the separate,
/// explicit authorization to unlock the sensitive services, matching
/// `facelock pam add`; neither flag implies the other.
fn setup_pam_knobs(plan: &SetupPlan) -> PamKnobs {
    let no_prompt = plan.base == Some(BaseMode::NonInteractive);
    PamKnobs {
        no_confirm: plan.yes || no_prompt,
        allow_sensitive: plan.allow_sensitive,
    }
}

/// The request one `setup --pam` service makes of the writer.
///
/// Spelled once, so the four call sites — two in [`run_with_plan`], two in the
/// wizard — cannot fill the same struct four slightly different ways.
fn pam_request(action: PamAction, service: &str, knobs: PamKnobs, if_present: bool) -> PamRequest {
    PamRequest {
        action,
        services: vec![service.to_string()],
        no_confirm: knobs.no_confirm,
        allow_sensitive: knobs.allow_sensitive,
        if_present,
        ..PamRequest::default()
    }
}

/// Execute a resolved plan: base setup first (if any), then the standalone
/// systemd and PAM actions, in that order — the wizard runs systemd (step 6)
/// before PAM (step 9), and `--systemd --pam` must match.
pub fn run_with_plan(plan: SetupPlan) -> anyhow::Result<()> {
    if needs_root_precheck(&plan) {
        crate::ipc_client::require_root("sudo facelock setup")?;
    }

    // Whether the interactive wizard ran, and therefore already asked about PAM.
    let mut wizard_ran = false;

    match plan.base {
        Some(BaseMode::NonInteractive) => run_non_interactive(&plan)?,
        // A non-tty demotes the wizard to the non-interactive flow, as before.
        Some(BaseMode::Wizard) if is_interactive() => {
            wizard_ran = true;
            run_wizard(&plan)?;
        }
        Some(BaseMode::Wizard) => run_non_interactive(&plan)?,
        None => {}
    }

    match plan.systemd {
        // Only the standalone `--systemd` / `--systemd --disable` are applied
        // here. A base flow applies its own, at step 6, because it has to
        // land before enrollment: `enroll` and `test` select their transport
        // at entry, so a daemon installed after them is never the one they
        // used — both would run direct-by-fallback and the recognition test
        // would validate a transport no later authentication takes.
        SystemdPref::Install if plan.base.is_none() => run_systemd(false)?,
        SystemdPref::Disable if plan.base.is_none() => run_systemd(true)?,
        SystemdPref::Install | SystemdPref::Disable => {}
        // Under a base flow `Ask` is step 6; everywhere else it means nothing.
        SystemdPref::Ask | SystemdPref::Skip => {}
    }

    // `--pam` is an alias onto `facelock pam add|remove` (#174): the plan and
    // its precedence rules stay here, the execution is the writer's.
    match &plan.pam {
        PamPref::Remove {
            service,
            if_present,
        } => super::pam::remove_for_setup(&pam_request(
            PamAction::Remove,
            service,
            // Removal is never gated on sensitivity and never prompts, so
            // neither knob has anything to do here. Default rather than
            // `setup_pam_knobs(&plan)`: handing a path that ignores the gate a
            // request that says "gate unlocked" pre-arms a bypass for whoever
            // later makes it read the field.
            PamKnobs::default(),
            *if_present,
        ))?,
        PamPref::Install {
            service,
            if_present,
        } => {
            // The wizard's step 9 already applied `--pam`; installing again here
            // would be a second, unasked-for edit of the same file.
            if !wizard_ran {
                let service = service.as_deref().unwrap_or(DEFAULT_PAM_SERVICE);
                super::pam::install_for_setup(&pam_request(
                    PamAction::Add,
                    service,
                    setup_pam_knobs(&plan),
                    *if_present,
                ))?;
            }
        }
        PamPref::Ask | PamPref::Skip => {}
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Interactive wizard
// ---------------------------------------------------------------------------

fn run_wizard(plan: &SetupPlan) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    // -- Welcome --
    Terminal.info(&SetupMessage::SetupIntro {
        version: env!("CARGO_PKG_VERSION").to_string(),
    });

    // -- Load or create config --
    // Deliberate load (D7): setup bootstraps the config file — it may not
    // exist yet, and the wizard edits it in place afterwards.
    let mut config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not load config ({e}), using default paths");
            create_default_config()?;
            Config::load().context("failed to load config after creating default")?
        }
    };

    // -- Create directories (always needed) --
    create_directories(&config)?;
    ensure_state_layout_or_bail(&config)?;

    // -- Step 1: Camera selection --
    // `--camera` answers the question and therefore replaces the prompt (plan
    // §1 rule 1). An explicit value that cannot be honoured is fatal: the user
    // asked for a specific device, so falling back would be silently wrong.
    Terminal.info(&SetupMessage::SetupStepCamera);
    match plan.camera.as_ref() {
        Some(choice) => apply_camera_choice(&mut config, choice)?,
        None => match wizard_camera_selection(&theme, &mut config) {
            Ok(()) => {}
            Err(e) if camera_selection_error_is_fatal(&e) => return Err(e),
            Err(e) => {
                Terminal.info(&SetupMessage::CameraStepFailed {
                    error: e.to_string(),
                    current: config
                        .device
                        .path
                        .as_deref()
                        .unwrap_or("/dev/video0")
                        .to_string(),
                });
            }
        },
    }

    // -- Step 2: Model quality --
    Terminal.info(&SetupMessage::SetupStepModelQuality);
    match plan.models {
        Some(preset) => apply_model_preset(&mut config, preset)?,
        None => match wizard_model_quality(&theme, &mut config) {
            Ok(()) => {}
            Err(e) => {
                Terminal.info(&SetupMessage::ModelQualityStepFailed {
                    error: e.to_string(),
                    current: config.recognition.detector_model.clone(),
                });
            }
        },
    }

    // -- Step 3: Execution provider --
    Terminal.info(&SetupMessage::SetupStepInferenceDevice);
    match plan.execution_provider {
        Some(choice) => apply_execution_provider(&mut config, choice)?,
        None => match wizard_execution_provider(&theme, &mut config) {
            Ok(()) => {}
            Err(e) => {
                Terminal.info(&SetupMessage::InferenceStepFailed {
                    error: e.to_string(),
                    current: config.recognition.execution_provider.clone(),
                });
            }
        },
    }

    // -- Step 4: Model download --
    Terminal.info(&SetupMessage::SetupStepModelDownload);
    match wizard_model_download(&theme, &config) {
        Ok(()) => {}
        Err(e) => {
            Terminal.info(&SetupMessage::ModelDownloadStepFailed {
                error: e.to_string(),
            });
        }
    }

    // -- Step 5: Encryption setup --
    Terminal.info(&SetupMessage::SetupStepEncryption);
    match plan.encryption {
        Some(choice) => apply_encryption_choice(&mut config, choice, Some(&theme))?,
        None => match wizard_encryption_setup(&theme, &mut config) {
            Ok(()) => {}
            Err(e) => {
                Terminal.info(&SetupMessage::EncryptionStepFailed {
                    error: e.to_string(),
                });
            }
        },
    }

    // -- Step 6: Daemon configuration --
    //
    // Ahead of enrollment on purpose. Both steps below select their transport
    // once, at entry (`backend::Backend::select`), so a daemon installed after
    // them is never the one they used: enrollment would run direct-by-fallback
    // and the recognition test would validate a transport that no later
    // authentication takes. Still after model download and encryption — the
    // daemon loads the models and opens the embedding key at startup, so it
    // has nothing to start from before those two steps have run.
    Terminal.info(&SetupMessage::SetupStepDaemon);
    let systemd_step = systemd_step_for(plan);
    let systemd_enabled = match systemd_step {
        SystemdStep::Ask => match wizard_systemd_setup(&theme, plan.yes) {
            Ok(enabled) => enabled,
            Err(e) => {
                Terminal.info(&SetupMessage::SystemdStepFailed {
                    error: e.to_string(),
                });
                false
            }
        },
        // Answered on the command line. Applied here rather than after the
        // whole base flow, for the ordering reason above; the error it can
        // raise is the one `run_with_plan` used to raise, only earlier.
        SystemdStep::Install => {
            Terminal.info(&SystemMessage::SystemdFromCommandLine);
            run_systemd(false)?;
            true
        }
        SystemdStep::Disable => {
            Terminal.info(&SystemMessage::SystemdFromCommandLine);
            run_systemd(true)?;
            false
        }
        SystemdStep::Skip => {
            Terminal.info(&SystemMessage::SystemdSkippedFlag);
            false
        }
    };

    if systemd_enabled {
        start_daemon_for_setup(&config);
    }

    // -- Step 7: Face enrollment --
    let enroll_steps = enroll_steps_for(plan);
    let enrolled = if enroll_steps.enroll {
        Terminal.info(&SetupMessage::SetupStepEnrollment);
        match wizard_face_enroll(&config, &theme, enroll_steps.assume_yes) {
            Ok(did_enroll) => did_enroll,
            Err(e) => {
                Terminal.info(&SetupMessage::EnrollStepFailed {
                    error: e.to_string(),
                });
                false
            }
        }
    } else {
        Terminal.info(&SetupMessage::SetupStepEnrollmentSkipped);
        false
    };

    // -- Step 8: Test recognition --
    if test_recognition_runs(enroll_steps, enrolled) {
        Terminal.info(&SetupMessage::SetupStepTest);
        match wizard_test_recognition(&config, &theme, plan.yes) {
            Ok(()) => {}
            Err(e) => {
                Terminal.info(&SetupMessage::TestStepFailed {
                    error: e.to_string(),
                });
            }
        }
    } else {
        Terminal.info(&SetupMessage::SetupStepTestSkipped);
    }

    // -- Step 9: PAM configuration --
    Terminal.info(&SetupMessage::SetupStepPam);
    let pam_services = match pam_step_in(
        &PamDirs::system(),
        plan,
        &theme,
        super::pam::module_installed(),
    ) {
        Ok(services) => services,
        Err(e) => {
            Terminal.info(&SetupMessage::PamStepFailed {
                error: e.to_string(),
            });
            Vec::new()
        }
    };

    if pam_services.iter().any(|s| s == "hyprlock") {
        wizard_hyprlock_handoff(&theme);
    }

    // The closing hint fires once per wizard run, whatever step 9 decided —
    // it is advice about extending PAM by hand, not a report of what happened.
    Terminal.info(&PamMessage::PamExtensionHint {
        line: PAM_LINE.to_string(),
    });

    // -- Summary --
    Terminal.info(&SetupMessage::SetupCompleteHeader);
    let encryption_label = match config.encryption.method {
        facelock_core::config::EncryptionMethod::Tpm => "AES-256-GCM (TPM-sealed key)",
        facelock_core::config::EncryptionMethod::Keyfile => "AES-256-GCM (keyfile)",
        facelock_core::config::EncryptionMethod::None => "none (NOT RECOMMENDED)",
    };
    let model_quality_label = match (
        config.recognition.detector_model.as_str(),
        config.recognition.embedder_model.as_str(),
    ) {
        ("det_10g.onnx", "glintr100.onnx") => "high accuracy (SCRFD 10G + ArcFace R100)",
        ("scrfd_2.5g_bnkps.onnx", "glintr100.onnx") => "balanced (SCRFD 2.5G + ArcFace R100)",
        _ => "standard (SCRFD 2.5G + ArcFace R50)",
    };
    Terminal.info(&SetupMessage::SummaryCamera {
        value: config
            .device
            .path
            .as_deref()
            .unwrap_or("/dev/video0")
            .to_string(),
    });
    Terminal.info(&SetupMessage::SummaryModels {
        dir: config.daemon.model_dir.clone(),
        quality: model_quality_label.to_string(),
    });
    Terminal.info(&SetupMessage::SummaryInference {
        value: config.recognition.execution_provider.to_uppercase(),
    });
    Terminal.info(&SetupMessage::SummaryDatabase {
        value: config.storage.db_path.clone(),
    });
    Terminal.info(&SetupMessage::SummaryEncryption {
        value: encryption_label.to_string(),
    });
    Terminal.info(&SetupMessage::SummaryDaemon {
        status: match systemd_step {
            SystemdStep::Skip => SetupMessage::DaemonStatusNotConfiguredNoSystemd,
            SystemdStep::Install | SystemdStep::Disable => {
                SetupMessage::DaemonStatusFromCommandLine
            }
            SystemdStep::Ask if systemd_enabled => SetupMessage::DaemonStatusEnabled,
            SystemdStep::Ask => SetupMessage::DaemonStatusNotConfigured,
        }
        .localized(),
    });
    if !pam_services.is_empty() {
        Terminal.info(&SetupMessage::SummaryPam {
            services: pam_services.join(", "),
        });
    } else if pam_step_for(plan) == PamStep::Skip {
        Terminal.info(&SetupMessage::SummaryPamSkipped);
    } else {
        Terminal.info(&SetupMessage::SummaryPamNone);
    }
    if enrolled {
        Terminal.info(&SetupMessage::SummaryFaceEnrolled);
    } else if !enroll_steps.enroll {
        Terminal.info(&SetupMessage::SummaryFaceNotEnrolledNoEnroll);
    } else {
        Terminal.info(&SetupMessage::SummaryFaceNotEnrolled);
    }
    Terminal.info(&SetupMessage::BlankLine);

    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;
    secure_setup_paths(&config, Some(&manifest))?;
    write_setup_marker()?;
    // Backfill/refresh enrollment markers from the DB. Needs DB access, so it
    // only ever runs here in privileged setup, never in `facelock is-enrolled`.
    if let Err(e) = super::enrollment_marker::reconcile_all(&config) {
        tracing::warn!("could not reconcile enrollment markers: {e}");
    }
    Ok(())
}

fn write_setup_marker() -> anyhow::Result<()> {
    let path = std::path::Path::new(SETUP_COMPLETE_MARKER);
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent, 0o755)?;
    }
    write_file(path, b"", 0o644)?;
    Ok(())
}

fn wizard_camera_selection(theme: &ColorfulTheme, config: &mut Config) -> anyhow::Result<()> {
    let candidates = enumerate_camera_candidates()?;

    if candidates.is_empty() {
        Terminal.info(&DeviceMessage::NoVideoDevices);
        return Ok(());
    }

    // A setup selection becomes an explicit device.path, so the auth path will
    // never revisit auto-detection's decodability filter. Apply the same
    // predicate here before either auto-selecting or presenting the menu.
    let selectable = wizard_camera_candidates(&candidates, config.security.require_ir)?;
    if selectable.is_empty() {
        bail!(
            "no camera with a decodable pixel format found; detected:\n{}",
            camera_candidate_listing(&candidates)
        );
    }

    let ir_devices: Vec<_> = selectable
        .iter()
        .copied()
        .filter(|candidate| candidate.is_ir)
        .collect();

    // If exactly one IR camera, auto-select it
    if ir_devices.len() == 1 {
        let dev = &ir_devices[0].device;
        Terminal.info(&DeviceMessage::AutoSelectedIrCamera {
            path: camera_display_field(&dev.path),
            name: camera_display_field(&dev.name),
        });
        config.device.path = Some(dev.path.clone());
        return Ok(());
    }

    // Build display list
    let display_items: Vec<String> = selectable
        .iter()
        .map(|candidate| candidate.menu_listing())
        .collect();

    // Find the currently configured device index for default selection
    let default_idx = selectable
        .iter()
        .position(|candidate| {
            config
                .device
                .path
                .as_ref()
                .is_some_and(|path| candidate.device.path == *path)
        })
        .or_else(|| {
            // Default to first IR camera if available
            selectable.iter().position(|candidate| candidate.is_ir)
        })
        .unwrap_or(0);

    let selection = Select::with_theme(theme)
        .with_prompt(DeviceMessage::PromptSelectCameraDevice.localized())
        .items(&display_items)
        .default(default_idx)
        .interact()?;

    let selected = &selectable[selection].device;
    config.device.path = Some(selected.path.clone());
    Terminal.info(&DeviceMessage::SelectedCamera {
        path: camera_display_field(&selected.path),
        name: camera_display_field(&selected.name),
    });

    Ok(())
}

/// One enumerated video device plus its IR verdict.
///
/// `--camera auto` selection is expressed over this rather than over V4L2 so it
/// stays a pure function: testable on a machine with no camera at all.
#[derive(Debug, Clone)]
struct CameraCandidate {
    device: facelock_camera::DeviceInfo,
    is_ir: bool,
}

#[derive(Debug)]
struct RequiredIrUnavailable {
    listed: String,
}

impl std::fmt::Display for RequiredIrUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "security.require_ir is enabled, but every detected IR camera advertises no decodable pixel format:\n{}\n  Connect an IR camera that advertises a supported format such as GREY or Y16; setup will not fall back to RGB.",
            self.listed
        )
    }
}

impl std::error::Error for RequiredIrUnavailable {}

// Other camera-step failures keep the wizard's long-standing recover-and-
// report behavior. This typed refusal alone must abort before the menu because
// recovering would let setup continue with no camera `require_ir` can admit.
fn camera_selection_error_is_fatal(error: &anyhow::Error) -> bool {
    error.is::<RequiredIrUnavailable>()
}

impl CameraCandidate {
    fn display_path(&self) -> String {
        camera_display_field(&self.device.path)
    }

    fn display_name(&self) -> String {
        camera_display_field(&self.device.name)
    }

    fn menu_listing(&self) -> String {
        let ir_tag = if self.is_ir { " [IR]" } else { "" };
        format!(
            "{}{} - \"{}\"",
            self.display_path(),
            ir_tag,
            self.display_name()
        )
    }

    fn format_listing(&self) -> String {
        self.device
            .formats
            .iter()
            .map(|format| camera_display_field(format.fourcc.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Render hardware-derived text without allowing control characters to create
/// fake terminal lines, prompts, or escape sequences.
fn camera_display_field(value: &str) -> String {
    value.chars().flat_map(char::escape_default).collect()
}

/// Setup may persist only candidates the auth path can decode. This is a
/// post-classification usability filter: it never changes whether a node is IR.
fn decodable_camera_candidates(candidates: &[CameraCandidate]) -> Vec<&CameraCandidate> {
    candidates
        .iter()
        .filter(|candidate| facelock_camera::has_decodable_format(&candidate.device))
        .collect()
}

fn wizard_camera_candidates(
    candidates: &[CameraCandidate],
    require_ir: bool,
) -> anyhow::Result<Vec<&CameraCandidate>> {
    let selectable = decodable_camera_candidates(candidates);
    let has_ir = candidates.iter().any(|candidate| candidate.is_ir);
    let has_decodable_ir = selectable.iter().any(|candidate| candidate.is_ir);

    if require_ir && has_ir && !has_decodable_ir {
        let listed = candidates
            .iter()
            .filter(|candidate| candidate.is_ir)
            .map(|candidate| {
                format!(
                    "    {} [{}]",
                    candidate.display_path(),
                    candidate.format_listing()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(RequiredIrUnavailable { listed }.into());
    }

    Ok(selectable)
}

fn camera_candidate_listing(candidates: &[CameraCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| {
            let usability = if facelock_camera::has_decodable_format(&candidate.device) {
                ""
            } else {
                " (excluded: no decodable pixel format)"
            };
            format!(
                "    {} [{}]{}",
                candidate.display_path(),
                candidate.format_listing(),
                usability
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn warn_undecodable_ir_candidates(candidates: &[CameraCandidate]) {
    for candidate in candidates.iter().filter(|candidate| {
        candidate.is_ir && !facelock_camera::has_decodable_format(&candidate.device)
    }) {
        tracing::warn!(
            device = %candidate.display_path(),
            name = %candidate.display_name(),
            formats = %candidate.format_listing(),
            "IR-classified camera has no decodable pixel format — excluded from setup selection"
        );
    }
}

/// Pick the single decodable IR device for `--camera auto`.
///
/// Zero and many are both errors: silently taking the first IR node would pin
/// setup to whichever device happened to enumerate first, and getting that
/// wrong means auth never works.
fn select_ir_camera(candidates: &[CameraCandidate]) -> anyhow::Result<String> {
    let decodable = decodable_camera_candidates(candidates);
    let ir: Vec<&CameraCandidate> = decodable
        .iter()
        .copied()
        .filter(|candidate| candidate.is_ir)
        .collect();

    match ir.len() {
        1 => Ok(ir[0].device.path.clone()),
        0 if candidates.is_empty() => Err(fail(DeviceMessage::AutoCameraNoDevices)),
        0 => {
            let listed = camera_candidate_listing(candidates);
            Err(fail(DeviceMessage::AutoCameraNoIr {
                listed,
                example: if candidates.iter().any(|candidate| candidate.is_ir) {
                    "<path-to-decodable-ir-camera>".to_string()
                } else {
                    decodable
                        .first()
                        .map(|candidate| candidate.display_path())
                        .unwrap_or_else(|| "<path-to-decodable-camera>".to_string())
                },
            }))
        }
        _ => {
            let listed = ir
                .iter()
                .map(|candidate| {
                    format!(
                        "    {} [{}]",
                        candidate.display_path(),
                        candidate.format_listing()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Err(fail(DeviceMessage::AutoCameraManyIr {
                count: ir.len(),
                listed,
            }))
        }
    }
}

/// Enumerate and classify video devices the same way step 1 does, so `auto`
/// and the prompt agree on what counts as IR.
fn enumerate_camera_candidates() -> anyhow::Result<Vec<CameraCandidate>> {
    let devices = facelock_camera::list_devices().map_err(|e| anyhow::anyhow!("{e}"))?;
    let quirks = facelock_camera::QuirksDb::load();
    let sources = facelock_camera::classify_ir_sources(&devices, Some(&quirks));

    let candidates = devices
        .into_iter()
        .enumerate()
        .map(|(i, device)| CameraCandidate {
            device,
            is_ir: sources
                .get(i)
                .copied()
                .unwrap_or(facelock_camera::IrSource::None)
                != facelock_camera::IrSource::None,
        })
        .collect::<Vec<_>>();
    warn_undecodable_ir_candidates(&candidates);
    Ok(candidates)
}

/// Apply `--camera`, persisting the result. `Auto` re-derives from hardware and
/// ignores whatever the config already says (plan §1 rule 3).
fn apply_camera_choice(config: &mut Config, choice: &CameraChoice) -> anyhow::Result<()> {
    let path = match choice {
        CameraChoice::Auto => {
            let path = select_ir_camera(&enumerate_camera_candidates()?)?;
            Terminal.info(&DeviceMessage::AutoSelectedIrCameraPath {
                path: camera_display_field(&path),
            });
            path
        }
        CameraChoice::Path(path) => {
            if !Path::new(path).exists() {
                return Err(fail(DeviceMessage::CameraDeviceMissing {
                    path: path.clone(),
                }));
            }
            Terminal.info(&DeviceMessage::SelectedValue {
                value: camera_display_field(path),
            });
            path.clone()
        }
    };

    config.device.path = Some(path);
    update_config_device(config)?;
    Ok(())
}

/// Persist `[device] path`. Counterpart to [`update_config_provider`]; the
/// prompt path has never written the device back, so this is only reached when
/// `--camera` supplied the value.
fn update_config_device(config: &Config) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if !config_path.exists() {
        return Ok(());
    }
    update_config_device_at(&config_path, config)
}

fn update_config_device_at(config_path: &Path, config: &Config) -> anyhow::Result<()> {
    let Some(path) = config.device.path.as_deref() else {
        return Ok(());
    };
    set_config_scalar(config_path, "device", "path", path)
}

/// The wizard's model-quality options, in prompt order. The `--models` values
/// and the `Select` arms index the same table so the two cannot drift.
const MODEL_PRESETS: [ModelPreset; 3] = [
    ModelPreset::Standard,
    ModelPreset::Balanced,
    ModelPreset::High,
];

/// Prompt labels, in the same order as [`MODEL_PRESETS`].
const MODEL_PRESET_OPTIONS: [&str; 3] = [
    "Standard (recommended) — SCRFD 2.5G + ArcFace R50 (~170MB, fast)",
    "Balanced — SCRFD 2.5G + ArcFace R100 (~252MB, ~15-30ms slower)",
    "High accuracy — SCRFD 10G + ArcFace R100 (~266MB, ~40-50ms slower)",
];

/// `(detector, embedder)` filenames for a preset. Single source of truth.
fn preset_models(preset: ModelPreset) -> (&'static str, &'static str) {
    match preset {
        ModelPreset::Standard => ("scrfd_2.5g_bnkps.onnx", "w600k_r50.onnx"),
        ModelPreset::Balanced => ("scrfd_2.5g_bnkps.onnx", "glintr100.onnx"),
        ModelPreset::High => ("det_10g.onnx", "glintr100.onnx"),
    }
}

/// The preset a config currently corresponds to, if any.
fn preset_of_models(detector: &str, embedder: &str) -> Option<ModelPreset> {
    MODEL_PRESETS
        .into_iter()
        .find(|p| preset_models(*p) == (detector, embedder))
}

/// What the wizard says once a preset is chosen.
fn preset_summary(preset: ModelPreset) -> DeviceMessage {
    match preset {
        ModelPreset::Standard => DeviceMessage::SelectedModelsStandard,
        ModelPreset::Balanced => DeviceMessage::SelectedModelsBalanced,
        ModelPreset::High => DeviceMessage::SelectedModelsHigh,
    }
}

/// Write a preset into `config`, taking the checksums from the bundled
/// manifest. In-memory half of [`apply_model_preset`].
fn set_model_preset(config: &mut Config, preset: ModelPreset) -> anyhow::Result<()> {
    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;
    let (detector, embedder) = preset_models(preset);

    config.recognition.detector_model = detector.to_string();
    config.recognition.detector_sha256 = manifest.find(detector).map(|m| m.sha256.clone());
    config.recognition.embedder_model = embedder.to_string();
    config.recognition.embedder_sha256 = manifest.find(embedder).map(|m| m.sha256.clone());
    Ok(())
}

/// Select a model preset and persist it. Used by both the prompt and
/// `--models`, so the two cannot disagree about what a preset means.
fn apply_model_preset(config: &mut Config, preset: ModelPreset) -> anyhow::Result<()> {
    set_model_preset(config, preset)?;
    Terminal.info(&preset_summary(preset));
    update_config_models(config)?;
    Ok(())
}

fn wizard_model_quality(theme: &ColorfulTheme, config: &mut Config) -> anyhow::Result<()> {
    let default_idx = preset_of_models(
        &config.recognition.detector_model,
        &config.recognition.embedder_model,
    )
    .and_then(|p| MODEL_PRESETS.iter().position(|c| *c == p))
    .unwrap_or(0);

    let selection = Select::with_theme(theme)
        .with_prompt(DeviceMessage::PromptSelectModelQuality.localized())
        .items(&MODEL_PRESET_OPTIONS[..])
        .default(default_idx)
        .interact()?;

    apply_model_preset(
        config,
        MODEL_PRESETS[selection.min(MODEL_PRESETS.len() - 1)],
    )
}

fn wizard_execution_provider(theme: &ColorfulTheme, config: &mut Config) -> anyhow::Result<()> {
    let current = config.recognition.execution_provider.as_str();

    let default_idx = match current {
        "cuda" => 1,
        _ => 0,
    };

    let options = [
        "CPU (recommended — works everywhere)",
        "CUDA (NVIDIA GPU — requires onnxruntime-opt-cuda package)",
    ];

    let selection = Select::with_theme(theme)
        .with_prompt(DeviceMessage::PromptSelectInferenceDevice.localized())
        .items(&options[..])
        .default(default_idx)
        .interact()?;

    let provider = match selection {
        1 => "cuda",
        _ => "cpu",
    };

    config.recognition.execution_provider = provider.to_string();
    Terminal.info(&DeviceMessage::SelectedValue {
        value: provider.to_string(),
    });
    warn_provider_preflight(provider);

    update_config_provider(config)?;
    Ok(())
}

/// Resolve `--execution-provider=auto` by asking the installed ONNX Runtime
/// which providers it was built with, preferring cuda > rocm > openvino > cpu.
///
/// Always prints what it found and why: the failure this exists to fix is a
/// machine with `onnxruntime-opt-cuda` silently running CPU inference, so a
/// silent `cpu` answer would be no better than the old error.
///
/// A runtime that cannot be loaded at all is reported loudly and resolves to
/// `cpu` rather than aborting setup — no provider is usable in that state, and
/// `cpu` is the only one that can become usable once the package is installed.
fn resolve_execution_provider_auto() -> anyhow::Result<String> {
    match facelock_face::detect_execution_provider() {
        Ok(detection) => {
            Terminal.info(&DeviceMessage::DetectedProvider {
                detail: detection.explain(),
            });
            Ok(detection.provider.as_str().to_string())
        }
        Err(e) => {
            Terminal.info(&DeviceMessage::ProviderQueryFailed {
                error: e.to_string(),
            });
            Ok("cpu".to_string())
        }
    }
}

/// The config value for an `--execution-provider` choice.
fn provider_name(choice: ExecutionProviderChoice) -> anyhow::Result<String> {
    Ok(match choice {
        ExecutionProviderChoice::Cpu => "cpu".to_string(),
        ExecutionProviderChoice::Cuda => "cuda".to_string(),
        ExecutionProviderChoice::Rocm => "rocm".to_string(),
        ExecutionProviderChoice::Openvino => "openvino".to_string(),
        ExecutionProviderChoice::Auto => resolve_execution_provider_auto()?,
    })
}

/// Warn about a GPU provider selected without the pieces it needs. Runs for the
/// flag as well as the prompt — the failure mode (silent CPU fallback at auth
/// time) is identical either way.
fn warn_provider_preflight(provider: &str) {
    if provider != "cuda" {
        return;
    }
    let has_nvidia_driver = Path::new("/dev/nvidiactl").exists();
    let has_cuda_ort = ["/usr/lib/libonnxruntime.so", "/usr/lib64/libonnxruntime.so"]
        .iter()
        .any(|p| Path::new(p).exists());

    if !has_nvidia_driver {
        Terminal.info(&DeviceMessage::NvidiaDriverMissing);
    }
    if !has_cuda_ort {
        Terminal.info(&DeviceMessage::CudaRuntimeMissing);
    }
}

/// Apply `--execution-provider`. The prompt only offers CPU and CUDA; `rocm`
/// and `openvino` are valid config values (see `facelock-face`'s provider
/// registry) and are accepted from the flag.
fn apply_execution_provider(
    config: &mut Config,
    choice: ExecutionProviderChoice,
) -> anyhow::Result<()> {
    let provider = provider_name(choice)?;
    config.recognition.execution_provider = provider.clone();
    Terminal.info(&DeviceMessage::SelectedValue {
        value: provider.clone(),
    });
    warn_provider_preflight(&provider);

    update_config_provider(config)?;
    Ok(())
}

fn update_config_provider(config: &Config) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if !config_path.exists() {
        return Ok(());
    }
    update_config_provider_at(&config_path, config)
}

fn update_config_provider_at(config_path: &Path, config: &Config) -> anyhow::Result<()> {
    set_config_scalar(
        config_path,
        "recognition",
        "execution_provider",
        &config.recognition.execution_provider,
    )
}

/// True if `line` assigns `key` (`key = ...`), ignoring leading whitespace.
fn is_key_assignment(line: &str, key: &str) -> bool {
    line.trim_start()
        .strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// Set `key = "value"` inside `[section]`, appending the section if absent.
///
/// Line-based rather than a serde round-trip on purpose: the config file is
/// heavily commented and a re-serialize would discard every comment.
fn set_config_scalar(
    config_path: &Path,
    section: &str,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;

    let header = format!("[{section}]");
    let entry = format!("{key} = \"{value}\"\n");

    if content.contains(&header) {
        let mut new_content = String::new();
        let mut in_section = false;
        let mut written = false;

        for line in content.lines() {
            if line.trim() == header {
                in_section = true;
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }
            if in_section && is_key_assignment(line, key) {
                new_content.push_str(&entry);
                written = true;
                continue;
            }
            if in_section && line.starts_with('[') {
                if !written {
                    new_content.push_str(&entry);
                }
                in_section = false;
            }
            new_content.push_str(line);
            new_content.push('\n');
        }
        if in_section && !written {
            new_content.push_str(&entry);
        }
        write_file(config_path, new_content.as_bytes(), 0o644)?;
    } else {
        let mut content = content;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("\n{header}\n{entry}"));
        write_file(config_path, content.as_bytes(), 0o644)?;
    }

    Ok(())
}

fn wizard_model_download(theme: &ColorfulTheme, config: &Config) -> anyhow::Result<()> {
    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;

    let model_dir = Path::new(&config.daemon.model_dir);
    let configured_detector = &config.recognition.detector_model;
    let configured_embedder = &config.recognition.embedder_model;

    // Only download the models actually selected in the config.
    // Non-optional (default) models that aren't selected are skipped — the user
    // chose different models, so there is no point fetching the defaults too.
    let needed: Vec<&ModelEntry> = manifest
        .models
        .iter()
        .filter(|m| m.filename == *configured_detector || m.filename == *configured_embedder)
        .collect();

    // Check which models actually need downloading
    let mut to_download: Vec<&ModelEntry> = Vec::new();
    let mut already_present: Vec<&ModelEntry> = Vec::new();

    for entry in &needed {
        let model_path = model_dir.join(&entry.filename);
        match check_model(&model_path, &entry.sha256)? {
            ModelStatus::Present => already_present.push(entry),
            ModelStatus::Missing | ModelStatus::BadChecksum => to_download.push(entry),
        }
    }

    for entry in &already_present {
        Terminal.info(&DownloadMessage::ModelPresentOk {
            name: entry.name.clone(),
            purpose: entry.purpose.clone(),
        });
    }

    if to_download.is_empty() {
        Terminal.info(&DownloadMessage::AllModelsPresent);
        return Ok(());
    }

    let total_mb: u64 = to_download.iter().map(|e| e.size_mb).sum();
    Terminal.info(&DownloadMessage::ModelsToDownloadHeader);
    for entry in &to_download {
        Terminal.info(&DownloadMessage::ModelToDownloadEntry {
            name: entry.name.clone(),
            size_mb: entry.size_mb,
            purpose: entry.purpose.clone(),
        });
    }
    Terminal.info(&DownloadMessage::TotalDownloadSize { mb: total_mb });

    let proceed = Confirm::with_theme(theme)
        .with_prompt(DownloadMessage::ConfirmDownloadRequiredModels.localized())
        .default(true)
        .interact()?;

    if !proceed {
        Terminal.info(&DownloadMessage::SkippingModelDownload);
        return Ok(());
    }

    for entry in &to_download {
        let model_path = model_dir.join(&entry.filename);
        Terminal.info(&DownloadMessage::DownloadingModel {
            name: entry.name.clone(),
        });
        download_model(entry, &model_path)?;
        verify_after_download(&model_path, &entry.sha256, &entry.name)?;
        Terminal.info(&DownloadMessage::ModelDownloaded {
            name: entry.name.clone(),
        });
    }

    Ok(())
}

/// Detect the "orphaned models" situation: encrypted models exist in the DB,
/// but the relevant key file is missing and we're about to mint a new one.
/// Generating a fresh key would silently invalidate every existing model.
/// Offer to clear them; abort if the user declines.
///
/// `theme` is `None` when the caller must not prompt (non-interactive base), in
/// which case the situation is an error rather than a question.
fn handle_orphan_models_before_keygen(
    config: &Config,
    theme: Option<&ColorfulTheme>,
) -> anyhow::Result<()> {
    // Fail closed on both reads (C2, issue #105): a database that cannot be
    // opened, or a query that fails on an open one, does NOT mean "nothing to
    // protect" — it means facelock cannot tell, and minting a key on "cannot
    // tell" is what orphans real templates. `Absent` is the one failure class
    // that authorizes proceeding: no database file means there are no
    // templates to orphan, and the probe must not create one to prove it.
    let store = match crate::direct::open_store_existing(config) {
        Ok(s) => s,
        Err(facelock_store::StoreError::Absent { .. }) => return Ok(()),
        Err(e) => bail!(
            "refusing to generate a new encryption key: the face database could \
             not be read, so facelock cannot tell whether existing templates \
             would be orphaned ({e}). Fix access to {} and re-run, or clear \
             models first with: sudo facelock clear",
            config.storage.db_path
        ),
    };
    match store.has_any_models() {
        Ok(false) => return Ok(()),
        Ok(true) => {}
        Err(e) => bail!(
            "refusing to generate a new encryption key: could not determine \
             whether face models exist in {} ({e}), so facelock cannot tell \
             whether existing templates would be orphaned. Fix the database and \
             re-run, or clear models first with: sudo facelock clear",
            config.storage.db_path
        ),
    }

    // `notice`, not `info`: this is the context for the "Delete orphaned
    // models and continue?" confirmation below, and on the non-interactive
    // path it is the context for the refusal. A prompt whose subject `--quiet`
    // swallowed is a question with no question in it.
    Terminal.notice(&SetupMessage::OrphanModelsWarning {
        db_path: config.storage.db_path.clone(),
    });

    let Some(theme) = theme.filter(|_| is_interactive()) else {
        bail!(
            "orphaned encrypted models found and no encryption key present; \
             re-run setup interactively, or clear models first with: sudo facelock clear --yes"
        );
    };

    let clear = Confirm::with_theme(theme)
        .with_prompt("Delete orphaned models and continue?")
        .default(false)
        .interact()?;

    if !clear {
        bail!("encryption setup aborted; restore the previous key file or clear models manually");
    }

    let removed = store
        .clear_all()
        .map_err(|e| anyhow::anyhow!("failed to clear orphaned models: {e}"))?;
    Terminal.info(&SetupMessage::OrphanModelsRemoved { count: removed });
    Ok(())
}

/// Tests for the pre-keygen orphan guard (C2, issue #105). The guard must
/// fail closed on *both* reads: an unopenable database and a failing query on
/// an open one are equally "cannot tell whether templates exist", and neither
/// may be read as "nothing to protect".
#[cfg(test)]
mod orphan_guard_tests {
    use super::*;
    use std::path::Path;

    fn config_with_db(db_path: &Path) -> Config {
        let mut config = Config::parse("").expect("defaults parse");
        config.storage.db_path = db_path.to_string_lossy().into_owned();
        config
    }

    /// (a) Unreadable store: not a SQLite database at all. The guard must
    /// error out (so the caller never reaches keygen), not conclude "nothing
    /// to protect".
    #[test]
    fn unreadable_database_aborts_before_keygen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        std::fs::write(&db_path, b"this is not a sqlite database").unwrap();

        let err = handle_orphan_models_before_keygen(&config_with_db(&db_path), None).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("refusing to generate a new encryption key"),
            "guard must refuse keygen on an unreadable store: {chain}"
        );
    }

    /// (b) The store opens, but the models query fails. The injection — and
    /// the schema coupling it carries — lives in `facelock_test_support::
    /// schema_faults`, shared with the daemon's storage-failure test.
    #[test]
    fn failing_models_query_on_open_store_aborts_before_keygen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        drop(facelock_store::FaceStore::create(&db_path).unwrap());

        facelock_test_support::schema_faults::break_face_models_table(&db_path);

        let err = handle_orphan_models_before_keygen(&config_with_db(&db_path), None).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("could not determine whether face models exist"),
            "guard must refuse keygen when the query fails on an open store: {chain}"
        );
    }

    /// (c) A real database that holds zero models — the state after `facelock
    /// clear`, or after an enrollment that was rolled back. The guard opens
    /// it, gets `has_any_models() == Ok(false)`, and must let keygen proceed.
    ///
    /// The store is created and dropped deliberately: without it this test
    /// lands on the `Absent` early return instead, which is case (c′) below,
    /// and this arm — the only one that actually runs the query — would go
    /// uncovered.
    #[test]
    fn zero_models_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        drop(facelock_store::FaceStore::create(&db_path).unwrap());
        assert!(db_path.exists(), "the store under test must exist on disk");

        handle_orphan_models_before_keygen(&config_with_db(&db_path), None)
            .expect("a real store with zero models must not block keygen");
    }

    /// (c′) The `Absent` variant is what encodes case (c): the guard proceeds
    /// on a missing database *because there is provably nothing to orphan* —
    /// and, unlike the create-based probe it replaces, leaves no empty
    /// database behind. Together with (a) this pins Absent ≠ Denied/Corrupt:
    /// one authorizes keygen, the other refuses it.
    #[test]
    fn absent_database_proceeds_without_creating_one() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");

        handle_orphan_models_before_keygen(&config_with_db(&db_path), None)
            .expect("an absent database means nothing to orphan");
        assert!(
            !db_path.exists(),
            "the orphan probe must not create the database it reports absent"
        );
    }

    /// (d) Models present, non-interactive: the existing orphaned-models bail
    /// path, unchanged.
    #[test]
    fn models_present_non_interactive_bails_with_orphan_message() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        {
            let store = facelock_store::FaceStore::create(&db_path).unwrap();
            store
                .add_model("alice", "front", &[0.5f32; 512], "embedder")
                .unwrap();
        }

        let err = handle_orphan_models_before_keygen(&config_with_db(&db_path), None).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("orphaned encrypted models found"),
            "models present must keep the existing prompt/bail path: {chain}"
        );
    }
}

fn wizard_encryption_setup(theme: &ColorfulTheme, config: &mut Config) -> anyhow::Result<()> {
    Terminal.info(&SetupMessage::EncryptionIntro);

    // Detect TPM availability
    let tpm_available = detect_tpm(config);

    if tpm_available {
        let options = [
            "TPM-protected key (recommended) — AES key sealed by TPM hardware",
            "Software keyfile — AES key stored as plaintext file",
        ];
        let selection = Select::with_theme(theme)
            .with_prompt("Select encryption key protection")
            .items(&options[..])
            .default(0)
            .interact()?;

        if selection == 0 {
            return setup_encryption_tpm_key(config, Some(theme));
        }
    }

    setup_encryption_keyfile(config, Some(theme))
}

/// Seal an AES key with the TPM and switch the config to it. Callers must have
/// established that a TPM is usable.
fn setup_encryption_tpm_key(
    config: &mut Config,
    theme: Option<&ColorfulTheme>,
) -> anyhow::Result<()> {
    use facelock_core::config::EncryptionMethod;

    let sealed_path = Path::new(&config.encryption.sealed_key_path);
    if sealed_path.exists() {
        Terminal.info(&SetupMessage::TpmSealedKeyPresent {
            path: sealed_path.display().to_string(),
        });
    } else {
        handle_orphan_models_before_keygen(config, theme)?;
        Terminal.info(&SetupMessage::GeneratingTpmSealedKey);
        let pcr = if config.tpm.pcr_binding {
            Some(config.tpm.pcr_indices.as_slice())
        } else {
            None
        };
        #[cfg(feature = "tpm")]
        {
            let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                .context("failed to initialize TPM")?;
            facelock_tpm::generate_and_seal_key(&mut tpm, sealed_path, pcr)
                .context("failed to generate and seal key")?;
            Terminal.info(&SetupMessage::TpmSealedKeyWritten {
                path: sealed_path.display().to_string(),
            });
        }
        #[cfg(not(feature = "tpm"))]
        {
            let _ = pcr;
            anyhow::bail!("TPM support not compiled in (missing 'tpm' feature)");
        }
    }
    config.encryption.method = EncryptionMethod::Tpm;
    update_config_encryption(config, "tpm")?;
    Terminal.info(&SetupMessage::EncryptionEnabledTpm);
    Ok(())
}

/// Generate (or reuse) a software keyfile and switch the config to it.
fn setup_encryption_keyfile(
    config: &mut Config,
    theme: Option<&ColorfulTheme>,
) -> anyhow::Result<()> {
    use facelock_core::config::EncryptionMethod;

    let key_path = Path::new(&config.encryption.key_path);
    if key_path.exists() {
        Terminal.info(&SetupMessage::KeyfilePresent {
            path: key_path.display().to_string(),
        });
    } else {
        handle_orphan_models_before_keygen(config, theme)?;
        Terminal.info(&SetupMessage::GeneratingKeyfile);
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to generate encryption key")?;
        Terminal.info(&SetupMessage::KeyfileWritten {
            path: key_path.display().to_string(),
        });
    }

    config.encryption.method = EncryptionMethod::Keyfile;
    update_config_encryption(config, "keyfile")?;
    Terminal.info(&SetupMessage::EncryptionEnabledKeyfile);

    Ok(())
}

/// Turn embedding encryption off, loudly.
fn setup_encryption_none(config: &mut Config) -> anyhow::Result<()> {
    use facelock_core::config::EncryptionMethod;

    config.encryption.method = EncryptionMethod::None;
    update_config_encryption(config, "none")?;

    // `notice`: biometric templates are about to be stored in plaintext, and
    // that is not a line `--quiet` may take. Still stdout, so the bytes a
    // normal run prints are the ones it always printed.
    Terminal.notice(&SetupMessage::EncryptionDisabledWarning);
    Ok(())
}

/// True if the auto policy would mint a new key, and therefore needs the
/// orphaned-models guard first.
fn auto_encryption_needs_keygen(config: &Config, tpm_available: bool) -> bool {
    use facelock_core::config::EncryptionMethod;

    if config.encryption.method != EncryptionMethod::None {
        return false;
    }
    let key_path = if tpm_available {
        &config.encryption.sealed_key_path
    } else {
        &config.encryption.key_path
    };
    !Path::new(key_path).exists()
}

/// Apply `--encryption`. `theme` is `None` where prompting is not allowed.
///
/// Every branch that can mint a key goes through
/// [`handle_orphan_models_before_keygen`]: a fresh key silently invalidates
/// every already-enrolled model.
fn apply_encryption_choice(
    config: &mut Config,
    choice: EncryptionChoice,
    theme: Option<&ColorfulTheme>,
) -> anyhow::Result<()> {
    match choice {
        EncryptionChoice::Tpm => {
            if !detect_tpm(config) {
                bail!(
                    "--encryption=tpm requested but no usable TPM 2.0 was found (tcti: {}); \
                     refusing to fall back to a software keyfile. \
                     Pass --encryption=keyfile to accept a software key, \
                     or --encryption=auto to use the TPM only when present.",
                    config.tpm.tcti
                );
            }
            setup_encryption_tpm_key(config, theme)
        }
        EncryptionChoice::Keyfile => setup_encryption_keyfile(config, theme),
        EncryptionChoice::None => setup_encryption_none(config),
        EncryptionChoice::Auto => {
            if auto_encryption_needs_keygen(config, detect_tpm(config)) {
                handle_orphan_models_before_keygen(config, theme)?;
            }
            setup_encryption_auto(config)
        }
    }
}

/// Detect if TPM is available and functional.
fn detect_tpm(config: &Config) -> bool {
    // Check for TPM device
    let device_path = config
        .tpm
        .tcti
        .strip_prefix("device:")
        .unwrap_or(&config.tpm.tcti);
    if !Path::new(device_path).exists() {
        return false;
    }

    #[cfg(feature = "tpm")]
    {
        match facelock_tpm::TpmSealer::new(&config.tpm.tcti) {
            Ok(_) => {
                Terminal.info(&SetupMessage::TpmDetected);
                true
            }
            Err(e) => {
                tracing::debug!("TPM detected but not functional: {e}");
                false
            }
        }
    }

    #[cfg(not(feature = "tpm"))]
    {
        false
    }
}

/// Update only the encryption method in the config file (no key_path changes).
/// Used by `facelock tpm seal-key` / `unseal-key` for migration.
#[cfg_attr(not(feature = "tpm"), allow(dead_code))]
pub fn update_config_encryption_method(config: &Config, method: &str) -> anyhow::Result<()> {
    update_config_encryption(config, method)
}

/// Update the config file on disk with the chosen encryption method.
fn update_config_encryption(config: &Config, method: &str) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if !config_path.exists() {
        return Ok(()); // Config will be created later
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;

    // Check if [encryption] section already exists
    if content.contains("[encryption]") {
        // Update existing section
        let mut new_content = String::new();
        let mut in_encryption = false;
        let mut method_written = false;
        for line in content.lines() {
            if line.trim() == "[encryption]" {
                in_encryption = true;
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }
            if in_encryption && line.trim_start().starts_with("method") {
                new_content.push_str(&format!("method = \"{method}\"\n"));
                method_written = true;
                continue;
            }
            if in_encryption && line.starts_with('[') {
                if !method_written {
                    new_content.push_str(&format!("method = \"{method}\"\n"));
                }
                in_encryption = false;
            }
            new_content.push_str(line);
            new_content.push('\n');
        }
        if in_encryption && !method_written {
            new_content.push_str(&format!("method = \"{method}\"\n"));
        }
        write_file(&config_path, new_content.as_bytes(), 0o644)?;
    } else {
        // Append new section
        let mut content = content;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!(
            "\n[encryption]\nmethod = \"{method}\"\nkey_path = \"{}\"\n",
            config.encryption.key_path
        ));
        write_file(&config_path, content.as_bytes(), 0o644)?;
    }

    Ok(())
}

fn update_config_models(config: &Config) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if !config_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;

    let detector = &config.recognition.detector_model;
    let embedder = &config.recognition.embedder_model;
    let detector_sha = resolve_configured_model_sha256(
        &manifest,
        detector,
        config.recognition.detector_sha256.as_deref(),
    )?;
    let embedder_sha = resolve_configured_model_sha256(
        &manifest,
        embedder,
        config.recognition.embedder_sha256.as_deref(),
    )?;

    if content.contains("[recognition]") {
        let mut new_content = String::new();
        let mut in_recognition = false;
        let mut detector_written = false;
        let mut embedder_written = false;
        let mut detector_sha_written = false;
        let mut embedder_sha_written = false;

        for line in content.lines() {
            if line.trim() == "[recognition]" {
                in_recognition = true;
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }
            if in_recognition && line.trim_start().starts_with("detector_model") {
                new_content.push_str(&format!("detector_model = \"{detector}\"\n"));
                detector_written = true;
                continue;
            }
            if in_recognition && line.trim_start().starts_with("detector_sha256") {
                new_content.push_str(&format!("detector_sha256 = \"{detector_sha}\"\n"));
                detector_sha_written = true;
                continue;
            }
            if in_recognition && line.trim_start().starts_with("embedder_model") {
                new_content.push_str(&format!("embedder_model = \"{embedder}\"\n"));
                embedder_written = true;
                continue;
            }
            if in_recognition && line.trim_start().starts_with("embedder_sha256") {
                new_content.push_str(&format!("embedder_sha256 = \"{embedder_sha}\"\n"));
                embedder_sha_written = true;
                continue;
            }
            if in_recognition && line.starts_with('[') {
                if !detector_written {
                    new_content.push_str(&format!("detector_model = \"{detector}\"\n"));
                }
                if !detector_sha_written {
                    new_content.push_str(&format!("detector_sha256 = \"{detector_sha}\"\n"));
                }
                if !embedder_written {
                    new_content.push_str(&format!("embedder_model = \"{embedder}\"\n"));
                }
                if !embedder_sha_written {
                    new_content.push_str(&format!("embedder_sha256 = \"{embedder_sha}\"\n"));
                }
                in_recognition = false;
            }
            new_content.push_str(line);
            new_content.push('\n');
        }
        if in_recognition {
            if !detector_written {
                new_content.push_str(&format!("detector_model = \"{detector}\"\n"));
            }
            if !detector_sha_written {
                new_content.push_str(&format!("detector_sha256 = \"{detector_sha}\"\n"));
            }
            if !embedder_written {
                new_content.push_str(&format!("embedder_model = \"{embedder}\"\n"));
            }
            if !embedder_sha_written {
                new_content.push_str(&format!("embedder_sha256 = \"{embedder_sha}\"\n"));
            }
        }
        write_file(&config_path, new_content.as_bytes(), 0o644)?;
    } else {
        let mut content = content;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!(
            "\n[recognition]\ndetector_model = \"{detector}\"\ndetector_sha256 = \"{detector_sha}\"\nembedder_model = \"{embedder}\"\nembedder_sha256 = \"{embedder_sha}\"\n",
        ));
        write_file(&config_path, content.as_bytes(), 0o644)?;
    }

    Ok(())
}

fn resolve_configured_model_sha256(
    manifest: &ModelManifest,
    filename: &str,
    configured_sha256: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(entry) = manifest.find(filename) {
        if let Some(explicit) = configured_sha256
            && explicit != entry.sha256
        {
            anyhow::bail!("configured SHA256 for {filename} does not match bundled manifest");
        }
        return Ok(entry.sha256.clone());
    }

    if let Some(explicit) = configured_sha256 {
        if explicit.is_empty() {
            anyhow::bail!("custom model {filename} requires a non-empty SHA256");
        }
        return Ok(explicit.to_string());
    }

    anyhow::bail!("custom model {filename} requires an explicit SHA256 in config")
}

// ---------------------------------------------------------------------------
// Action step control flow (steps 6, 7, 8, 9)
//
// Each action step decides *whether it acts* before it does any I/O, so
// `--no-enroll` / `--no-systemd` / `--no-pam` are testable without a camera,
// systemd or root. Rule 2 of the plan: declining an action is not the same as
// letting it fall back to the default — declining PAM must configure nothing,
// not configure the candidate set's five `default_enabled` services.
// ---------------------------------------------------------------------------

/// Steps 7 and 8, decided up front. Step 8 exists only to exercise the
/// enrollment step 7 produced, so `--no-enroll` suppresses both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnrollSteps {
    /// Step 7 runs at all.
    enroll: bool,
    /// Step 7 proceeds without its confirmation prompt.
    assume_yes: bool,
}

fn enroll_steps_for(plan: &SetupPlan) -> EnrollSteps {
    match plan.enroll {
        // `--no-enroll` declines the action outright.
        Some(false) => EnrollSteps {
            enroll: false,
            assume_yes: false,
        },
        // `--enroll` answers the question, so the confirm is not asked.
        Some(true) => EnrollSteps {
            enroll: true,
            assume_yes: true,
        },
        // No flag: today's prompt, taking its default under `-y`.
        None => EnrollSteps {
            enroll: true,
            assume_yes: plan.yes,
        },
    }
}

/// Step 8 runs only when step 7 actually enrolled a face.
fn test_recognition_runs(steps: EnrollSteps, enrolled: bool) -> bool {
    steps.enroll && enrolled
}

/// What step 6 does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemdStep {
    /// No flag: prompt, then install if accepted.
    Ask,
    /// `--no-systemd`: declined. No unit files, no `systemctl`.
    Skip,
    /// `--systemd`: already answered — install without prompting.
    Install,
    /// `--systemd --disable`: already answered — disable without prompting.
    Disable,
}

/// Which [`SystemdStep`] a plan's `--systemd` / `--no-systemd` answer means.
///
/// `Install` and `Disable` used to collapse into one `Deferred` variant, on
/// the rule that [`run_with_plan`] applied both after the base flow. It now
/// applies them only when there is no base flow: inside one, the daemon has to
/// be configured before enrollment, so step 6 performs them and the two
/// answers need to be told apart.
fn systemd_step_for(plan: &SetupPlan) -> SystemdStep {
    match plan.systemd {
        SystemdPref::Ask => SystemdStep::Ask,
        SystemdPref::Skip => SystemdStep::Skip,
        SystemdPref::Install => SystemdStep::Install,
        SystemdPref::Disable => SystemdStep::Disable,
    }
}

/// What step 9 does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PamStep {
    /// No flag: today's multi-select over the detected candidates.
    Ask,
    /// `--pam [--service X] [--if-present]`: exactly this one service.
    /// Deliberately *not* the candidates' five `default_enabled` entries —
    /// `--pam` already means "sudo" in standalone mode, so a scripter gets one
    /// consistent meaning everywhere.
    ///
    /// `if_present` rides along because this arm is `--pam` under a *wizard*
    /// base — `setup --pam --service X --if-present --enroll` reaches step 9
    /// rather than [`run_with_plan`]'s alias call, and a flag the operator
    /// typed must not be decided differently by which other flags they typed.
    Install { service: String, if_present: bool },
    /// `--no-pam`: declined. Nothing under the PAM directory is written or
    /// backed up.
    Skip,
    /// `--pam --remove`: [`run_with_plan`] performs the removal.
    Deferred,
}

impl PamStep {
    /// Whether step 9 modifies anything under the PAM directory.
    fn touches_pam_d(&self) -> bool {
        matches!(self, PamStep::Ask | PamStep::Install { .. })
    }
}

fn pam_step_for(plan: &SetupPlan) -> PamStep {
    match &plan.pam {
        PamPref::Ask => PamStep::Ask,
        PamPref::Skip => PamStep::Skip,
        PamPref::Install {
            service,
            if_present,
        } => PamStep::Install {
            service: service
                .clone()
                .unwrap_or_else(|| DEFAULT_PAM_SERVICE.to_string()),
            if_present: *if_present,
        },
        PamPref::Remove { .. } => PamStep::Deferred,
    }
}

/// A step's yes/no confirmation, whose default is always "yes".
///
/// `assume_yes` — from `--enroll` (which answers the question) or `-y` (which
/// suppresses confirmations, §2.1) — takes that default without prompting.
fn confirm_step(theme: &ColorfulTheme, prompt: &str, assume_yes: bool) -> anyhow::Result<bool> {
    if assume_yes {
        return Ok(true);
    }
    Ok(Confirm::with_theme(theme)
        .with_prompt(prompt)
        .default(true)
        .interact()?)
}

fn wizard_face_enroll(
    config: &Config,
    theme: &ColorfulTheme,
    assume_yes: bool,
) -> anyhow::Result<bool> {
    let proceed = confirm_step(
        theme,
        &SetupMessage::ConfirmEnrollNow.localized(),
        assume_yes,
    )?;

    if !proceed {
        Terminal.info(&SetupMessage::EnrollSkipped);
        return Ok(false);
    }

    super::enroll::run(config, None, None, true)?;
    Ok(true)
}

fn wizard_test_recognition(
    config: &Config,
    theme: &ColorfulTheme,
    assume_yes: bool,
) -> anyhow::Result<()> {
    let proceed = confirm_step(
        theme,
        &SetupMessage::ConfirmTestRecognition.localized(),
        assume_yes,
    )?;

    if !proceed {
        Terminal.info(&SetupMessage::TestSkipped);
        return Ok(());
    }

    super::test_cmd::run(config, None)?;
    Ok(())
}

fn wizard_systemd_setup(theme: &ColorfulTheme, assume_yes: bool) -> anyhow::Result<bool> {
    if !Path::new("/run/systemd/system").exists() {
        Terminal.info(&SystemMessage::SystemdNotDetected);
        return Ok(false);
    }

    let proceed = confirm_step(
        theme,
        &SystemMessage::ConfirmDaemonMode.localized(),
        assume_yes,
    )?;

    if !proceed {
        Terminal.info(&SystemMessage::SystemdDeclined);
        return Ok(false);
    }

    run_systemd(false)?;
    Ok(true)
}

/// Step 9, parameterized on the PAM directory so that "declining PAM leaves
/// every file byte-identical" is testable against a tempdir.
///
/// `module_present` is the caller's answer to "is `pam_facelock.so` installed?".
/// It is hoisted out of the writer so the check happens once, before any
/// prompt or write, and so tests can drive the write path on a machine that has
/// no PAM module installed.
fn pam_step_in(
    dirs: &PamDirs,
    plan: &SetupPlan,
    theme: &ColorfulTheme,
    module_present: bool,
) -> anyhow::Result<Vec<String>> {
    let step = pam_step_for(plan);

    if !step.touches_pam_d() {
        if step == PamStep::Skip {
            Terminal.info(&PamMessage::PamSkippedFlag {
                dir: dirs.display(),
            });
        }
        return Ok(Vec::new());
    }

    if !module_present {
        Terminal.info(&PamMessage::PamModuleMissing {
            paths: PAM_MODULE_PATHS.join(", "),
        });
        return Ok(Vec::new());
    }

    match step {
        PamStep::Install {
            service,
            if_present,
        } => {
            Terminal.info(&PamMessage::ConfiguringPamFor {
                service: service.clone(),
            });
            // Step 9 runs only under a wizard base, never
            // `--non-interactive`, so its prompt knob reduces to `plan.yes`.
            // Sensitive authorization stays an independent decision carried
            // by `plan.allow_sensitive`.
            //
            // The returned bool, not the absence of an `Err`, decides whether
            // this service is named in the closing summary and whether the
            // hyprlock handoff fires. Under `--if-present` an absent service
            // is a success that configured nothing, and reporting it as
            // configured would have offered to wire `hyprlock.conf` up to a
            // PAM service that has no facelock line.
            let configured = super::pam::install_one_in(
                dirs,
                &pam_request(PamAction::Add, &service, setup_pam_knobs(plan), if_present),
            )?;
            Ok(if configured {
                vec![service]
            } else {
                Vec::new()
            })
        }
        PamStep::Ask => wizard_pam_setup_in(dirs, theme),
        // Both returned above; repeated here to keep the match exhaustive.
        PamStep::Skip | PamStep::Deferred => Ok(Vec::new()),
    }
}

fn wizard_pam_setup_in(dirs: &PamDirs, theme: &ColorfulTheme) -> anyhow::Result<Vec<String>> {
    let candidates = candidates_in(dirs);

    if candidates.is_empty() {
        Terminal.info(&PamMessage::NoPamCandidates {
            dir: dirs.display(),
        });
        return Ok(Vec::new());
    }

    Terminal.info(&PamMessage::PamLinePreview {
        line: PAM_LINE.to_string(),
    });

    let labels: Vec<&str> = candidates.iter().map(|c| c.description).collect();
    let defaults: Vec<bool> = candidates.iter().map(|c| c.default_enabled).collect();

    // The multi-select is the only thing that can select a service here, and
    // it needs a terminal. There used to be a `!is_interactive()` arm that
    // auto-selected every `default_enabled` candidate instead — hyprlock,
    // swaylock, kscreenlocker_greet and lightdm, written with no consent from
    // anyone. It could not fire in production (`run_with_plan` demotes a
    // non-TTY wizard base to `run_non_interactive` under the same
    // `is_interactive()` test, so `run_wizard` — this function's only caller —
    // never runs headless), and the one test that reached it did so by calling
    // step 9 directly.
    let selections = MultiSelect::with_theme(theme)
        .with_prompt(PamMessage::PromptSelectPamServices.localized())
        .items(&labels)
        .defaults(&defaults)
        .interact()?;
    let selected_services: Vec<String> = selections
        .into_iter()
        .map(|i| candidates[i].service.to_string())
        .collect();

    let mut configured = Vec::new();
    for service in selected_services {
        Terminal.info(&PamMessage::ConfiguringPamFor {
            service: service.clone(),
        });
        // The multi-select above *is* the per-service consent, so no
        // confirmation is asked for again; no candidate is in
        // SENSITIVE_SERVICES (`no_candidate_is_a_sensitive_service`), so the
        // gate is moot either way.
        let knobs = PamKnobs {
            no_confirm: true,
            allow_sensitive: true,
        };
        // `false`, not the plan's `--if-present`: the multi-select only ever
        // offers services whose file it just found, so there is no absence to
        // forgive, and these are not the service the operator named.
        match super::pam::install_one_in(dirs, &pam_request(PamAction::Add, &service, knobs, false))
        {
            Ok(true) => configured.push(service),
            // Unreachable as written — the multi-select offers only services
            // whose file it just found, and `no_confirm` suppresses the
            // decline — but the list must carry what was configured, not what
            // was attempted.
            Ok(false) => {}
            Err(e) => {
                Terminal.info(&PamMessage::PamConfigureFailed {
                    service: service.clone(),
                    error: e.to_string(),
                });
            }
        }
    }

    if configured.is_empty() {
        Terminal.info(&PamMessage::NoPamServicesSelected);
    }

    Ok(configured)
}

// ---------------------------------------------------------------------------
// Hyprlock integration handoff
// ---------------------------------------------------------------------------

fn print_hyprlock_hint() {
    Terminal.info(&SetupMessage::HyprlockHint);
}

fn wizard_hyprlock_handoff(theme: &ColorfulTheme) {
    // The wizard runs as root via `sudo facelock setup`, but hyprlock.conf lives
    // in the invoking user's $HOME — only `runuser -u $SUDO_USER` can touch it.
    let sudo_user = match std::env::var("SUDO_USER") {
        Ok(u) if !u.is_empty() && u != "root" => u,
        _ => {
            print_hyprlock_hint();
            return;
        }
    };

    let user = match nix::unistd::User::from_name(&sudo_user) {
        Ok(Some(u)) => u,
        _ => {
            print_hyprlock_hint();
            return;
        }
    };

    let hyprlock_conf = user.dir.join(".config").join("hypr").join("hyprlock.conf");
    if !hyprlock_conf.exists() {
        print_hyprlock_hint();
        return;
    }

    Terminal.info(&SetupMessage::BlankLine);
    let proceed = Confirm::with_theme(theme)
        .with_prompt(format!(
            "Apply hyprlock face-unlock integration for {sudo_user}? (face icon + empty-Enter submit)"
        ))
        .default(true)
        .interact()
        .unwrap_or(false);

    if !proceed {
        print_hyprlock_hint();
        return;
    }

    let status = Command::new("runuser")
        .args(["-u", &sudo_user, "--", "facelock", "hyprlock", "enable"])
        .status();

    match status {
        Ok(s) if s.success() => {
            Terminal.info(&SetupMessage::HyprlockApplied { user: sudo_user });
        }
        _ => {
            print_hyprlock_hint();
        }
    }
}

// ---------------------------------------------------------------------------
// Non-interactive setup (original behavior)
// ---------------------------------------------------------------------------

fn run_non_interactive(plan: &SetupPlan) -> anyhow::Result<()> {
    Terminal.info(&SetupMessage::NonInteractivePreparing);

    // Load config (or use defaults for paths). Deliberate load (D7): setup
    // bootstraps the config file, which may not exist yet.
    let mut config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not load config ({e}), using default paths");
            create_default_config()?;
            Config::load().context("failed to load config after creating default")?
        }
    };

    // 1. Create directories
    create_directories(&config)?;
    ensure_state_layout_or_bail(&config)?;

    // 1b. Choice flags override the config. This has to happen before step 3:
    //     `--models` decides which models are downloaded. None of these paths
    //     prompt — an unresolvable value is an error, never a hang.
    if let Some(choice) = plan.camera.as_ref() {
        apply_camera_choice(&mut config, choice)?;
    }
    if let Some(preset) = plan.models {
        apply_model_preset(&mut config, preset)?;
    }
    if let Some(provider) = plan.execution_provider {
        apply_execution_provider(&mut config, provider)?;
    }

    // 2. Parse model manifest
    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;

    let model_dir = Path::new(&config.daemon.model_dir);

    // 3. Check and download only the models actually selected in the config.
    //    Non-optional (default) models that aren't referenced by the config are
    //    skipped — if the user chose different models there is no reason to fetch
    //    the defaults as well.  On a first run the config defaults resolve to the
    //    standard models (scrfd_2.5g + w600k_r50) so those are still downloaded.
    let configured_detector = &config.recognition.detector_model;
    let configured_embedder = &config.recognition.embedder_model;

    let needed: Vec<&ModelEntry> = manifest
        .models
        .iter()
        .filter(|m| m.filename == *configured_detector || m.filename == *configured_embedder)
        .collect();

    Terminal.info(&SetupMessage::CheckingModels {
        count: needed.len(),
    });

    for entry in &needed {
        let model_path = model_dir.join(&entry.filename);
        let status = check_model(&model_path, &entry.sha256)?;

        match status {
            ModelStatus::Present => {
                Terminal.info(&DownloadMessage::ModelPresentOk {
                    name: entry.name.clone(),
                    purpose: entry.purpose.clone(),
                });
            }
            ModelStatus::Missing => {
                Terminal.info(&DownloadMessage::ModelDownloadPending {
                    name: entry.name.clone(),
                    size_mb: entry.size_mb,
                    purpose: entry.purpose.clone(),
                });
                download_model(entry, &model_path)?;
                verify_after_download(&model_path, &entry.sha256, &entry.name)?;
                Terminal.info(&DownloadMessage::ModelDownloaded {
                    name: entry.name.clone(),
                });
            }
            ModelStatus::BadChecksum => {
                Terminal.info(&DownloadMessage::ModelRedownloading {
                    name: entry.name.clone(),
                });
                download_model(entry, &model_path)?;
                verify_after_download(&model_path, &entry.sha256, &entry.name)?;
                Terminal.info(&DownloadMessage::ModelRedownloaded {
                    name: entry.name.clone(),
                });
            }
        }
    }

    // 4. Configure encryption. `--encryption` answers the question; without it
    //    the auto policy runs, exactly as before.
    match plan.encryption {
        // No theme: nothing here may prompt under a non-interactive base.
        Some(choice) => apply_encryption_choice(&mut config, choice, None)?,
        None => setup_encryption_auto(&config)?,
    }

    secure_setup_paths(&config, Some(&manifest))?;
    write_setup_marker()?;

    // Daemon configuration, before enrollment and for the same reason the
    // wizard puts step 6 there: `enroll::run` selects its transport at entry,
    // so a daemon installed after it is never the one it used. `Ask` and
    // `Skip` mean "do nothing here", exactly as before — this flow has never
    // prompted about systemd.
    match plan.systemd {
        SystemdPref::Install => {
            run_systemd(false)?;
            start_daemon_for_setup(&config);
        }
        SystemdPref::Disable => run_systemd(true)?,
        SystemdPref::Ask | SystemdPref::Skip => {}
    }

    // Enrollment only happens here when it was explicitly asked for: `None` and
    // `--no-enroll` both mean "do nothing", which is what this flow has always
    // done. `--enroll` runs unattended — `enroll::run` prompts for nothing once
    // the setup marker exists.
    let enrolled = plan.enroll == Some(true);
    if enrolled {
        Terminal.info(&SetupMessage::EnrollingFace);
        super::enroll::run(&config, None, None, true)?;
    }

    // See the matching call in `run_wizard`.
    if let Err(e) = super::enrollment_marker::reconcile_all(&config) {
        tracing::warn!("could not reconcile enrollment markers: {e}");
    }

    if super::pam::is_configured(&PamDirs::system(), "hyprlock") {
        print_hyprlock_hint();
    }

    if enrolled {
        Terminal.info(&SetupMessage::SetupCompleteShort);
    } else {
        Terminal.info(&SetupMessage::SetupCompleteEnroll);
    }
    Ok(())
}

/// Auto-configure encryption in non-interactive mode.
/// Prefers TPM-sealed key if TPM is available, falls back to keyfile.
fn setup_encryption_auto(config: &Config) -> anyhow::Result<()> {
    use facelock_core::config::EncryptionMethod;

    // Skip if already configured
    if config.encryption.method != EncryptionMethod::None {
        Terminal.info(&SetupMessage::EncryptionAlreadyConfigured {
            // Pre-formatted: `Debug` here is the config enum's own spelling
            // (`Tpm`/`Keyfile`), and formatting before the seam keeps the
            // rendering locale-independent.
            method: format!("{:?}", config.encryption.method),
        });
        return Ok(());
    }

    // Try TPM first
    if detect_tpm(config) {
        #[cfg(feature = "tpm")]
        {
            let sealed_path = Path::new(&config.encryption.sealed_key_path);
            if !sealed_path.exists() {
                let pcr = if config.tpm.pcr_binding {
                    Some(config.tpm.pcr_indices.as_slice())
                } else {
                    None
                };
                let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                    .context("failed to initialize TPM")?;
                facelock_tpm::generate_and_seal_key(&mut tpm, sealed_path, pcr)
                    .context("failed to generate and seal key")?;
                Terminal.info(&SetupMessage::GeneratedTpmKeyAt {
                    path: sealed_path.display().to_string(),
                });
            }
            let mut config = config.clone();
            config.encryption.method = EncryptionMethod::Tpm;
            update_config_encryption(&config, "tpm")?;
            Terminal.info(&SetupMessage::EncryptionEnabledTpmAuto);
            return Ok(());
        }
    }

    // Fall back to keyfile
    let key_path = Path::new(&config.encryption.key_path);
    if !key_path.exists() {
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to generate encryption key")?;
        Terminal.info(&SetupMessage::GeneratedKeyfileAt {
            path: key_path.display().to_string(),
        });
    }

    let mut config = config.clone();
    config.encryption.method = EncryptionMethod::Keyfile;
    update_config_encryption(&config, "keyfile")?;
    Terminal.info(&SetupMessage::EncryptionEnabledKeyfileAuto);
    Ok(())
}

// ---------------------------------------------------------------------------
// legacy facelock system group
// ---------------------------------------------------------------------------

/// Remove a `facelock` group left behind by an older install, best-effort.
///
/// ADR 010 retired the group: the bus policy no longer names it and packaging
/// no longer creates it. This runs at the end of `secure_setup_paths`, once
/// every path setup owns has converged to `root:root`, so nothing under
/// `/var/lib/facelock` is group-owned by the time the group goes away;
/// `/run/facelock` converges through tmpfiles at the next boot or through the
/// package scriptlets. `groupdel` fails only when the group is some account's
/// primary group, which facelock never did; a failure is reported, never
/// fatal.
fn retire_facelock_group() {
    match nix::unistd::Group::from_name("facelock") {
        Ok(Some(_)) => match run_cmd("groupdel", &["facelock"]) {
            Ok(()) => Terminal.info(&SystemMessage::RetiredFacelockGroup),
            Err(e) => tracing::warn!("could not remove the legacy facelock group: {e}"),
        },
        Ok(None) => {}
        Err(e) => tracing::warn!("could not look up the facelock group: {e}"),
    }
}

/// Tighten an existing directory to `mode` and, when running as root, make it
/// `root:root`. Every directory setup secures is root-owned by construction.
fn secure_dir_if_exists(path: &Path, mode: u32, is_root: bool) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if !path.is_dir() {
        bail!(
            "expected directory but found non-directory path: {}",
            path.display()
        );
    }

    apply_dir(path, mode, is_root)
}

fn secure_setup_paths(config: &Config, manifest: Option<&ModelManifest>) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/facelock"));
    let audit_path = Path::new(&config.audit.path);
    let key_path = Path::new(&config.encryption.key_path);
    let sealed_key_path = Path::new(&config.encryption.sealed_key_path);
    // Every path setup secures is root-owned by construction: enforce that when
    // running as root, apply modes alone otherwise.
    let is_root = nix::unistd::Uid::current().is_root();

    secure_dir_if_exists(config_dir, 0o755, is_root)?;
    // Snapshots hold raw face images: root-only, no group access.
    secure_dir_if_exists(Path::new(&config.snapshots.dir), 0o700, is_root)?;

    // The state directory subtree — state dir, models/, enrolled/, and the
    // database file with its -wal/-shm sidecars — is owned by `state_layout`.
    // Re-applied here so a path created earlier in setup with a looser mode
    // converges before the marker reconcile runs.
    ensure_state_layout_or_bail(config)?;
    if let Some(parent) = audit_path.parent() {
        // Per-user auth history: root-only, like the snapshots.
        secure_dir_if_exists(parent, 0o700, is_root)?;
    }
    if let Some(parent) = key_path.parent() {
        secure_dir_if_exists(parent, 0o755, is_root)?;
    }
    if let Some(parent) = sealed_key_path.parent() {
        secure_dir_if_exists(parent, 0o755, is_root)?;
    }

    apply_file(&config_path, 0o644, is_root)?;
    apply_file(audit_path, 0o600, is_root)?;
    apply_file(key_path, 0o600, is_root)?;
    apply_file(sealed_key_path, 0o600, is_root)?;
    apply_file(Path::new(SETUP_COMPLETE_MARKER), 0o644, is_root)?;

    if let Some(manifest) = manifest {
        for entry in &manifest.models {
            let model_path = Path::new(&config.daemon.model_dir).join(&entry.filename);
            apply_file(&model_path, 0o644, is_root)?;
        }
    }

    // ADR 010: everything above is root:root now, so a facelock group left by
    // an older install owns nothing; remove it (best-effort, reported).
    retire_facelock_group();

    Ok(())
}

/// Apply the state-directory layout, turning a failure into a fatal setup
/// error.
///
/// Setup is the one place a user is watching, so a failure to create or
/// secure the state directory must stop here with its message intact rather
/// than being logged and stepped over.
fn ensure_state_layout_or_bail(config: &Config) -> anyhow::Result<()> {
    crate::state_layout::ensure_state_layout(config)
        .context("failed to prepare the facelock state directory")
}

fn create_directories(config: &Config) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    let mut dirs: Vec<(&Path, u32)> = vec![
        (
            Path::new(&config.daemon.model_dir),
            crate::state_layout::MODELS_DIR_MODE,
        ),
        // Snapshots hold raw face images: root-only.
        (Path::new(&config.snapshots.dir), 0o700),
    ];

    for (path, mode) in [
        (&config.storage.db_path, crate::state_layout::STATE_DIR_MODE),
        // Per-user auth history: root-only.
        (&config.audit.path, 0o700),
        (&config.encryption.key_path, 0o755),
        (&config.encryption.sealed_key_path, 0o755),
    ] {
        if let Some(parent) = Path::new(path.as_str()).parent() {
            dirs.push((parent, mode));
        }
    }

    if let Some(parent) = config_path.parent() {
        dirs.push((parent, 0o755));
    }

    for (dir, mode) in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }

        ensure_private_dir(dir, mode)
            .with_context(|| format!("failed to create directory {}", dir.display()))?;
        tracing::debug!("ensured directory: {}", dir.display());
    }

    Terminal.info(&SetupMessage::DirectoriesCreated);
    Ok(())
}

fn create_default_config() -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if config_path.exists() {
        return Ok(());
    }

    if let Some(parent) = config_path.parent() {
        ensure_private_dir(parent, 0o755).context("failed to create config directory")?;
    }

    let default_config = r#"[device]
path = "/dev/video0"
"#;
    write_file(&config_path, default_config.as_bytes(), 0o644).with_context(|| {
        format!(
            "failed to write default config to {}",
            config_path.display()
        )
    })?;
    Terminal.info(&SetupMessage::CreatedDefaultConfig {
        path: config_path.display().to_string(),
    });
    Ok(())
}

enum ModelStatus {
    Present,
    Missing,
    BadChecksum,
}

fn check_model(path: &Path, expected_sha256: &str) -> anyhow::Result<ModelStatus> {
    if !path.exists() {
        return Ok(ModelStatus::Missing);
    }

    // If no checksum configured, accept any existing file
    if expected_sha256.is_empty() {
        return Ok(ModelStatus::Present);
    }

    let data = fs::read(path).context("failed to read model file")?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    // Encode the digest by iterating bytes so this works with both sha2 0.10
    // (`GenericArray`) and sha2 0.11 (`hybrid_array::Array`), since only the
    // older type implements `LowerHex` directly.
    let hex: String = result.iter().map(|b| format!("{b:02x}")).collect();

    if hex == expected_sha256 {
        Ok(ModelStatus::Present)
    } else {
        Ok(ModelStatus::BadChecksum)
    }
}

fn download_model(entry: &ModelEntry, dest: &Path) -> anyhow::Result<()> {
    if entry.url.is_empty() {
        bail!("no download URL configured for {}", entry.name);
    }
    let url = entry.url.as_str();

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .context("failed to create HTTP client")?;

    let mut response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to download {}", entry.name))?;

    if !response.status().is_success() {
        bail!(
            "download failed for {}: HTTP {}",
            entry.name,
            response.status()
        );
    }

    let total_size = response
        .content_length()
        .unwrap_or(entry.size_mb * 1024 * 1024);

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::with_template(
            "    {spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .expect("valid template")
        .progress_chars("#>-"),
    );

    // Write atomically: write to temp file first, then rename
    let tmp_path = dest.with_extension("tmp");
    let mut file = create_truncate_file(&tmp_path, 0o644)
        .with_context(|| format!("failed to create {}", tmp_path.display()))?;

    let mut downloaded: u64 = 0;
    let mut buffer = vec![0u8; 8192];
    loop {
        use std::io::Read as _;
        let n = response
            .read(&mut buffer)
            .context("failed to read response")?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])
            .context("failed to write to temp file")?;
        downloaded += n as u64;
        pb.set_position(downloaded);
    }
    pb.finish_and_clear();
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp_path, dest)
        .with_context(|| format!("failed to rename temp file to {}", dest.display()))?;
    ensure_mode(dest, 0o644).with_context(|| format!("failed to secure {}", dest.display()))?;

    Ok(())
}

// --- PAM candidate detection ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PamCategory {
    PrivilegeEscalation, // sudo, polkit-1
    LockScreen,          // hyprlock, swaylock, kscreenlocker_greet
    DisplayManager,      // gdm-password, sddm, lightdm
}

#[derive(Clone, Debug)]
struct PamCandidate {
    service: &'static str,
    #[allow(dead_code)] // retained for documentation and future category-based filtering
    category: PamCategory,
    description: &'static str,
    default_enabled: bool,
}

// Intentionally excluded from PAM_CANDIDATES:
//   - the shared stacks, under whichever name the distribution uses: system-auth
//     and password-auth (Fedora/RHEL, Arch), the -ac spellings authconfig left
//     behind, common-auth (Debian), system-login (Arch). Editing one gives face
//     auth to passwd, su, chsh and the display manager at once — risky. They are
//     SENSITIVE_SERVICES, and `no_candidate_is_a_sensitive_service` pins that
//     nothing here is on that list.
//   - login: TTY login. Cameras often aren't initialized at boot.
//   - su, passwd, chsh, chfn: privilege/credential-change tools that should
//     require a real password.
//   - any unknown service: detection is by the service file existing anywhere on
//     the PAM search path — /etc/pam.d, then the vendor directories, which is
//     where polkit-1 ships on current Arch — but only services in
//     PAM_CANDIDATES are offered.
const PAM_CANDIDATES: &[PamCandidate] = &[
    PamCandidate {
        service: "sudo",
        category: PamCategory::PrivilegeEscalation,
        description: "sudo (privilege escalation)",
        default_enabled: true,
    },
    PamCandidate {
        service: "polkit-1",
        category: PamCategory::PrivilegeEscalation,
        description: "polkit-1 (GUI privilege escalation prompts)",
        default_enabled: true,
    },
    PamCandidate {
        service: "hyprlock",
        category: PamCategory::LockScreen,
        description: "hyprlock (Hyprland screen lock)",
        default_enabled: true,
    },
    PamCandidate {
        service: "swaylock",
        category: PamCategory::LockScreen,
        description: "swaylock (Sway screen lock)",
        default_enabled: true,
    },
    PamCandidate {
        service: "kscreenlocker_greet",
        category: PamCategory::LockScreen,
        description: "KDE Plasma screen lock",
        default_enabled: true,
    },
    // Opt-in, unlike the other lock screens: this is an Omarchy-specific service
    // file, so leave the choice to users who know they have it.
    PamCandidate {
        service: "omarchy-lock-face",
        category: PamCategory::LockScreen,
        description: "omarchy-lock-face (Omarchy face-unlock screen lock)",
        default_enabled: false,
    },
    PamCandidate {
        service: "gdm-password",
        category: PamCategory::DisplayManager,
        description: "GDM login screen (GNOME) (experimental)",
        default_enabled: false,
    },
    PamCandidate {
        service: "sddm",
        category: PamCategory::DisplayManager,
        description: "SDDM login screen (KDE) (experimental)",
        default_enabled: false,
    },
    PamCandidate {
        service: "lightdm",
        category: PamCategory::DisplayManager,
        description: "LightDM login screen (Ubuntu/Xfce/Mint)",
        default_enabled: true,
    },
];

/// Returns candidates from `PAM_CANDIDATES` whose service file exists anywhere
/// on the PAM search path.
///
/// Through the writer's own resolver rather than a `join(...).exists()` here:
/// the menu has to offer exactly what `pam add` can configure, and those two
/// answers drifting apart is what hid `polkit-1` from the wizard on every Arch
/// box where the file moved to `/usr/lib/pam.d`.
fn candidates_in(dirs: &PamDirs) -> Vec<&'static PamCandidate> {
    PAM_CANDIDATES
        .iter()
        .filter(|c| super::pam::service_exists(dirs, c.service))
        .collect()
}

fn verify_after_download(path: &Path, expected_sha256: &str, name: &str) -> anyhow::Result<()> {
    if expected_sha256.is_empty() {
        return Ok(());
    }

    let data = fs::read(path).context("failed to read downloaded model")?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    // Encode the digest by iterating bytes so this works with both sha2 0.10
    // (`GenericArray`) and sha2 0.11 (`hybrid_array::Array`), since only the
    // older type implements `LowerHex` directly.
    let hex: String = result.iter().map(|b| format!("{b:02x}")).collect();

    if hex != expected_sha256 {
        // Remove the bad file
        fs::remove_file(path).ok();
        bail!("SHA256 verification failed for {name}: expected {expected_sha256}, got {hex}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// systemd unit installation
// ---------------------------------------------------------------------------

const SYSTEMD_UNIT_DIR: &str = "/usr/lib/systemd/system";
const SERVICE_FILENAME: &str = "facelock-daemon.service";
const DBUS_SYSTEM_SERVICES_DIR: &str = "/usr/share/dbus-1/system-services";
const DBUS_SYSTEM_CONF_DIR: &str = "/usr/share/dbus-1/system.d";
const DBUS_SERVICE_FILENAME: &str = "org.facelock.Daemon.service";
const DBUS_POLICY_FILENAME: &str = "org.facelock.Daemon.conf";
const LEGACY_SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/facelock-daemon.service";
const LEGACY_DBUS_SYSTEM_SERVICE_PATH: &str =
    "/etc/dbus-1/system-services/org.facelock.Daemon.service";
const LEGACY_DBUS_SYSTEM_CONF_PATH: &str = "/etc/dbus-1/system.d/org.facelock.Daemon.conf";

fn check_systemd() -> anyhow::Result<()> {
    if !Path::new("/run/systemd/system").exists() {
        bail!("systemd not found — use manual daemon management or oneshot mode");
    }
    Ok(())
}

fn check_root() -> anyhow::Result<()> {
    let uid = unsafe { libc::getuid() };
    if uid != 0 {
        bail!("this command must be run as root (try: sudo facelock setup --systemd)");
    }
    Ok(())
}

fn run_cmd(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{program} {} failed: {stderr}", args.join(" "));
    }
    Ok(())
}

fn refresh_legacy_copy_if_present(path: &Path, contents: &str, marker: &str) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let existing = fs::read_to_string(path)
        .with_context(|| format!("failed to read existing legacy file {}", path.display()))?;
    if !existing.contains(marker) {
        return Ok(());
    }

    write_file(path, contents.as_bytes(), 0o644)
        .with_context(|| format!("failed to refresh {}", path.display()))?;
    Terminal.info(&SystemMessage::RefreshedLegacyFile {
        path: path.display().to_string(),
    });
    Ok(())
}

pub fn run_systemd(disable: bool) -> anyhow::Result<()> {
    check_root()?;
    check_systemd()?;

    if disable {
        Terminal.info(&SystemMessage::DisablingSystemdUnits);
        run_cmd("systemctl", &["disable", "--now", "facelock-daemon"])?;
        Terminal.info(&SystemMessage::SystemdUnitsDisabled);
    } else {
        Terminal.info(&SystemMessage::InstallingSystemdUnits);

        // Install systemd service unit
        let unit_dir = Path::new(SYSTEMD_UNIT_DIR);
        fs::create_dir_all(unit_dir)
            .with_context(|| format!("failed to create {SYSTEMD_UNIT_DIR}"))?;

        let service_path = unit_dir.join(SERVICE_FILENAME);
        write_file(&service_path, SERVICE_UNIT.as_bytes(), 0o644)
            .with_context(|| format!("failed to write {}", service_path.display()))?;
        Terminal.info(&SystemMessage::WroteFile {
            path: service_path.display().to_string(),
        });
        refresh_legacy_copy_if_present(
            Path::new(LEGACY_SYSTEMD_UNIT_PATH),
            SERVICE_UNIT,
            "ExecStart=/usr/bin/facelock daemon",
        )?;

        // Install D-Bus policy file
        let conf_dir = Path::new(DBUS_SYSTEM_CONF_DIR);
        fs::create_dir_all(conf_dir)
            .with_context(|| format!("failed to create {DBUS_SYSTEM_CONF_DIR}"))?;

        let policy_path = conf_dir.join(DBUS_POLICY_FILENAME);
        write_file(&policy_path, DBUS_POLICY.as_bytes(), 0o644)
            .with_context(|| format!("failed to write {}", policy_path.display()))?;
        Terminal.info(&SystemMessage::WroteFile {
            path: policy_path.display().to_string(),
        });
        refresh_legacy_copy_if_present(
            Path::new(LEGACY_DBUS_SYSTEM_CONF_PATH),
            DBUS_POLICY,
            "org.facelock.Daemon",
        )?;

        // Install D-Bus activation service
        let svc_dir = Path::new(DBUS_SYSTEM_SERVICES_DIR);
        fs::create_dir_all(svc_dir)
            .with_context(|| format!("failed to create {DBUS_SYSTEM_SERVICES_DIR}"))?;

        let dbus_svc_path = svc_dir.join(DBUS_SERVICE_FILENAME);
        write_file(&dbus_svc_path, DBUS_SERVICE.as_bytes(), 0o644)
            .with_context(|| format!("failed to write {}", dbus_svc_path.display()))?;
        Terminal.info(&SystemMessage::WroteFile {
            path: dbus_svc_path.display().to_string(),
        });
        refresh_legacy_copy_if_present(
            Path::new(LEGACY_DBUS_SYSTEM_SERVICE_PATH),
            DBUS_SERVICE,
            "org.facelock.Daemon",
        )?;

        run_cmd("systemctl", &["daemon-reload"])?;
        Terminal.info(&SystemMessage::SystemctlDaemonReloadDone);

        // The bus half of the same reload. Until the bus re-reads the policy
        // written above, nothing may own `org.facelock.Daemon` — the system
        // bus denies `own` by default and root is not exempt. Bus
        // implementations differ on whether they pick the directory up over
        // inotify, so ask; where it was unnecessary this is a no-op, and a
        // failure is not worth failing an install over. ADR 010 also relies
        // on this: a lock screen may call `Authenticate` as soon as the
        // policy changes.
        if let Err(e) = run_cmd("systemctl", &["reload", "dbus"]) {
            tracing::debug!("could not reload the D-Bus configuration: {e}");
        }

        run_cmd("systemctl", &["enable", SERVICE_FILENAME])?;
        Terminal.info(&SystemMessage::SystemctlEnableDone {
            unit: SERVICE_FILENAME.to_string(),
        });

        Terminal.info(&SystemMessage::DbusActivationEnabled);
    }

    Ok(())
}

/// How long a base flow waits for a freshly installed daemon to answer.
///
/// Bounded on purpose, and deliberately shorter than systemd's 90s start
/// timeout: the daemon is a convenience for the two steps that follow, not a
/// precondition for them, so a wizard must not stall a minute and a half on
/// one that is never coming up.
const DAEMON_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Gap between readiness probes.
const DAEMON_READY_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Bring the daemon up so the enrollment and test steps that follow select it
/// instead of falling back to direct camera access.
///
/// A stopped unit is started; one that is already running is restarted. The
/// daemon reads the encryption method (step 5), the model preset (step 2) and
/// the inference device (step 3) once, at startup, so on a second `setup` run
/// leaving the running instance alone would enroll and test through a daemon
/// holding the configuration the wizard has just replaced.
///
/// A restart needs more than a `Ping` to be believed. `--no-block` returns
/// when systemd has queued the job, not when it has run it, so the first
/// probe would be answered by the process being replaced — and step 7, with
/// no prompt in front of it under `--enroll`, would then select its backend
/// during the window where the old instance has exited and the new one is
/// still loading its models, falling back to direct camera access under a
/// line claiming the opposite. So the unit's main PID is read before the
/// restart and the wait requires a *different* non-zero PID as well as a
/// responding daemon ([`daemon_ready`]). The `start` branch needs none of
/// that: there is no outgoing instance for an answer to have come from.
///
/// Every failure here is survivable and none of them are fatal: [`run_systemd`]
/// has already installed and enabled the unit, which is what the user asked
/// for, and a daemon that does not come up leaves exactly the direct-access
/// fallback those steps used before this ordering existed. Reported, not
/// raised.
///
/// Only reached from a base flow — a standalone `facelock setup --systemd`
/// installs and enables as it always has, and starts on the next boot or on
/// the next D-Bus activation. The bus-config reload the daemon needs before it
/// can own its name is [`run_systemd`]'s, next to the policy file it serves.
fn start_daemon_for_setup(config: &Config) {
    if !daemon_start_wanted(&config.daemon.mode) {
        return;
    }

    let bring_up = daemon_bring_up_for(daemon_unit_is_active());
    // Read before the job is queued, so it names the instance being replaced.
    let outgoing_pid = bring_up.main_pid();

    // `--no-block` because `Type=dbus` makes a blocking start or restart wait
    // on bus-name acquisition under systemd's start timeout. The wait below is
    // ours, and bounded by us.
    let issued = run_cmd(
        "systemctl",
        &[bring_up.verb(), "--no-block", SERVICE_FILENAME],
    );
    if let Err(e) = &issued {
        tracing::warn!("could not {} {SERVICE_FILENAME}: {e}", bring_up.verb());
    }
    if restart_announced(bring_up, issued.is_ok()) {
        Terminal.info(&SystemMessage::DaemonRestarted);
    }

    if wait_for_daemon(DAEMON_READY_TIMEOUT, bring_up, outgoing_pid) {
        Terminal.info(&SystemMessage::DaemonRunning);
    } else {
        Terminal.info(&SystemMessage::DaemonNotReady {
            seconds: DAEMON_READY_TIMEOUT.as_secs(),
        });
    }
}

/// How the wizard brings the unit up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonBringUp {
    /// Nothing is running: `systemctl start`.
    Start,
    /// Something is running, on the configuration from before the wizard:
    /// `systemctl try-restart`.
    Restart,
}

impl DaemonBringUp {
    /// The two verbs differ in which starting state they do nothing for, and
    /// that is the whole reason both exist here: `start` is a no-op on an
    /// already-running unit — the stale daemon this pairing exists to replace
    /// — and `try-restart` is a no-op on a stopped one.
    ///
    /// Both directions of the race between the check and the call therefore
    /// end somewhere survivable. A unit that stops in that window is left
    /// down, rather than started by a verb whose branch never checked for
    /// that. A unit that starts in it — a concurrent D-Bus activation — takes
    /// `Start`, which is then the no-op: the freshly activated daemon
    /// survives with the pre-wizard configuration, and no restart is
    /// announced, because none happened. Milliseconds wide, and the readiness
    /// wait reports whatever is actually there either way.
    fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Restart => "try-restart",
        }
    }

    /// The unit's main PID, on the branch whose acceptance rule reads one.
    ///
    /// `Start` has no outgoing instance to tell a successor from, so it never
    /// consults a PID and does not pay a `systemctl show` per poll for one.
    fn main_pid(self) -> Option<u32> {
        match self {
            Self::Restart => daemon_main_pid(),
            Self::Start => None,
        }
    }
}

/// Which verb the unit's current state calls for. Pure, so the choice is
/// testable without systemd; [`daemon_unit_is_active`] is the part that is not.
fn daemon_bring_up_for(unit_active: bool) -> DaemonBringUp {
    if unit_active {
        DaemonBringUp::Restart
    } else {
        DaemonBringUp::Start
    }
}

/// Whether systemd reports the unit as already running.
///
/// `systemctl is-active` exits non-zero for every state that is not active, so
/// a stopped unit, a failed one, and a systemd that is not there at all all
/// read as inactive: the [`DaemonBringUp::Start`] branch, whose own failure is
/// already best effort.
///
/// The `Err` is the *answer*, not a fault — do not propagate it with `?`, or a
/// stopped unit becomes a fatal setup.
fn daemon_unit_is_active() -> bool {
    run_cmd("systemctl", &["is-active", "--quiet", SERVICE_FILENAME]).is_ok()
}

/// The PID of the running instance, or `None` when there is not one.
///
/// `systemctl show -p MainPID --value` prints `0` for a unit that is not
/// running; a systemd that is absent or that fails the query prints nothing
/// usable. All three mean the same thing here — no instance to tell apart from
/// its successor — so all three answer `None`, which [`daemon_ready`] treats
/// as "cannot prove a restart happened" on the restart branch and ignores
/// entirely on the start branch.
fn daemon_main_pid() -> Option<u32> {
    let out = Command::new("systemctl")
        .args(["show", "-p", "MainPID", "--value", SERVICE_FILENAME])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    (pid != 0).then_some(pid)
}

/// Whether the step announces the restart it asked for.
///
/// Only a restart systemd accepted: a rejected one replaced nothing, so the
/// daemon the wait goes on to find is the same one that was already there, and
/// saying otherwise would be the lie the notice exists to prevent.
fn restart_announced(bring_up: DaemonBringUp, systemctl_ok: bool) -> bool {
    systemctl_ok && matches!(bring_up, DaemonBringUp::Restart)
}

/// Whether a poll of the unit means the daemon the wizard asked for is up.
///
/// The two branches accept different evidence, because they have different
/// things to prove:
///
/// - `Start` had nothing running, so any daemon answering is the one it
///   started. A `Ping` is the whole test.
/// - `Restart` had something running, and that something answers `Ping` until
///   systemd gets round to stopping it. So the answer only counts once the
///   unit reports a *different*, non-zero main PID: systemd runs a restart as
///   stop-then-start, so a changed PID means the process that could have
///   answered falsely is gone.
///
/// `current_pid` of `None` — a stopped unit, or a systemd that would not say —
/// is never proof of a restart. It fails the wait rather than passing it,
/// which costs at worst a bounded 20s and the direct-access fallback the whole
/// step is best-effort about. An `old_pid` of `None` is the one soft spot: the
/// wizard could not name the outgoing instance, so any running one is accepted
/// and the branch degrades to the `Start` rule.
fn daemon_ready(
    bring_up: DaemonBringUp,
    old_pid: Option<u32>,
    current_pid: Option<u32>,
    ping_ok: bool,
) -> bool {
    if !ping_ok {
        return false;
    }
    match bring_up {
        DaemonBringUp::Start => true,
        DaemonBringUp::Restart => match current_pid {
            None => false,
            Some(pid) => Some(pid) != old_pid,
        },
    }
}

/// Whether a running daemon is any use to the steps that follow.
///
/// `mode = "oneshot"` is the configuration, not a degraded state: backend
/// selection never probes the bus under it, so enrollment and the test would
/// take direct camera access whether or not a daemon were running. Starting
/// one would cost the wizard a spurious wait and win it nothing.
fn daemon_start_wanted(mode: &facelock_core::config::DaemonMode) -> bool {
    use facelock_core::config::DaemonMode;
    matches!(mode, DaemonMode::Daemon)
}

/// Poll until the daemon the wizard asked for answers, or `timeout` elapses.
///
/// The probe is the seam's [`crate::backend::probe_daemon`], the full `Ping`
/// round-trip rather than the non-activating `name_has_owner` that backend
/// selection uses. "The daemon answers method calls" is the property the next
/// two steps need, and activating one systemd has not got to yet is a success
/// here rather than the hazard it would be at selection time.
///
/// [`daemon_ready`] decides what a poll proves; `old_pid` is the main PID read
/// before the restart was queued. The PID is read *before* the ping, and only
/// on the branch that needs it: a PID that is already the successor's cannot
/// then be paired with a ping the outgoing instance answered a moment earlier:
/// systemd runs a restart as stop-then-start, so the old process has exited by
/// the time its PID stops being the unit's.
fn wait_for_daemon(
    timeout: std::time::Duration,
    bring_up: DaemonBringUp,
    old_pid: Option<u32>,
) -> bool {
    use crate::backend::DaemonPing;
    use facelock_core::config::DaemonMode;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        let current_pid = bring_up.main_pid();
        let ping_ok = matches!(
            crate::backend::probe_daemon(&DaemonMode::Daemon).known(),
            Some(DaemonPing::Responding)
        );
        if daemon_ready(bring_up, old_pid, current_pid, ping_ok) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(DAEMON_READY_POLL);
    }
}

// ---------------------------------------------------------------------------
/// Serializes the tests that set the process-global config-path override.
/// Cargo runs a binary's tests on many threads in one process, so two such
/// tests interleaving would read or write through each other's override. Take
/// this lock before `set_process_config_override`, and declare it before the
/// clearing Drop guard so the override is cleared while the lock is still
/// held. A poisoned lock (a previous test panicked) is recovered rather than
/// cascading the failure.
#[cfg(test)]
static CONFIG_OVERRIDE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// Choice-flag tests
//
// These run as an unprivileged user with no camera, no TPM and no network. The
// flag paths are factored so every decision is a pure function and only the
// writers touch the filesystem, which is what makes that possible.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod choice_tests {
    use super::*;
    use facelock_core::config::EncryptionMethod;

    fn base_config() -> Config {
        Config::parse("").expect("an empty config must resolve to defaults")
    }

    fn cam(path: &str, name: &str, is_ir: bool, formats: &[&str]) -> CameraCandidate {
        CameraCandidate {
            device: facelock_camera::DeviceInfo {
                path: path.to_string(),
                name: name.to_string(),
                driver: "test".to_string(),
                capabilities: vec!["VIDEO_CAPTURE".to_string()],
                formats: formats
                    .iter()
                    .map(|fourcc| facelock_camera::FormatInfo {
                        fourcc: (*fourcc).to_string(),
                        description: "test".to_string(),
                        sizes: vec![(640, 480)],
                    })
                    .collect(),
            },
            is_ir,
        }
    }

    fn temp_config(name: &str, contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    // -- --models ------------------------------------------------------------

    /// The preset table is the contract documented in the plan; pin it.
    #[test]
    fn preset_models_match_documented_table() {
        assert_eq!(
            preset_models(ModelPreset::Standard),
            ("scrfd_2.5g_bnkps.onnx", "w600k_r50.onnx")
        );
        assert_eq!(
            preset_models(ModelPreset::Balanced),
            ("scrfd_2.5g_bnkps.onnx", "glintr100.onnx")
        );
        assert_eq!(
            preset_models(ModelPreset::High),
            ("det_10g.onnx", "glintr100.onnx")
        );
    }

    /// Every preset resolves to checksums taken from the bundled manifest, not
    /// from anything typed twice.
    #[test]
    fn preset_checksums_come_from_the_bundled_manifest() {
        let manifest: ModelManifest = toml::from_str(MANIFEST_TOML).unwrap();

        for preset in MODEL_PRESETS {
            let mut config = base_config();
            set_model_preset(&mut config, preset).unwrap();

            let (detector, embedder) = preset_models(preset);
            assert_eq!(config.recognition.detector_model, detector);
            assert_eq!(config.recognition.embedder_model, embedder);
            assert_eq!(
                config.recognition.detector_sha256.as_deref(),
                Some(manifest.find(detector).unwrap().sha256.as_str()),
                "{preset:?} detector checksum"
            );
            assert_eq!(
                config.recognition.embedder_sha256.as_deref(),
                Some(manifest.find(embedder).unwrap().sha256.as_str()),
                "{preset:?} embedder checksum"
            );
        }
    }

    /// The wizard picks `MODEL_PRESETS[selection]`, so the prompt labels must
    /// stay in preset order. This is what stops the two from drifting.
    #[test]
    fn wizard_options_are_in_preset_order() {
        assert_eq!(MODEL_PRESET_OPTIONS.len(), MODEL_PRESETS.len());
        for (idx, preset) in MODEL_PRESETS.iter().enumerate() {
            let expected_prefix = match preset {
                ModelPreset::Standard => "Standard",
                ModelPreset::Balanced => "Balanced",
                ModelPreset::High => "High accuracy",
            };
            assert!(
                MODEL_PRESET_OPTIONS[idx].starts_with(expected_prefix),
                "option {idx} ({}) does not describe {preset:?}",
                MODEL_PRESET_OPTIONS[idx]
            );
        }
    }

    /// The prompt's default index is a reverse lookup of the same table, so a
    /// config written by `--models` must pre-select the matching prompt entry.
    #[test]
    fn preset_reverse_lookup_round_trips() {
        for (idx, preset) in MODEL_PRESETS.iter().enumerate() {
            let mut config = base_config();
            set_model_preset(&mut config, *preset).unwrap();

            let found = preset_of_models(
                &config.recognition.detector_model,
                &config.recognition.embedder_model,
            );
            assert_eq!(found, Some(*preset));
            assert_eq!(
                MODEL_PRESETS.iter().position(|p| Some(*p) == found),
                Some(idx)
            );
        }
        assert_eq!(preset_of_models("custom.onnx", "custom.onnx"), None);
    }

    // -- --camera ------------------------------------------------------------

    #[test]
    fn camera_auto_selects_the_single_ir_device() {
        let candidates = [
            cam("/dev/video0", "Integrated Camera", false, &["MJPG"]),
            cam("/dev/video2", "Integrated IR Camera", true, &["GREY"]),
        ];
        assert_eq!(select_ir_camera(&candidates).unwrap(), "/dev/video2");
    }

    #[test]
    fn camera_auto_errors_when_no_ir_device() {
        let candidates = [cam("/dev/video0", "Integrated Camera", false, &["MJPG"])];
        let err = select_ir_camera(&candidates).unwrap_err().to_string();
        assert!(err.contains("no IR-capable camera"), "{err}");
        // The message has to be actionable: name what was found and the way out.
        assert!(err.contains("/dev/video0"), "{err}");
        assert!(err.contains("--camera="), "{err}");
    }

    #[test]
    fn camera_auto_example_escapes_an_enumerated_device_path() {
        let candidates = [cam(
            "/dev/video0\n  Success\x1b[32m",
            "Integrated Camera",
            false,
            &["MJPG"],
        )];

        let err = select_ir_camera(&candidates).unwrap_err().to_string();

        assert_eq!(err.lines().count(), 3, "{err:?}");
        assert!(!err.contains('\x1b'), "{err:?}");
        assert!(!err.lines().any(|line| line.trim() == "Success"), "{err:?}");
        assert!(err.contains("/dev/video0\\n"), "{err}");
    }

    #[test]
    fn camera_auto_errors_when_no_devices_at_all() {
        let err = select_ir_camera(&[]).unwrap_err().to_string();
        assert!(err.contains("no video devices"), "{err}");
    }

    /// Never silently take the first of several IR nodes — pinning setup to the
    /// wrong node means auth never works.
    #[test]
    fn camera_auto_errors_when_several_ir_devices() {
        let candidates = [
            cam("/dev/video2", "Integrated IR Camera", true, &["GREY"]),
            cam("/dev/video4", "BRIO IR", true, &["Y16"]),
        ];
        let err = select_ir_camera(&candidates).unwrap_err().to_string();
        assert!(err.contains("2 IR-capable cameras"), "{err}");
        assert!(
            err.contains("/dev/video2") && err.contains("/dev/video4"),
            "{err}"
        );
    }

    #[test]
    fn camera_auto_excludes_undecodable_ir_without_falling_back_to_rgb() {
        let candidates = [
            cam("/dev/video0", "Integrated Camera", false, &["MJPG"]),
            cam("/dev/video2", "Integrated IR Camera", true, &["Y10"]),
        ];

        let err = select_ir_camera(&candidates).unwrap_err().to_string();
        assert!(err.contains("no IR-capable camera"), "{err}");
        assert!(err.contains("/dev/video2"), "{err}");
        assert!(err.contains("Y10"), "{err}");
        assert!(err.contains("excluded: no decodable pixel format"), "{err}");
        assert!(!err.contains("--camera=/dev/video0"), "{err}");
    }

    #[test]
    fn setup_selection_keeps_grey_and_y16_but_excludes_y8_y10_y12() {
        let candidates = [
            cam("/dev/video0", "Y8 IR", true, &["Y8"]),
            cam("/dev/video1", "GREY IR", true, &["GREY"]),
            cam("/dev/video2", "Y10 IR", true, &["Y10"]),
            cam("/dev/video3", "Y16 IR", true, &["Y16"]),
            cam("/dev/video4", "Y12 IR", true, &["Y12"]),
            cam("/dev/video5", "RGB", false, &["MJPG"]),
        ];

        let paths: Vec<&str> = decodable_camera_candidates(&candidates)
            .into_iter()
            .map(|candidate| candidate.device.path.as_str())
            .collect();

        assert_eq!(paths, vec!["/dev/video1", "/dev/video3", "/dev/video5"]);
    }

    #[test]
    fn wizard_require_ir_refuses_excluded_ir_instead_of_offering_rgb() {
        let candidates = [
            cam("/dev/video0", "Integrated RGB", false, &["MJPG"]),
            cam("/dev/video2", "Y8 IR", true, &["Y8"]),
            cam("/dev/video4", "Y10 IR", true, &["Y10"]),
            cam("/dev/video6", "Y12 IR", true, &["Y12"]),
        ];

        let err = wizard_camera_candidates(&candidates, true).unwrap_err();
        assert!(camera_selection_error_is_fatal(&err));
        let err = err.to_string();

        for expected in [
            "/dev/video2",
            "Y8",
            "/dev/video4",
            "Y10",
            "/dev/video6",
            "Y12",
        ] {
            assert!(err.contains(expected), "missing {expected}: {err}");
        }
        assert!(!err.contains("/dev/video0"), "must not offer RGB: {err}");
        assert!(!err.contains("MJPG"), "must not recommend RGB: {err}");
    }

    #[test]
    fn required_ir_refusal_does_not_render_untrusted_camera_name_controls() {
        let candidates = [cam(
            "/dev/video2",
            "Y10 IR\n  Pass --camera=/dev/video0\x1b[31m",
            true,
            &["Y10"],
        )];

        let err = wizard_camera_candidates(&candidates, true)
            .unwrap_err()
            .to_string();

        assert_eq!(
            err.lines().count(),
            3,
            "one physical candidate line: {err:?}"
        );
        assert!(
            !err.contains('\x1b'),
            "must not render terminal escapes: {err:?}"
        );
        assert!(
            !err.contains("Pass --camera=/dev/video0"),
            "must not render an injected RGB recommendation: {err:?}"
        );
        assert!(
            err.contains("/dev/video2"),
            "must retain the device path: {err}"
        );
        assert!(err.contains("Y10"), "must retain advertised formats: {err}");
    }

    #[test]
    fn camera_menu_listing_escapes_hardware_derived_fields_on_one_line() {
        let candidate = cam(
            "/dev/video2\n/dev/video0",
            "IR \"camera\"\n\x1b[31m",
            true,
            &["Y\n1"],
        );

        let item = candidate.menu_listing();
        let diagnostic = camera_candidate_listing(std::slice::from_ref(&candidate));

        for rendered in [&item, &diagnostic] {
            assert_eq!(rendered.lines().count(), 1, "{rendered:?}");
            assert!(!rendered.contains('\x1b'), "{rendered:?}");
            assert!(!rendered.contains('\n'), "{rendered:?}");
        }
        assert!(
            item.contains("\\n"),
            "control characters should be visible: {item}"
        );
        assert!(item.contains("\\\"camera\\\""), "names stay quoted: {item}");
        assert!(
            diagnostic.contains("Y\\n1"),
            "formats stay actionable: {diagnostic}"
        );
    }

    #[test]
    fn wizard_without_require_ir_keeps_decodable_rgb_available() {
        let candidates = [
            cam("/dev/video0", "Integrated RGB", false, &["MJPG"]),
            cam("/dev/video2", "Y10 IR", true, &["Y10"]),
        ];

        let selectable = wizard_camera_candidates(&candidates, false).unwrap();

        assert_eq!(selectable.len(), 1);
        assert_eq!(selectable[0].device.path, "/dev/video0");
    }

    #[test]
    fn wizard_require_ir_keeps_grey_and_y16_when_other_ir_is_excluded() {
        let candidates = [
            cam("/dev/video0", "Integrated RGB", false, &["MJPG"]),
            cam("/dev/video2", "Y10 IR", true, &["Y10"]),
            cam("/dev/video4", "GREY IR", true, &["GREY"]),
            cam("/dev/video6", "Y16 IR", true, &["Y16"]),
        ];

        let paths: Vec<&str> = wizard_camera_candidates(&candidates, true)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.device.path.as_str())
            .collect();

        assert_eq!(paths, vec!["/dev/video0", "/dev/video4", "/dev/video6"]);
    }

    #[test]
    fn only_required_ir_unavailable_is_a_fatal_wizard_camera_error() {
        let security_refusal = anyhow::Error::new(RequiredIrUnavailable {
            listed: "    /dev/video2 - Y10 IR [Y10]".to_string(),
        });
        let ordinary_probe_error = anyhow::anyhow!("camera enumeration failed");

        assert!(camera_selection_error_is_fatal(&security_refusal));
        assert!(!camera_selection_error_is_fatal(&ordinary_probe_error));
    }

    #[test]
    fn camera_explicit_path_must_exist() {
        let mut config = base_config();
        let err = apply_camera_choice(
            &mut config,
            &CameraChoice::Path("/dev/video-does-not-exist".to_string()),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("does not exist"), "{err}");
        // Nothing was written into the config on the way out.
        assert_eq!(config.device.path, None);
    }

    // -- config writers ------------------------------------------------------

    #[test]
    fn update_config_device_appends_section_when_absent() {
        let (_dir, path) = temp_config("config.toml", "[recognition]\nthreshold = 0.75\n");
        let mut config = base_config();
        config.device.path = Some("/dev/video2".to_string());
        update_config_device_at(&path, &config).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("[device]"), "{result}");
        assert!(result.contains("path = \"/dev/video2\""), "{result}");
        assert!(result.contains("threshold = 0.75"), "{result}");
    }

    #[test]
    fn update_config_device_updates_path_in_place() {
        let (_dir, path) = temp_config(
            "config.toml",
            "[device]\npath = \"/dev/video0\"\nmax_height = 480\n\n[recognition]\nthreshold = 0.75\n",
        );
        let mut config = base_config();
        config.device.path = Some("/dev/video2".to_string());
        update_config_device_at(&path, &config).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("path = \"/dev/video2\""), "{result}");
        assert!(!result.contains("/dev/video0"), "{result}");
        // Unrelated keys, in this and in other sections, survive.
        assert!(result.contains("max_height = 480"), "{result}");
        assert!(result.contains("threshold = 0.75"), "{result}");
        assert_eq!(result.matches("[device]").count(), 1, "{result}");
        assert_eq!(result.matches("path = ").count(), 1, "{result}");
    }

    /// `[device]` has no other key that starts with `path`, but the writer must
    /// match on the assignment rather than the prefix regardless.
    #[test]
    fn key_assignment_matches_whole_key_only() {
        assert!(is_key_assignment("path = \"/dev/video0\"", "path"));
        assert!(is_key_assignment("  path=\"/dev/video0\"", "path"));
        assert!(!is_key_assignment("path_suffix = 1", "path"));
        assert!(!is_key_assignment("# path = \"/dev/video0\"", "path"));
    }

    // -- --execution-provider ------------------------------------------------

    /// Detection depends on which ONNX Runtime build the host has installed, so
    /// this asserts only that `auto` resolves to something the provider
    /// registry accepts — never that a particular GPU was found. The priority
    /// rule itself is unit-tested in `facelock-face`.
    #[test]
    fn execution_provider_auto_resolves_to_a_valid_provider() {
        let resolved = resolve_execution_provider_auto().expect("auto must always resolve");
        assert!(
            facelock_face::ProviderKind::parse(&resolved).is_some(),
            "auto produced an unregisterable provider: {resolved}"
        );

        assert_eq!(
            provider_name(ExecutionProviderChoice::Auto).unwrap(),
            resolved,
            "provider_name must not diverge from resolve_execution_provider_auto"
        );
    }

    #[test]
    fn explicit_providers_resolve_to_their_config_value() {
        for (choice, expected) in [
            (ExecutionProviderChoice::Cpu, "cpu"),
            (ExecutionProviderChoice::Cuda, "cuda"),
            (ExecutionProviderChoice::Rocm, "rocm"),
            (ExecutionProviderChoice::Openvino, "openvino"),
        ] {
            assert_eq!(provider_name(choice).unwrap(), expected);
        }
    }

    /// Including `rocm` and `openvino`, which the prompt never offers but the
    /// ORT provider registry accepts.
    #[test]
    fn every_explicit_provider_persists() {
        for provider in ["cpu", "cuda", "rocm", "openvino"] {
            let (_dir, path) = temp_config(
                "config.toml",
                "[recognition]\nexecution_provider = \"cpu\"\nthreshold = 0.75\n",
            );
            let mut config = base_config();
            config.recognition.execution_provider = provider.to_string();
            update_config_provider_at(&path, &config).unwrap();

            let result = std::fs::read_to_string(&path).unwrap();
            assert!(
                result.contains(&format!("execution_provider = \"{provider}\"")),
                "{result}"
            );
            assert!(result.contains("threshold = 0.75"), "{result}");
            assert_eq!(result.matches("execution_provider").count(), 1, "{result}");
        }
    }

    #[test]
    fn provider_persists_into_a_config_without_a_recognition_section() {
        let (_dir, path) = temp_config("config.toml", "[device]\npath = \"/dev/video0\"\n");
        let mut config = base_config();
        config.recognition.execution_provider = "rocm".to_string();
        update_config_provider_at(&path, &config).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("[recognition]"), "{result}");
        assert!(result.contains("execution_provider = \"rocm\""), "{result}");
        assert!(result.contains("path = \"/dev/video0\""), "{result}");
    }

    // -- --encryption --------------------------------------------------------

    /// A TPM that is not there is an error, never a quiet downgrade to a
    /// software keyfile: the user asked for hardware protection.
    #[test]
    fn encryption_tpm_without_a_tpm_errors_instead_of_downgrading() {
        let mut config = Config::parse(
            "[tpm]\ntcti = \"device:/nonexistent/tpm\"\n\n[encryption]\nmethod = \"none\"\n",
        )
        .unwrap();

        let err = apply_encryption_choice(&mut config, EncryptionChoice::Tpm, None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("no usable TPM"), "{err}");
        assert!(err.contains("--encryption=keyfile"), "{err}");
        // Specifically not Keyfile: that is the downgrade this must never do.
        assert_eq!(config.encryption.method, EncryptionMethod::None);
    }

    /// Writes through `paths::config_path()`, so it needs the process override.
    #[test]
    fn encryption_none_sets_method_none() {
        let _lock = super::CONFIG_OVERRIDE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        struct OverrideGuard;
        impl Drop for OverrideGuard {
            fn drop(&mut self) {
                facelock_core::paths::clear_process_config_override();
            }
        }

        let (_dir, path) = temp_config(
            "config.toml",
            "[encryption]\nmethod = \"keyfile\"\nkey_path = \"/tmp/k\"\n",
        );

        facelock_core::paths::set_process_config_override(path.clone());
        let _guard = OverrideGuard;

        let mut config = base_config();
        apply_encryption_choice(&mut config, EncryptionChoice::None, None).unwrap();

        assert_eq!(config.encryption.method, EncryptionMethod::None);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("method = \"none\""), "{result}");
    }

    /// The orphaned-models guard must not become a prompt when no theme is
    /// available — that is the non-interactive path, and it must not hang.
    #[test]
    fn auto_encryption_keygen_detection() {
        let mut config = base_config();
        config.encryption.method = EncryptionMethod::None;
        config.encryption.key_path = "/nonexistent/facelock/encryption.key".to_string();
        config.encryption.sealed_key_path =
            "/nonexistent/facelock/encryption.key.sealed".to_string();

        assert!(auto_encryption_needs_keygen(&config, false));
        assert!(auto_encryption_needs_keygen(&config, true));

        // Already-configured encryption never mints a key.
        config.encryption.method = EncryptionMethod::Keyfile;
        assert!(!auto_encryption_needs_keygen(&config, false));

        // An existing key file is reused, not replaced.
        let (_dir, key) = temp_config("encryption.key", "not-a-real-key");
        config.encryption.method = EncryptionMethod::None;
        config.encryption.key_path = key.display().to_string();
        assert!(!auto_encryption_needs_keygen(&config, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manifest() {
        let manifest: ModelManifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert_eq!(manifest.models.len(), 4);

        let required: Vec<_> = manifest.models.iter().filter(|m| !m.optional).collect();
        assert_eq!(required.len(), 2);
        assert_eq!(required[0].name, "scrfd_2.5g");
        assert_eq!(required[1].name, "arcface_r50");
    }

    #[test]
    fn check_model_missing_file() {
        let status = check_model(Path::new("/nonexistent/model.onnx"), "abc123").unwrap();
        assert!(matches!(status, ModelStatus::Missing));
    }

    #[test]
    fn check_model_empty_sha256_accepts_any() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"test data").unwrap();

        let status = check_model(&path, "").unwrap();
        assert!(matches!(status, ModelStatus::Present));
    }

    #[test]
    fn detect_candidates_filters_by_presence() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("sudo"), "").unwrap();
        std::fs::write(tmp.path().join("hyprlock"), "").unwrap();
        let found: Vec<_> = candidates_in(&only(tmp.path()))
            .iter()
            .map(|c| c.service)
            .collect();
        assert!(found.contains(&"sudo"));
        assert!(found.contains(&"hyprlock"));
        assert!(!found.contains(&"sddm"));
        assert!(!found.contains(&"gdm-password"));
        assert!(!found.contains(&"swaylock"));
    }

    #[test]
    fn omarchy_lock_face_is_offered_but_opt_in() {
        let candidate = PAM_CANDIDATES
            .iter()
            .find(|c| c.service == "omarchy-lock-face")
            .expect("omarchy-lock-face must be offered by the wizard");
        assert_eq!(candidate.category, PamCategory::LockScreen);
        assert!(
            !candidate.default_enabled,
            "omarchy-lock-face is opt-in: the multi-select must not pre-check it"
        );

        // ...and, like every other candidate, it is only offered when its
        // service file is actually present.
        let tmp = tempfile::TempDir::new().unwrap();
        let present = |base: &Path| {
            candidates_in(&only(base))
                .iter()
                .any(|c| c.service == "omarchy-lock-face")
        };
        assert!(!present(tmp.path()));
        std::fs::write(tmp.path().join("omarchy-lock-face"), "").unwrap();
        assert!(present(tmp.path()));
    }

    #[test]
    fn no_excluded_services_in_candidates() {
        let excluded = [
            "system-auth",
            "system-auth-ac",
            "common-auth",
            "password-auth",
            "password-auth-ac",
            "system-login",
            "login",
            "su",
            "passwd",
            "chsh",
            "chfn",
        ];
        for ex in excluded {
            assert!(
                !PAM_CANDIDATES.iter().any(|c| c.service == ex),
                "{ex} must not appear in PAM_CANDIDATES"
            );
        }
    }

    /// The ordinary wizard multi-select does not grant sensitive-service
    /// authorization. Its candidates therefore have to stay disjoint from
    /// `SENSITIVE_SERVICES`, or a default wizard selection could unexpectedly
    /// require a separate authorization. The lists live in different modules,
    /// so pin their intersection directly rather than relying on the exclusions
    /// above, which would not notice a new sensitive-service entry.
    #[test]
    fn no_candidate_is_a_sensitive_service() {
        for candidate in PAM_CANDIDATES {
            assert!(
                !super::super::pam::SENSITIVE_SERVICES.contains(&candidate.service),
                "`{}` is offered by the wizard without implicit sensitive \
                 authorization — remove it from one list or the other",
                candidate.service
            );
        }
    }

    /// Every uninstall surface delegates discovery and mutation to the same
    /// config-independent command. A fixed service list cannot cover the
    /// arbitrary names accepted by `pam add --service`, and a shell `sed`
    /// cannot enforce the writer's provenance, confinement or rollback rules.
    #[test]
    fn every_installer_calls_the_shared_pam_cleanup() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

        for rel in [
            "dist/facelock.install",
            "dist/facelock.spec",
            "debian/prerm",
            "justfile",
        ] {
            let path = root.join(rel);
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{rel} must be readable from the workspace: {e}"));

            assert!(
                source.contains("facelock pam remove --all"),
                "{rel} must call the shared config-independent PAM cleanup"
            );
            assert!(!source.contains("FACELOCK_PAM_SERVICES="), "{rel}");
            assert!(!source.contains("sed -i '/pam_facelock"), "{rel}");
        }
    }

    #[test]
    fn man_pages_contain_no_prohibited_control_bytes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut pages = fs::read_dir(root.join("man"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "1"))
            .collect::<Vec<_>>();
        pages.sort();
        assert!(
            !pages.is_empty(),
            "the man-page control-byte guard must cover a page"
        );

        for path in pages {
            let bytes = fs::read(&path).unwrap();
            let prohibited = bytes
                .iter()
                .enumerate()
                .filter_map(|(offset, byte)| {
                    matches!(*byte, 0x00..=0x08 | 0x0b..=0x0c | 0x0e..=0x1f | 0x7f)
                        .then_some((offset, *byte))
                })
                .collect::<Vec<_>>();

            assert!(
                prohibited.is_empty(),
                "{} contains prohibited control bytes at offset/byte pairs: {prohibited:?}",
                path.display()
            );
        }
    }

    #[test]
    fn arch_packages_ship_an_aborting_pretransaction_cleanup_hook() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let hook = fs::read_to_string(root.join("dist/facelock-pam-remove.hook")).unwrap();
        for required in [
            "Operation = Remove",
            "Type = Package",
            "When = PreTransaction",
            "Exec = /usr/bin/facelock pam remove --all",
            "AbortOnFail",
        ] {
            assert!(hook.contains(required), "missing `{required}`");
        }
        assert!(
            !hook.contains("Operation = Upgrade"),
            "the cleanup hook must not remove configured PAM edits during upgrade"
        );
        let contracts = fs::read_to_string(root.join("docs/contracts.md")).unwrap();
        assert!(contracts.contains("Remove-only"));
        assert!(!contracts.contains("`Remove`/`Upgrade`"));
        for pkgbuild in ["dist/PKGBUILD", "dist/PKGBUILD-git", "dist/PKGBUILD-bin"] {
            let source = fs::read_to_string(root.join(pkgbuild)).unwrap();
            assert!(source.contains("facelock-pam-remove.hook"), "{pkgbuild}");
            assert!(source.contains("usr/share/libalpm/hooks"), "{pkgbuild}");
        }
    }

    #[test]
    fn package_validation_proves_removal_aborts_before_the_module_is_removed() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let validation = fs::read_to_string(root.join("test/pkg-validate.sh")).unwrap();

        for required in [
            "facelock-package-owned",
            "facelock-package-blocker",
            "dpkg removal aborts on an unmanaged PAM reference",
            "aborted dpkg removal leaves inactive common-auth bytes unchanged",
            "rpm removal aborts on an unmanaged PAM reference",
            "PAM module remains after aborted package removal",
            "recognized PAM edit remains after aborted package removal",
        ] {
            assert!(validation.contains(required), "missing `{required}`");
        }
    }

    #[test]
    fn package_validation_covers_frontend_abort_retention_and_success() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let validation = fs::read_to_string(root.join("test/pkg-validate.sh")).unwrap();

        for required in [
            "apt-get wrapper removal aborts on an unmanaged PAM reference",
            "apt-get wrapper keeps the package installed after abort",
            "apt-get wrapper keeps the PAM module after abort",
            "apt-get wrapper preserves recognized PAM edit bytes after abort",
            "apt wrapper removal aborts on an unmanaged PAM reference",
            "apt wrapper keeps the package installed after abort",
            "apt wrapper keeps the PAM module after abort",
            "apt wrapper preserves recognized PAM edit bytes after abort",
            "apt-get wrapper removal succeeds without a blocker",
            "apt-get wrapper leaves the package not installed",
            "apt-get wrapper removes the PAM module",
            "apt wrapper removal succeeds without a blocker",
            "apt wrapper leaves the package not installed",
            "apt wrapper removes the PAM module",
            "dnf wrapper removal aborts on an unmanaged PAM reference",
            "dnf wrapper keeps the package installed after abort",
            "dnf wrapper keeps the PAM module after abort",
            "dnf wrapper preserves recognized PAM edit bytes after abort",
            "dnf wrapper removal succeeds without a blocker",
        ] {
            assert!(validation.contains(required), "missing `{required}`");
        }
        assert!(validation.contains("db:Status-Status"));
        let rpm = fs::read_to_string(root.join("test/Containerfile.rpm-e2e")).unwrap();
        let deb = fs::read_to_string(root.join("test/Containerfile.deb-runtime")).unwrap();
        assert!(deb.contains("/facelock-test-package.deb"));
        assert!(deb.contains("apt-get install -y /facelock-test-package.deb"));
        assert!(rpm.contains("/facelock-test-package.rpm"));
    }

    #[test]
    fn rpm_preun_propagates_shared_cleanup_failure() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let spec = fs::read_to_string(root.join("dist/facelock.spec")).unwrap();

        assert!(
            spec.contains("facelock pam remove --all || exit $?"),
            "the RPM preun scriptlet must abort before a later best-effort command can mask cleanup failure"
        );
    }

    #[test]
    fn package_runner_never_mounts_checkout_models_at_the_mutable_runtime_path() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let runner = fs::read_to_string(root.join("test/run-pkg-validate-systemd.sh")).unwrap();

        assert!(runner.contains("$PWD/models:/facelock-test-models:ro"));
        assert!(runner.contains("for model in /facelock-test-models/*.onnx"));
        assert!(runner.contains("install -m 0644 \"$model\" /var/lib/facelock/models/"));
        assert!(
            !runner.contains("$PWD/models:/var/lib/facelock/models"),
            "the removal test deletes its runtime model directory; the checkout must never be mounted there"
        );
    }

    #[test]
    fn arch_package_validation_runs_an_aborting_booted_transaction() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let justfile = fs::read_to_string(root.join("justfile")).unwrap();
        let container = fs::read_to_string(root.join("test/Containerfile")).unwrap();
        let runner = fs::read_to_string(root.join("test/run-arch-package-systemd.sh")).unwrap();
        let validation = fs::read_to_string(root.join("test/arch-package-validate.sh")).unwrap();

        assert!(justfile.contains("test/run-arch-package-systemd.sh facelock-pam-test"));
        assert!(container.contains("test/build-arch-test-package.sh"));
        assert!(runner.contains("--systemd=always"));
        for required in [
            "pacman removal aborts on an unmanaged PAM reference",
            "facelock-package-owned",
            "facelock-package-blocker",
            "pacman -Q facelock",
            "PAM module remains after aborted package removal",
        ] {
            assert!(validation.contains(required), "missing `{required}`");
        }
    }

    #[test]
    fn check_model_correct_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello world").unwrap();

        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let status = check_model(&path, expected).unwrap();
        assert!(matches!(status, ModelStatus::Present));

        let status = check_model(&path, "0000000000000000").unwrap();
        assert!(matches!(status, ModelStatus::BadChecksum));
    }

    #[test]
    fn update_config_models_scenarios() {
        let _lock = super::CONFIG_OVERRIDE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        struct ProcessConfigOverrideGuard;

        impl Drop for ProcessConfigOverrideGuard {
            fn drop(&mut self) {
                facelock_core::paths::clear_process_config_override();
            }
        }

        facelock_core::paths::set_process_config_override(config_path.clone());
        let _override_guard = ProcessConfigOverrideGuard;

        // Scenario 1: appends [recognition] section when absent
        std::fs::write(&config_path, "[device]\npath = \"/dev/video0\"\n").unwrap();
        let mut config = Config::load_from(&config_path).unwrap();
        config.recognition.detector_model = "det_10g.onnx".to_string();
        config.recognition.embedder_model = "glintr100.onnx".to_string();
        update_config_models(&config).unwrap();

        let result = std::fs::read_to_string(&config_path).unwrap();
        assert!(result.contains("[recognition]"));
        assert!(result.contains("detector_model = \"det_10g.onnx\""));
        assert!(result.contains(
            "detector_sha256 = \"5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91\""
        ));
        assert!(result.contains("embedder_model = \"glintr100.onnx\""));
        assert!(result.contains(
            "embedder_sha256 = \"4ab1d6435d639628a6f3e5008dd4f929edf4c4124b1a7169e1048f9fef534cdf\""
        ));

        // Scenario 2: updates existing model fields, preserves other fields
        std::fs::write(
            &config_path,
            "[device]\npath = \"/dev/video0\"\n\n[recognition]\ndetector_model = \"scrfd_2.5g_bnkps.onnx\"\nembedder_model = \"w600k_r50.onnx\"\nthreshold = 0.80\n",
        )
        .unwrap();
        let mut config = Config::load_from(&config_path).unwrap();
        config.recognition.detector_model = "det_10g.onnx".to_string();
        config.recognition.embedder_model = "glintr100.onnx".to_string();
        update_config_models(&config).unwrap();

        let result = std::fs::read_to_string(&config_path).unwrap();
        assert!(result.contains("detector_model = \"det_10g.onnx\""));
        assert!(result.contains(
            "detector_sha256 = \"5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91\""
        ));
        assert!(result.contains("embedder_model = \"glintr100.onnx\""));
        assert!(result.contains(
            "embedder_sha256 = \"4ab1d6435d639628a6f3e5008dd4f929edf4c4124b1a7169e1048f9fef534cdf\""
        ));
        assert!(!result.contains("scrfd_2.5g_bnkps.onnx"));
        assert!(!result.contains("w600k_r50.onnx"));
        assert!(result.contains("threshold = 0.80"));

        // Scenario 3: adds model fields to existing [recognition] without them
        std::fs::write(
            &config_path,
            "[device]\npath = \"/dev/video0\"\n\n[recognition]\nthreshold = 0.75\n",
        )
        .unwrap();
        let mut config = Config::load_from(&config_path).unwrap();
        config.recognition.detector_model = "det_10g.onnx".to_string();
        config.recognition.embedder_model = "glintr100.onnx".to_string();
        update_config_models(&config).unwrap();

        let result = std::fs::read_to_string(&config_path).unwrap();
        assert!(result.contains("detector_model = \"det_10g.onnx\""));
        assert!(result.contains(
            "detector_sha256 = \"5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91\""
        ));
        assert!(result.contains("embedder_model = \"glintr100.onnx\""));
        assert!(result.contains(
            "embedder_sha256 = \"4ab1d6435d639628a6f3e5008dd4f929edf4c4124b1a7169e1048f9fef534cdf\""
        ));
        assert!(result.contains("threshold = 0.75"));
    }

    /// ADR 010: the default context may call exactly `Authenticate`; there
    /// is no group policy and only root may receive signals. Pinned on the
    /// embedded policy so a "cleanup" of the XML cannot silently close the
    /// lock screen out again, reopen the whole interface, or bring the group
    /// back. Order matters — dbus-daemon and dbus-broker apply the last
    /// matching rule in a context — so the allow must follow the deny.
    #[test]
    fn dbus_policy_opens_authenticate_to_the_default_context_only() {
        /// The text between the first `open` and the `close` that follows it.
        fn between<'a>(hay: &'a str, open: &str, close: &str) -> &'a str {
            let start = hay
                .find(open)
                .unwrap_or_else(|| panic!("{open:?} not found in the policy"))
                + open.len();
            let len = hay[start..]
                .find(close)
                .unwrap_or_else(|| panic!("{close:?} does not follow {open:?} in the policy"));
            &hay[start..start + len]
        }

        let policy = DBUS_POLICY;
        assert_eq!(
            policy.matches(r#"<policy context="default">"#).count(),
            1,
            "exactly one default-context policy block; a second one could reopen the interface below this test's slice"
        );
        let default = between(policy, r#"<policy context="default">"#, "</policy>");

        let deny = default
            .find(r#"<deny send_destination="org.facelock.Daemon"/>"#)
            .expect("default context denies the interface");
        assert_eq!(
            default.matches("<allow").count(),
            1,
            "exactly one allow in the default context"
        );
        let allow_start = default.find("<allow").expect("the one allow");
        let allow = between(default, "<allow", "/>");
        assert!(
            allow_start > deny,
            "the Authenticate allow must follow the deny"
        );
        for attr in [
            r#"send_destination="org.facelock.Daemon""#,
            r#"send_interface="org.facelock.Daemon""#,
            r#"send_member="Authenticate""#,
        ] {
            assert!(allow.contains(attr), "allow lacks {attr}: {allow}");
        }
        assert!(
            default
                .contains(r#"<deny receive_sender="org.facelock.Daemon" receive_type="signal"/>"#),
            "signals stay denied to the default context"
        );

        assert!(
            !policy.contains("<policy group"),
            "no group policy (ADR 010: the group is retired)"
        );
        let root = between(policy, r#"<policy user="root">"#, "</policy>");
        let signal_at = root
            .find(r#"receive_type="signal""#)
            .expect("root may receive the daemon's signals");
        let allow_at = root[..signal_at]
            .rfind("<allow")
            .expect("the root signal rule is an allow");
        let signal_allow = between(&root[allow_at..], "<allow", "/>");
        assert!(
            signal_allow.contains(r#"receive_sender="org.facelock.Daemon""#),
            "root signal allow lacks receive_sender: {signal_allow}"
        );
        assert_eq!(
            policy.matches(r#"receive_type="signal""#).count(),
            2,
            "signal rules: the root allow and the default-context deny, nothing else"
        );
    }
}

/// Tests for the action opt-outs `--no-pam` / `--no-systemd` / `--no-enroll`.
///
/// All of these run as an unprivileged user with no camera, no systemd and no
/// writes outside a tempdir.
#[cfg(test)]
mod action_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A realistic-enough `/etc/pam.d` to hash.
    fn fake_pam_d() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(
            dir.path().join("sudo"),
            "#%PAM-1.0\nauth\t\tinclude\t\tsystem-auth\naccount\t\tinclude\t\tsystem-auth\nsession\t\tinclude\t\tsystem-auth\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("polkit-1"),
            "#%PAM-1.0\nauth\t\tinclude\t\tsystem-auth\naccount\t\tinclude\t\tsystem-auth\npassword\tinclude\t\tsystem-auth\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("hyprlock"),
            "#%PAM-1.0\nauth\t\tinclude\t\tsystem-auth\n",
        )
        .unwrap();
        dir
    }

    /// SHA-256 of every entry in `dir`, keyed by file name.
    ///
    /// Enumerating the directory rather than the files we wrote is what catches
    /// a stray `.facelock-backup` appearing.
    fn hash_dir(dir: &Path) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                continue;
            }
            let bytes = fs::read(entry.path()).unwrap();
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hex: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            out.insert(entry.file_name().to_string_lossy().into_owned(), hex);
        }
        out
    }

    fn pam_plan(pam: PamPref) -> SetupPlan {
        SetupPlan {
            pam,
            ..SetupPlan::default()
        }
    }

    // -- `--no-pam` ---------------------------------------------------------

    /// The plan's acceptance criterion: declining PAM leaves every file under
    /// the PAM directory byte-identical, and adds none.
    ///
    /// `hash_harness_detects_a_real_pam_write` below is the foil that keeps
    /// this honest. It used to be `default_plan_still_configures_pam`, which
    /// drove the default (`Ask`) plan and relied on step 9 auto-selecting every
    /// `default_enabled` candidate when stdin was not a terminal. That branch
    /// is gone (#174) and the test with it — `cargo test` from a terminal gets
    /// a tty on stdin, so it was already blocking on the multi-select there.
    #[test]
    fn no_pam_leaves_every_pam_file_byte_identical() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = pam_plan(PamPref::Skip);
        let configured =
            pam_step_in(&only(dir.path()), &plan, &ColorfulTheme::default(), true).unwrap();

        assert!(configured.is_empty());
        assert_eq!(
            before,
            hash_dir(dir.path()),
            "--no-pam must not modify or add any file under pam.d"
        );
    }

    /// Positive control for the harness above: without this, that test could
    /// pass because `hash_dir` cannot see a write at all.
    #[test]
    fn hash_harness_detects_a_real_pam_write() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = SetupPlan {
            yes: true,
            ..pam_plan(PamPref::Install {
                service: Some("sudo".to_string()),
                if_present: false,
            })
        };
        let configured =
            pam_step_in(&only(dir.path()), &plan, &ColorfulTheme::default(), true).unwrap();

        assert_eq!(configured, vec!["sudo".to_string()]);
        let after = hash_dir(dir.path());
        assert_ne!(before["sudo"], after["sudo"], "sudo must have changed");
        assert_eq!(
            fs::read_dir(dir.path().join(".facelock-pam-backups"))
                .unwrap()
                .count(),
            2,
            "a dedicated backup and provenance record must have appeared"
        );
        assert!(super::super::pam::is_configured(&only(dir.path()), "sudo"));
    }

    /// `--pam --remove` is `run_with_plan`'s job; step 9 must not pre-empt it.
    #[test]
    fn pam_remove_is_deferred_and_step_9_writes_nothing() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = pam_plan(PamPref::Remove {
            service: "sudo".to_string(),
            if_present: true,
        });
        assert_eq!(pam_step_for(&plan), PamStep::Deferred);
        assert!(
            pam_step_in(&only(dir.path()), &plan, &ColorfulTheme::default(), true)
                .unwrap()
                .is_empty()
        );
        assert_eq!(before, hash_dir(dir.path()));
    }

    // -- `--pam` in a wizard base -------------------------------------------

    /// `--pam --service hyprlock` configures exactly `hyprlock`.
    #[test]
    fn pam_with_service_configures_only_that_service() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = SetupPlan {
            yes: true,
            ..pam_plan(PamPref::Install {
                service: Some("hyprlock".to_string()),
                if_present: false,
            })
        };
        let configured =
            pam_step_in(&only(dir.path()), &plan, &ColorfulTheme::default(), true).unwrap();

        assert_eq!(configured, vec!["hyprlock".to_string()]);
        let after = hash_dir(dir.path());
        assert_ne!(before["hyprlock"], after["hyprlock"]);
        assert_eq!(before["sudo"], after["sudo"]);
        assert_eq!(before["polkit-1"], after["polkit-1"]);
    }

    /// `--if-present` reaches the writer through step 9, not only through the
    /// alias call in [`run_with_plan`].
    ///
    /// `setup --pam --service X --if-present --enroll` resolves to a *wizard*
    /// base, so `run_with_plan` skips its own `install_for_setup` and step 9
    /// performs the install instead. Step 9 used to hand the writer a
    /// hard-coded `false`, which made the flag mean one thing on
    /// `--pam --if-present` and nothing at all as soon as any base-forcing flag
    /// was typed beside it.
    #[test]
    fn pam_if_present_survives_into_step_nine() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = SetupPlan {
            yes: true,
            ..pam_plan(PamPref::Install {
                service: Some("facelock-absent".to_string()),
                if_present: true,
            })
        };
        assert_eq!(
            pam_step_for(&plan),
            PamStep::Install {
                service: "facelock-absent".to_string(),
                if_present: true,
            }
        );

        let configured =
            pam_step_in(&only(dir.path()), &plan, &ColorfulTheme::default(), true).unwrap();
        assert!(
            configured.is_empty(),
            "an absent service configured nothing, so the closing summary must not \
             name it and the hyprlock handoff must not fire for it: {configured:?}"
        );
        assert_eq!(
            before,
            hash_dir(dir.path()),
            "an absent service under --if-present must write nothing"
        );
    }

    /// The default stays a hard error: without the flag, a service that is not
    /// there fails the step rather than being skipped. This is what catches
    /// `--service polkti-1`.
    #[test]
    fn an_absent_service_without_if_present_still_fails_step_nine() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = SetupPlan {
            yes: true,
            ..pam_plan(PamPref::Install {
                service: Some("facelock-absent".to_string()),
                if_present: false,
            })
        };

        let error = pam_step_in(&only(dir.path()), &plan, &ColorfulTheme::default(), true)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("PAM service file not found:"),
            "got: {error}"
        );
        assert_eq!(before, hash_dir(dir.path()));
    }

    /// Bare `--pam` means `sudo`, not the candidates' `default_enabled` set.
    ///
    /// `yes` is set for the same reason its sibling tests set it: without it
    /// the write reaches the per-file "Proceed?" confirmation, and `cargo test`
    /// run from a terminal hands the test a real tty on stdin, so the prompt
    /// blocks the suite rather than failing it. The assertion is about *which*
    /// service is configured; prompting is not part of it.
    #[test]
    fn pam_without_service_means_sudo_not_the_default_enabled_set() {
        let plan = SetupPlan {
            yes: true,
            ..pam_plan(PamPref::Install {
                service: None,
                if_present: false,
            })
        };
        assert_eq!(
            pam_step_for(&plan),
            PamStep::Install {
                service: DEFAULT_PAM_SERVICE.to_string(),
                if_present: false,
            }
        );

        let dir = fake_pam_d();
        let before = hash_dir(dir.path());
        let configured =
            pam_step_in(&only(dir.path()), &plan, &ColorfulTheme::default(), true).unwrap();
        assert_eq!(configured, vec![DEFAULT_PAM_SERVICE.to_string()]);

        // hyprlock is `default_enabled` but was not named, so it is untouched.
        let after = hash_dir(dir.path());
        assert_eq!(before["hyprlock"], after["hyprlock"]);
        assert_eq!(before["polkit-1"], after["polkit-1"]);
    }

    /// `setup --yes` and `--non-interactive` answer prompts only. Neither is
    /// authorization to edit a shared or login PAM stack.
    /// `commands::pam` pins that the engine honours the knobs; this pins that
    /// `setup` keeps prompt suppression separate from sensitive authorization.
    #[test]
    fn setup_maps_prompt_and_sensitive_authorization_independently() {
        let with = |base, yes, allow_sensitive| {
            setup_pam_knobs(&SetupPlan {
                base,
                yes,
                allow_sensitive,
                ..SetupPlan::default()
            })
        };

        let knobs = |no_confirm, allow_sensitive| PamKnobs {
            no_confirm,
            allow_sensitive,
        };

        // Standalone `--pam`: ask, and refuse the sensitive services.
        assert_eq!(with(None, false, false), knobs(false, false));
        // `--non-interactive --pam`: no prompts, and *still* refuse them.
        assert_eq!(
            with(Some(BaseMode::NonInteractive), false, false),
            knobs(true, false)
        );
        // `--pam --yes`: suppress the ordinary confirmation, but do not
        // authorize a sensitive PAM mutation.
        assert_eq!(with(None, true, false), knobs(true, false));
        assert_eq!(
            with(Some(BaseMode::NonInteractive), true, false),
            knobs(true, false)
        );
        // `--allow-sensitive` authorizes the gated write and does not answer
        // the ordinary confirmation by itself.
        assert_eq!(with(None, false, true), knobs(false, true));
        assert_eq!(with(None, true, true), knobs(true, true));
    }

    // -- `--no-systemd` -----------------------------------------------------

    /// `--no-systemd` writes no unit files and invokes no `systemctl`. The
    /// decision is made before any I/O, so this is assertable without root.
    #[test]
    fn no_systemd_writes_nothing_and_invokes_no_systemctl() {
        let plan = SetupPlan {
            systemd: SystemdPref::Skip,
            ..SetupPlan::default()
        };
        let step = systemd_step_for(&plan);
        assert_eq!(step, SystemdStep::Skip);

        // `Skip` is the one step-6 arm with no `run_systemd` behind it —
        // the other three all reach it, by prompt or by flag. And
        // `run_with_plan`'s standalone arms act only on Install/Disable.
        assert!(!matches!(
            plan.systemd,
            SystemdPref::Install | SystemdPref::Disable
        ));
    }

    /// `--systemd` and `--systemd --disable` are answers, not deferrals: step
    /// 6 applies them itself so the daemon is configured before enrollment.
    /// They used to share one `Deferred` variant, which is why the step could
    /// not tell an install from a disable.
    #[test]
    fn systemd_flags_are_applied_by_the_daemon_step() {
        for (pref, expected) in [
            (SystemdPref::Install, SystemdStep::Install),
            (SystemdPref::Disable, SystemdStep::Disable),
        ] {
            let plan = SetupPlan {
                systemd: pref,
                ..SetupPlan::default()
            };
            assert_eq!(systemd_step_for(&plan), expected);
        }
    }

    /// The other half of that move: `run_with_plan` must not repeat what the
    /// base flow's step 6 already did. Its `--systemd` arms are guarded on
    /// `plan.base.is_none()`, which only a standalone `facelock setup
    /// --systemd` satisfies.
    #[test]
    fn run_with_plan_applies_systemd_only_without_a_base_flow() {
        // Standalone: no base flow exists to run step 6.
        let standalone = resolve_setup_plan(SetupArgs {
            systemd: true,
            ..SetupArgs::default()
        });
        assert_eq!(standalone.base, None);
        assert_eq!(standalone.systemd, SystemdPref::Install);

        // Any flag that forces the base flow hands the action to step 6.
        for args in [
            SetupArgs {
                systemd: true,
                enroll: true,
                ..SetupArgs::default()
            },
            SetupArgs {
                systemd: true,
                non_interactive: true,
                ..SetupArgs::default()
            },
        ] {
            let plan = resolve_setup_plan(args);
            assert!(plan.base.is_some());
            assert_eq!(plan.systemd, SystemdPref::Install);
            assert_eq!(systemd_step_for(&plan), SystemdStep::Install);
        }
    }

    /// A daemon-less install enrolls exactly as it did before the reorder.
    /// `--no-systemd` declines step 6 and nothing else: steps 7 and 8 still
    /// run, on the direct-access fallback they have always used.
    #[test]
    fn no_systemd_still_reaches_enrollment_and_the_test() {
        let plan = resolve_setup_plan(SetupArgs {
            no_systemd: true,
            enroll: true,
            ..SetupArgs::default()
        });
        assert_eq!(plan.base, Some(BaseMode::Wizard));
        assert_eq!(systemd_step_for(&plan), SystemdStep::Skip);

        let steps = enroll_steps_for(&plan);
        assert!(steps.enroll, "step 7 runs without a daemon");
        assert!(steps.assume_yes);
        assert!(test_recognition_runs(steps, true), "step 8 runs too");
    }

    /// Oneshot mode wants no daemon started: backend selection never probes
    /// the bus under it, so the wait would buy the steps below nothing.
    #[test]
    fn the_daemon_is_only_started_for_daemon_mode() {
        use facelock_core::config::DaemonMode;
        assert!(daemon_start_wanted(&DaemonMode::Daemon));
        assert!(!daemon_start_wanted(&DaemonMode::Oneshot));
    }

    /// A second `setup` run must not enroll through the daemon the first run
    /// left behind: the config the wizard just wrote is only read at startup.
    /// The verbs are pinned because each is a no-op in the other's state —
    /// `start` on a running unit is the stale daemon, `try-restart` on a
    /// stopped one is no daemon at all.
    #[test]
    fn an_already_running_daemon_is_restarted_rather_than_left_alone() {
        assert_eq!(daemon_bring_up_for(true), DaemonBringUp::Restart);
        assert_eq!(daemon_bring_up_for(true).verb(), "try-restart");

        assert_eq!(daemon_bring_up_for(false), DaemonBringUp::Start);
        assert_eq!(daemon_bring_up_for(false).verb(), "start");
    }

    /// `try-restart --no-block` returns when systemd has *queued* the job, so
    /// the outgoing daemon goes on answering `Ping` for as long as it takes to
    /// stop it. On the restart branch a ping alone is therefore not evidence:
    /// the unit's main PID has to have become a different, non-zero one.
    #[test]
    fn a_restart_is_only_ready_once_a_different_process_answers() {
        // The outgoing instance answering its own replacement's wait.
        assert!(!daemon_ready(
            DaemonBringUp::Restart,
            Some(4242),
            Some(4242),
            true
        ));

        // The successor: a different PID, and answering.
        assert!(daemon_ready(
            DaemonBringUp::Restart,
            Some(4242),
            Some(9001),
            true
        ));

        // MainPID 0 — nothing is running, whatever answered the bus.
        assert!(!daemon_ready(
            DaemonBringUp::Restart,
            Some(4242),
            None,
            true
        ));

        // A new process that is not answering yet: still loading its models.
        assert!(!daemon_ready(
            DaemonBringUp::Restart,
            Some(4242),
            Some(9001),
            false
        ));

        // No outgoing PID to tell apart: degrades to the `Start` rule rather
        // than waiting out the timeout on a daemon that is plainly up.
        assert!(daemon_ready(DaemonBringUp::Restart, None, Some(9001), true));
    }

    /// The start branch has no outgoing instance an answer could have come
    /// from, so the ping is the whole test and the PIDs are not consulted.
    #[test]
    fn a_start_is_ready_as_soon_as_the_daemon_answers() {
        assert!(daemon_ready(DaemonBringUp::Start, None, None, true));
        assert!(daemon_ready(DaemonBringUp::Start, Some(1), Some(1), true));
        assert!(!daemon_ready(DaemonBringUp::Start, None, Some(9001), false));
    }

    /// The notice claims a replacement happened. A `systemctl` that refused
    /// the job replaced nothing, and a start was never a replacement.
    #[test]
    fn only_a_restart_systemd_accepted_is_announced() {
        assert!(restart_announced(DaemonBringUp::Restart, true));
        assert!(!restart_announced(DaemonBringUp::Restart, false));
        assert!(!restart_announced(DaemonBringUp::Start, true));
        assert!(!restart_announced(DaemonBringUp::Start, false));
    }

    // -- `--no-enroll` ------------------------------------------------------

    /// `--no-enroll` suppresses step 7 *and* step 8, which depends on it.
    #[test]
    fn no_enroll_suppresses_steps_7_and_8() {
        let plan = SetupPlan {
            enroll: Some(false),
            ..SetupPlan::default()
        };
        let steps = enroll_steps_for(&plan);

        assert!(!steps.enroll, "step 7 must not run");
        // Step 8 is gated on step 7, so it cannot run whatever `enrolled` says.
        assert!(!test_recognition_runs(steps, true));
        assert!(!test_recognition_runs(steps, false));
    }

    /// `--enroll` runs enrollment without the confirm prompt.
    #[test]
    fn enroll_flag_runs_without_the_confirm_prompt() {
        let plan = SetupPlan {
            enroll: Some(true),
            ..SetupPlan::default()
        };
        let steps = enroll_steps_for(&plan);
        assert!(steps.enroll);
        assert!(steps.assume_yes);
        assert!(test_recognition_runs(steps, true));
    }

    // -- defaults and `-y` --------------------------------------------------

    /// Nothing is suppressed by default: a bare `facelock setup` still reaches
    /// steps 6, 7, 8 and 9.
    #[test]
    fn default_plan_still_reaches_all_four_action_steps() {
        let plan = SetupPlan::default();

        let steps = enroll_steps_for(&plan);
        assert_eq!(systemd_step_for(&plan), SystemdStep::Ask, "step 6");
        assert!(steps.enroll, "step 7");
        assert!(!steps.assume_yes, "step 7 still prompts");
        assert!(test_recognition_runs(steps, true), "step 8");
        assert_eq!(pam_step_for(&plan), PamStep::Ask, "step 9");
        assert!(pam_step_for(&plan).touches_pam_d());
    }

    /// `-y` makes the step 6/7/8 confirmations take their default instead of
    /// prompting. `cargo test` gives us a non-tty stdin, so a real prompt here
    /// would fail — `Ok(true)` is proof it was suppressed.
    #[test]
    fn yes_makes_step_confirms_take_their_default() {
        let theme = ColorfulTheme::default();
        for prompt in [
            "Would you like to enroll a face now?",
            "Would you like to test recognition?",
            "Enable daemon mode with D-Bus activation?",
        ] {
            assert!(confirm_step(&theme, prompt, true).unwrap());
        }

        // `-y` is what feeds `assume_yes` at the step 7 call site; steps 6 and 8
        // are passed `plan.yes` directly.
        let plan = SetupPlan {
            yes: true,
            ..SetupPlan::default()
        };
        assert!(enroll_steps_for(&plan).assume_yes);
    }

    /// A PAM module that is not installed short-circuits step 9 before any
    /// write — the check the tests above hoist out of the writer.
    #[test]
    fn missing_pam_module_writes_nothing() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = SetupPlan::default();
        let configured =
            pam_step_in(&only(dir.path()), &plan, &ColorfulTheme::default(), false).unwrap();

        assert!(configured.is_empty());
        assert_eq!(before, hash_dir(dir.path()));
    }
}
