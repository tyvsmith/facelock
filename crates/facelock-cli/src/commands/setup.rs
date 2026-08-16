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

/// PAM service targeted by `--pam` when `--service` is not given.
pub const DEFAULT_PAM_SERVICE: &str = "sudo";

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
    Install { service: Option<String> },
    Remove { service: String, if_present: bool },
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
/// they bail immediately from their own root checks (`run_pam`, `check_root`).
/// Escalating there would be a new, surprising behavior for exactly the
/// scripted invocations that must stay byte-compatible.
pub fn needs_root_precheck(plan: &SetupPlan) -> bool {
    plan.base.is_some()
}

/// Execute a resolved plan: base setup first (if any), then the standalone
/// systemd and PAM actions, in that order — the wizard runs systemd (step 8)
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
        SystemdPref::Install => run_systemd(false)?,
        SystemdPref::Disable => run_systemd(true)?,
        // Under a wizard base `Ask` is step 8; everywhere else it means nothing.
        SystemdPref::Ask | SystemdPref::Skip => {}
    }

    match &plan.pam {
        PamPref::Remove {
            service,
            if_present,
        } => run_pam(service, true, *if_present, plan.yes)?,
        PamPref::Install { service } => {
            // The wizard's step 9 already applied `--pam`; installing again here
            // would be a second, unasked-for edit of the same file.
            if !wizard_ran {
                let service = service.as_deref().unwrap_or(DEFAULT_PAM_SERVICE);
                // `--non-interactive` promises no prompts, so the per-file
                // "Proceed?" confirmation is suppressed. It deliberately does
                // *not* bypass the SENSITIVE_SERVICES gate, which checks `--yes`.
                let no_prompt = plan.base == Some(BaseMode::NonInteractive);
                pam_install(service, plan.yes, no_prompt)?;
                print_pam_extension_hint();
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

    // -- Step 6: Face enrollment --
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

    // -- Step 7: Test recognition --
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

    // -- Step 8: Systemd setup --
    Terminal.info(&SetupMessage::SetupStepDaemon);
    let systemd_step = systemd_step_for(plan);
    let systemd_enabled = if systemd_step.wizard_may_install() {
        match wizard_systemd_setup(&theme, plan.yes) {
            Ok(enabled) => enabled,
            Err(e) => {
                Terminal.info(&SetupMessage::SystemdStepFailed {
                    error: e.to_string(),
                });
                false
            }
        }
    } else {
        if systemd_step == SystemdStep::Skip {
            Terminal.info(&SystemMessage::SystemdSkippedFlag);
        } else {
            Terminal.info(&SystemMessage::SystemdDeferred);
        }
        false
    };

    // Group membership: without it, the first daemon command a normal user
    // runs after setup fails with a bare D-Bus AccessDenied (issue #89).
    if let Err(e) = setup_group_membership(Some(&theme)) {
        Terminal.info(&SetupMessage::GroupStepFailed {
            error: e.to_string(),
        });
    }

    // -- Step 9: PAM configuration --
    Terminal.info(&SetupMessage::SetupStepPam);
    let pam_services = match pam_step_in(
        Path::new(PAM_DIR),
        plan,
        &theme,
        Path::new(PAM_MODULE_PATH).exists(),
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

    print_pam_extension_hint();

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
            SystemdStep::Deferred => SetupMessage::DaemonStatusDeferred,
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
    println!();

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
    let devices = facelock_camera::list_devices().map_err(|e| anyhow::anyhow!("{e}"))?;

    if devices.is_empty() {
        Terminal.info(&DeviceMessage::NoVideoDevices);
        return Ok(());
    }

    // Consult the quirks DB so IR classification here matches the auth path.
    // classify_ir_sources disambiguates multi-node USB cameras (e.g. the
    // Logitech BRIO's RGB + IR nodes share one VID:PID quirk): only the actual
    // IR sensor node is tagged [IR], so auto-select picks the right node.
    let quirks = facelock_camera::QuirksDb::load();
    let sources = facelock_camera::classify_ir_sources(&devices, Some(&quirks));
    let is_ir_at = |idx: usize| {
        sources
            .get(idx)
            .copied()
            .unwrap_or(facelock_camera::IrSource::None)
            != facelock_camera::IrSource::None
    };

    let ir_devices: Vec<_> = devices
        .iter()
        .enumerate()
        .filter(|(i, _)| is_ir_at(*i))
        .map(|(_, d)| d)
        .collect();

    // If exactly one IR camera, auto-select it
    if ir_devices.len() == 1 {
        let dev = ir_devices[0];
        Terminal.info(&DeviceMessage::AutoSelectedIrCamera {
            path: dev.path.clone(),
            name: dev.name.clone(),
        });
        config.device.path = Some(dev.path.clone());
        return Ok(());
    }

    // Build display list
    let display_items: Vec<String> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let ir_tag = if is_ir_at(i) { " [IR]" } else { "" };
            format!("{}{} - {}", d.path, ir_tag, d.name)
        })
        .collect();

    // Find the currently configured device index for default selection
    let default_idx = devices
        .iter()
        .position(|d| config.device.path.as_ref().is_some_and(|p| d.path == *p))
        .or_else(|| {
            // Default to first IR camera if available
            (0..devices.len()).find(|&i| is_ir_at(i))
        })
        .unwrap_or(0);

    let selection = Select::with_theme(theme)
        .with_prompt(DeviceMessage::PromptSelectCameraDevice.localized())
        .items(&display_items)
        .default(default_idx)
        .interact()?;

    let selected = &devices[selection];
    config.device.path = Some(selected.path.clone());
    Terminal.info(&DeviceMessage::SelectedCamera {
        path: selected.path.clone(),
        name: selected.name.clone(),
    });

    Ok(())
}

/// One enumerated video device plus its IR verdict.
///
/// `--camera auto` selection is expressed over this rather than over V4L2 so it
/// stays a pure function: testable on a machine with no camera at all.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CameraCandidate {
    path: String,
    name: String,
    is_ir: bool,
}

/// Pick the single IR device for `--camera auto`.
///
/// Zero and many are both errors: silently taking the first IR node would pin
/// setup to whichever device happened to enumerate first, and getting that
/// wrong means auth never works.
fn select_ir_camera(candidates: &[CameraCandidate]) -> anyhow::Result<String> {
    let ir: Vec<&CameraCandidate> = candidates.iter().filter(|c| c.is_ir).collect();

    match ir.len() {
        1 => Ok(ir[0].path.clone()),
        0 if candidates.is_empty() => Err(fail(DeviceMessage::AutoCameraNoDevices)),
        0 => {
            let listed = candidates
                .iter()
                .map(|c| format!("    {} - {}", c.path, c.name))
                .collect::<Vec<_>>()
                .join("\n");
            Err(fail(DeviceMessage::AutoCameraNoIr {
                listed,
                example: candidates[0].path.clone(),
            }))
        }
        _ => {
            let listed = ir
                .iter()
                .map(|c| format!("    {} - {}", c.path, c.name))
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

    Ok(devices
        .iter()
        .enumerate()
        .map(|(i, d)| CameraCandidate {
            path: d.path.clone(),
            name: d.name.clone(),
            is_ir: sources
                .get(i)
                .copied()
                .unwrap_or(facelock_camera::IrSource::None)
                != facelock_camera::IrSource::None,
        })
        .collect())
}

/// Apply `--camera`, persisting the result. `Auto` re-derives from hardware and
/// ignores whatever the config already says (plan §1 rule 3).
fn apply_camera_choice(config: &mut Config, choice: &CameraChoice) -> anyhow::Result<()> {
    let path = match choice {
        CameraChoice::Auto => {
            let path = select_ir_camera(&enumerate_camera_candidates()?)?;
            Terminal.info(&DeviceMessage::AutoSelectedIrCameraPath { path: path.clone() });
            path
        }
        CameraChoice::Path(path) => {
            if !Path::new(path).exists() {
                return Err(fail(DeviceMessage::CameraDeviceMissing {
                    path: path.clone(),
                }));
            }
            Terminal.info(&DeviceMessage::SelectedValue {
                value: path.clone(),
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

fn preset_summary(preset: ModelPreset) -> &'static str {
    match preset {
        ModelPreset::Standard => "Selected standard models (fast, good accuracy).",
        ModelPreset::Balanced => {
            "Selected balanced models (fast detection, high-accuracy embedding)."
        }
        ModelPreset::High => "Selected high-accuracy models (larger, ~40-50ms slower).",
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
    println!("  {}", preset_summary(preset));
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
            println!("  Detected: {}", detection.explain());
            Ok(detection.provider.as_str().to_string())
        }
        Err(e) => {
            println!("  \u{26a0} Could not query the ONNX Runtime for available providers: {e}");
            println!("    Selecting cpu. Re-run with an explicit --execution-provider once the");
            println!("    runtime is installed if you need GPU inference.");
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
        println!("  \u{26a0} NVIDIA driver not detected. Install the NVIDIA driver package");
        println!("    before starting the daemon.");
    }
    if !has_cuda_ort {
        println!("  \u{26a0} CUDA-enabled ONNX Runtime not found. Install onnxruntime-opt-cuda");
        println!("    before starting the daemon, or inference will fall back to CPU.");
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

    println!();
    println!(
        "  WARNING: encrypted face models already exist in {} but the",
        config.storage.db_path
    );
    println!("  encryption key is missing. Generating a new key would make them unreadable.");
    println!();

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
    println!("  Removed {removed} orphaned model(s).");
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
    println!("  Setting up AES-256-GCM encryption for face embeddings.");

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
        println!(
            "  TPM-sealed key already exists at {}.",
            sealed_path.display()
        );
    } else {
        handle_orphan_models_before_keygen(config, theme)?;
        println!("  Generating and sealing AES key with TPM...");
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
            println!(
                "  TPM-sealed key written to {} (permissions: 0600).",
                sealed_path.display()
            );
        }
        #[cfg(not(feature = "tpm"))]
        {
            let _ = pcr;
            anyhow::bail!("TPM support not compiled in (missing 'tpm' feature)");
        }
    }
    config.encryption.method = EncryptionMethod::Tpm;
    update_config_encryption(config, "tpm")?;
    println!("  Encryption enabled (TPM-sealed key).");
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
        println!("  Encryption key already exists at {}.", key_path.display());
    } else {
        handle_orphan_models_before_keygen(config, theme)?;
        println!("  Generating encryption key...");
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to generate encryption key")?;
        println!(
            "  Key written to {} (permissions: 0600).",
            key_path.display()
        );
    }

    config.encryption.method = EncryptionMethod::Keyfile;
    update_config_encryption(config, "keyfile")?;
    println!("  Encryption enabled.");

    Ok(())
}

/// Turn embedding encryption off, loudly.
fn setup_encryption_none(config: &mut Config) -> anyhow::Result<()> {
    use facelock_core::config::EncryptionMethod;

    config.encryption.method = EncryptionMethod::None;
    update_config_encryption(config, "none")?;

    println!("  \u{26a0} WARNING: encryption disabled (--encryption=none).");
    println!("    Biometric templates will be stored UNENCRYPTED in the database.");
    println!("    `facelock enroll` refuses to write plaintext embeddings unless");
    println!("    security.allow_plaintext is also set in the config.");
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
                println!("  TPM 2.0 detected and functional.");
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

/// Steps 6 and 7, decided up front. Step 7 exists only to exercise the
/// enrollment step 6 produced, so `--no-enroll` suppresses both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnrollSteps {
    /// Step 6 runs at all.
    enroll: bool,
    /// Step 6 proceeds without its confirmation prompt.
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

/// Step 7 runs only when step 6 actually enrolled a face.
fn test_recognition_runs(steps: EnrollSteps, enrolled: bool) -> bool {
    steps.enroll && enrolled
}

/// What step 8 does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemdStep {
    /// No flag: prompt, then install if accepted.
    Ask,
    /// `--no-systemd`: declined. No unit files, no `systemctl`.
    Skip,
    /// `--systemd` / `--systemd --disable`: already answered, and
    /// [`run_with_plan`] performs the action after the base flow — the wizard
    /// must not do it a second time.
    Deferred,
}

impl SystemdStep {
    /// Whether the wizard itself may write unit files or invoke `systemctl`.
    fn wizard_may_install(self) -> bool {
        matches!(self, SystemdStep::Ask)
    }
}

fn systemd_step_for(plan: &SetupPlan) -> SystemdStep {
    match plan.systemd {
        SystemdPref::Ask => SystemdStep::Ask,
        SystemdPref::Skip => SystemdStep::Skip,
        SystemdPref::Install | SystemdPref::Disable => SystemdStep::Deferred,
    }
}

/// What step 9 does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PamStep {
    /// No flag: today's multi-select over the detected candidates.
    Ask,
    /// `--pam [--service X]`: exactly this one service. Deliberately *not* the
    /// candidates' five `default_enabled` entries — `--pam` already means "sudo"
    /// in standalone mode, so a scripter gets one consistent meaning everywhere.
    Install(String),
    /// `--no-pam`: declined. Nothing under the PAM directory is written or
    /// backed up.
    Skip,
    /// `--pam --remove`: [`run_with_plan`] performs the removal.
    Deferred,
}

impl PamStep {
    /// Whether step 9 modifies anything under the PAM directory.
    fn touches_pam_d(&self) -> bool {
        matches!(self, PamStep::Ask | PamStep::Install(_))
    }
}

fn pam_step_for(plan: &SetupPlan) -> PamStep {
    match &plan.pam {
        PamPref::Ask => PamStep::Ask,
        PamPref::Skip => PamStep::Skip,
        PamPref::Install { service } => PamStep::Install(
            service
                .clone()
                .unwrap_or_else(|| DEFAULT_PAM_SERVICE.to_string()),
        ),
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
/// It is hoisted out of [`pam_install_in`] so the check happens once, before any
/// prompt or write, and so tests can drive the write path on a machine that has
/// no PAM module installed.
fn pam_step_in(
    base: &Path,
    plan: &SetupPlan,
    theme: &ColorfulTheme,
    module_present: bool,
) -> anyhow::Result<Vec<String>> {
    let step = pam_step_for(plan);

    if !step.touches_pam_d() {
        if step == PamStep::Skip {
            Terminal.info(&PamMessage::PamSkippedFlag {
                dir: base.display().to_string(),
            });
        }
        return Ok(Vec::new());
    }

    if !module_present {
        Terminal.info(&PamMessage::PamModuleMissing {
            path: PAM_MODULE_PATH.to_string(),
        });
        return Ok(Vec::new());
    }

    match step {
        PamStep::Install(service) => {
            Terminal.info(&PamMessage::ConfiguringPamFor {
                service: service.clone(),
            });
            pam_install_in(base, &service, plan.yes, false)?;
            Ok(vec![service])
        }
        PamStep::Ask => wizard_pam_setup_in(base, theme),
        // Both returned above; repeated here to keep the match exhaustive.
        PamStep::Skip | PamStep::Deferred => Ok(Vec::new()),
    }
}

fn wizard_pam_setup_in(base: &Path, theme: &ColorfulTheme) -> anyhow::Result<Vec<String>> {
    let candidates = candidates_in(base);

    if candidates.is_empty() {
        Terminal.info(&PamMessage::NoPamCandidates {
            dir: base.display().to_string(),
        });
        return Ok(Vec::new());
    }

    Terminal.info(&PamMessage::PamLinePreview {
        line: PAM_LINE.to_string(),
    });

    let labels: Vec<&str> = candidates.iter().map(|c| c.description).collect();
    let defaults: Vec<bool> = candidates.iter().map(|c| c.default_enabled).collect();

    let selected_services: Vec<String> = if !is_interactive() {
        // Non-TTY / non-interactive: auto-select per defaults.
        candidates
            .iter()
            .zip(defaults.iter())
            .filter(|(_, d)| **d)
            .map(|(c, _)| c.service.to_string())
            .collect()
    } else {
        let selections = MultiSelect::with_theme(theme)
            .with_prompt(PamMessage::PromptSelectPamServices.localized())
            .items(&labels)
            .defaults(&defaults)
            .interact()?;
        selections
            .into_iter()
            .map(|i| candidates[i].service.to_string())
            .collect()
    };

    let mut configured = Vec::new();
    for service in selected_services {
        Terminal.info(&PamMessage::ConfiguringPamFor {
            service: service.clone(),
        });
        // `yes = true`: the multi-select above *is* the per-service consent, and
        // no candidate is in SENSITIVE_SERVICES.
        match pam_install_in(base, &service, true, false) {
            Ok(()) => configured.push(service),
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
    println!();
    println!("==> To finish hyprlock integration, run as your normal user:");
    println!("==>     facelock hyprlock enable");
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

    println!();
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
            println!("  hyprlock integration applied for {sudo_user}.");
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
                println!("  [ok] {} ({})", entry.name, entry.purpose);
            }
            ModelStatus::Missing => {
                println!(
                    "  [download] {} (~{}MB) - {}",
                    entry.name, entry.size_mb, entry.purpose
                );
                download_model(entry, &model_path)?;
                verify_after_download(&model_path, &entry.sha256, &entry.name)?;
                println!("  [ok] {} downloaded and verified", entry.name);
            }
            ModelStatus::BadChecksum => {
                println!(
                    "  [redownload] {} - checksum mismatch, re-downloading",
                    entry.name
                );
                download_model(entry, &model_path)?;
                verify_after_download(&model_path, &entry.sha256, &entry.name)?;
                println!("  [ok] {} re-downloaded and verified", entry.name);
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

    // Ensure the facelock group exists (secure_setup_paths chowns to it) and
    // add the invoking user so daemon commands work without sudo (#89).
    setup_group_membership(None)?;

    secure_setup_paths(&config, Some(&manifest))?;
    write_setup_marker()?;

    // Enrollment only happens here when it was explicitly asked for: `None` and
    // `--no-enroll` both mean "do nothing", which is what this flow has always
    // done. `--enroll` runs unattended — `enroll::run` prompts for nothing once
    // the setup marker exists.
    let enrolled = plan.enroll == Some(true);
    if enrolled {
        println!("\nEnrolling face...");
        super::enroll::run(&config, None, None, true)?;
    }

    // See the matching call in `run_wizard`.
    if let Err(e) = super::enrollment_marker::reconcile_all(&config) {
        tracing::warn!("could not reconcile enrollment markers: {e}");
    }

    if let Ok(pam_content) = fs::read_to_string(Path::new("/etc/pam.d/hyprlock"))
        && pam_content.lines().any(is_facelock_pam_line)
    {
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
        println!(
            "  Encryption already configured ({:?}).",
            config.encryption.method
        );
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
                println!(
                    "  [ok] Generated TPM-sealed encryption key at {}",
                    sealed_path.display()
                );
            }
            let mut config = config.clone();
            config.encryption.method = EncryptionMethod::Tpm;
            update_config_encryption(&config, "tpm")?;
            println!("  [ok] AES-256-GCM encryption enabled (TPM-sealed key).");
            return Ok(());
        }
    }

    // Fall back to keyfile
    let key_path = Path::new(&config.encryption.key_path);
    if !key_path.exists() {
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to generate encryption key")?;
        println!("  [ok] Generated encryption key at {}", key_path.display());
    }

    let mut config = config.clone();
    config.encryption.method = EncryptionMethod::Keyfile;
    update_config_encryption(&config, "keyfile")?;
    println!("  [ok] AES-256-GCM encryption enabled.");
    Ok(())
}

#[cfg(unix)]
fn chown_path(path: &Path, uid: u32, gid: u32) -> anyhow::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains embedded NUL: {}", path.display()))?;
    let result = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
    if result != 0 {
        anyhow::bail!(
            "failed to chown {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn chown_path(_path: &Path, _uid: u32, _gid: u32) -> anyhow::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// facelock group membership
// ---------------------------------------------------------------------------

/// Resolve the invoking (non-root) user behind `sudo facelock setup`.
fn invoking_user() -> Option<String> {
    let valid = |u: &String| !u.is_empty() && u != "root";
    std::env::var("SUDO_USER")
        .ok()
        .filter(valid)
        .or_else(|| std::env::var("DOAS_USER").ok().filter(valid))
}

/// True if `user` is a member of `group` (supplementary or primary).
fn user_in_group(user: &str, group: &nix::unistd::Group) -> bool {
    if group.mem.iter().any(|m| m == user) {
        return true;
    }
    // Primary-group membership is recorded in passwd, not in group.mem.
    matches!(nix::unistd::User::from_name(user), Ok(Some(u)) if u.gid == group.gid)
}

/// Ensure the `facelock` system group exists and the invoking user is in it.
///
/// The D-Bus system-bus policy admits only root and the `facelock` group, so
/// without membership the first `facelock preview`/`test` after setup fails
/// with AccessDenied (issue #89). Packaging creates the group but cannot know
/// which human user to add — setup is the right place. Pass `theme` for an
/// interactive Y/n prompt; `None` adds the invoking sudo/doas user without
/// prompting, and prints a manual `usermod` command when no invoking user can
/// be determined.
fn setup_group_membership(theme: Option<&ColorfulTheme>) -> anyhow::Result<()> {
    // Create the system group if packaging didn't (e.g. source installs).
    if nix::unistd::Group::from_name("facelock")
        .context("failed to look up facelock group")?
        .is_none()
    {
        Terminal.info(&SystemMessage::CreatingFacelockGroup);
        run_cmd("groupadd", &["-r", "facelock"])?;
    }
    let group = nix::unistd::Group::from_name("facelock")
        .context("failed to look up facelock group")?
        .context("facelock group missing after creation")?;

    let Some(user) = invoking_user() else {
        Terminal.info(&SystemMessage::GroupMembershipNote);
        return Ok(());
    };

    if user_in_group(&user, &group) {
        Terminal.info(&SystemMessage::AlreadyInGroup { user: user.clone() });
        return Ok(());
    }

    if let Some(theme) = theme {
        // Propagate prompt failures instead of treating them as "declined":
        // the wizard's caller prints the error plus the manual usermod command,
        // so a broken stdin surfaces rather than silently skipping the add.
        let proceed = Confirm::with_theme(theme)
            .with_prompt(SystemMessage::ConfirmAddToGroup { user: user.clone() }.localized())
            .default(true)
            .interact()
            .context("group membership prompt failed")?;
        if !proceed {
            Terminal.info(&SystemMessage::GroupAddSkipped { user: user.clone() });
            return Ok(());
        }
    }

    run_cmd("usermod", &["-aG", "facelock", &user])?;
    Terminal.info(&SystemMessage::AddedToGroup { user: user.clone() });
    Ok(())
}

fn facelock_group_gid() -> anyhow::Result<u32> {
    nix::unistd::Group::from_name("facelock")
        .context("failed to look up facelock group")?
        .map(|group| group.gid.as_raw())
        .context(
            "facelock group is missing; install package assets or create the facelock system group",
        )
}

fn secure_existing_path(path: &Path, mode: u32, gid: u32) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    ensure_mode(path, mode)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;

    if nix::unistd::Uid::current().is_root() {
        chown_path(path, 0, gid)?;
    }

    Ok(())
}

fn secure_dir_if_exists(path: &Path, mode: u32, gid: u32) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if !path.is_dir() {
        bail!(
            "expected directory but found non-directory path: {}",
            path.display()
        );
    }

    ensure_private_dir(path, mode)
        .with_context(|| format!("failed to secure directory {}", path.display()))?;
    if nix::unistd::Uid::current().is_root() {
        chown_path(path, 0, gid)?;
    }

    Ok(())
}

fn secure_setup_paths(config: &Config, manifest: Option<&ModelManifest>) -> anyhow::Result<()> {
    let facelock_gid = facelock_group_gid()?;
    let config_path = facelock_core::paths::config_path();
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/facelock"));
    let db_path = Path::new(&config.storage.db_path);
    let audit_path = Path::new(&config.audit.path);
    let key_path = Path::new(&config.encryption.key_path);
    let sealed_key_path = Path::new(&config.encryption.sealed_key_path);

    secure_dir_if_exists(config_dir, 0o755, 0)?;
    // Snapshots hold raw face images: root-only, no group access.
    secure_dir_if_exists(Path::new(&config.snapshots.dir), 0o700, 0)?;

    // The state directory subtree — state dir, models/, enrolled/, and the
    // database file modes — is owned by `state_layout`. Re-applied here
    // because setup only creates the `facelock` group partway through, so the
    // earlier call could not chown.
    if let Some(layout) = crate::state_layout::StateLayout::from_config(config) {
        let owners = nix::unistd::Uid::current()
            .is_root()
            .then_some(crate::state_layout::Owners { facelock_gid });
        crate::state_layout::apply_layout(&layout, owners)?;
    }
    if let Some(parent) = audit_path.parent() {
        // Per-user auth history: root-only, like the snapshots.
        secure_dir_if_exists(parent, 0o700, 0)?;
    }
    if let Some(parent) = key_path.parent() {
        secure_dir_if_exists(parent, 0o755, 0)?;
    }
    if let Some(parent) = sealed_key_path.parent() {
        secure_dir_if_exists(parent, 0o755, 0)?;
    }

    secure_existing_path(&config_path, 0o644, 0)?;
    secure_existing_path(db_path, 0o600, 0)?;
    secure_existing_path(audit_path, 0o600, 0)?;
    secure_existing_path(key_path, 0o600, 0)?;
    secure_existing_path(sealed_key_path, 0o600, 0)?;
    secure_existing_path(Path::new(SETUP_COMPLETE_MARKER), 0o644, 0)?;

    if let Some(manifest) = manifest {
        for entry in &manifest.models {
            let model_path = Path::new(&config.daemon.model_dir).join(&entry.filename);
            secure_existing_path(&model_path, 0o644, 0)?;
        }
    }

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

    println!("  Directories created.");
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
    println!("  Created default config at {}", config_path.display());
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
//   - system-auth (Arch) and common-auth (Debian): shared stacks included by many
//     services. Editing them gives face auth to passwd, su, chsh, etc. — risky.
//   - login: TTY login. Cameras often aren't initialized at boot.
//   - su, passwd, chsh, chfn: privilege/credential-change tools that should
//     require a real password.
//   - any unknown service: detection is by /etc/pam.d/<name> existence, but only
//     services in PAM_CANDIDATES are offered.
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

/// The real PAM configuration directory. `candidates_in` and `pam_install_in`
/// take it as a parameter so tests can point them at a tempdir instead.
const PAM_DIR: &str = "/etc/pam.d";

/// Returns candidates from `PAM_CANDIDATES` whose service file exists under `base`.
fn candidates_in(base: &Path) -> Vec<&'static PamCandidate> {
    PAM_CANDIDATES
        .iter()
        .filter(|c| base.join(c.service).exists())
        .collect()
}

// --- PAM installation ---

const PAM_LINE: &str = "auth      sufficient pam_facelock.so";

/// Print a copy-pasteable hint so users can extend PAM integration to any service.
fn print_pam_extension_hint() {
    println!();
    println!("==> facelock PAM line for manual extension to other services:");
    println!("==>   {PAM_LINE}");
    println!(
        "==> Add the above line above the first 'auth' line in any /etc/pam.d/<service> file."
    );
}

const PAM_MODULE_PATH: &str = "/lib/security/pam_facelock.so";
const SENSITIVE_SERVICES: &[&str] = &["system-auth", "login", "sshd"];

/// Check if a PAM config line references pam_facelock, regardless of spacing.
fn is_facelock_pam_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.starts_with('#') && trimmed.contains("pam_facelock.so")
}

pub fn run_pam(service: &str, remove: bool, if_present: bool, yes: bool) -> anyhow::Result<()> {
    // 1. Check root
    if !nix::unistd::Uid::current().is_root() {
        bail!("PAM configuration requires root. Run with sudo.");
    }

    if remove {
        pam_remove(service, if_present)
    } else {
        pam_install(service, yes, false)?;
        print_pam_extension_hint();
        Ok(())
    }
}

/// [`pam_install_in`] against the real [`PAM_DIR`], plus the module-presence
/// precondition that the parameterized form leaves to its caller.
///
/// Root is re-checked here rather than only in [`run_pam`]: this function edits
/// `/etc/pam.d`, and it is reachable both from `run_pam` and directly from
/// `run_with_plan`. The parameterized [`pam_install_in`] deliberately does not
/// check, so tests can drive the write path against a tempdir unprivileged.
fn pam_install(service: &str, yes: bool, no_prompt: bool) -> anyhow::Result<()> {
    // 1. Check root
    if !nix::unistd::Uid::current().is_root() {
        bail!("PAM configuration requires root. Run with sudo.");
    }

    // 2. Check PAM module exists
    if !Path::new(PAM_MODULE_PATH).exists() {
        bail!(
            "PAM module not found at {PAM_MODULE_PATH}.\n\
             Install it first: cargo build --release -p pam-facelock && \
             sudo cp target/release/libpam_facelock.so {PAM_MODULE_PATH}"
        );
    }

    pam_install_in(Path::new(PAM_DIR), service, yes, no_prompt)
}

/// Insert the facelock PAM line into `<base>/<service>`.
///
/// `yes` gates [`SENSITIVE_SERVICES`]; `no_prompt` only suppresses the per-file
/// "Proceed?" confirmation, because `--non-interactive` promises no prompts but
/// must not weaken the sensitive-service gate.
///
/// The caller is responsible for checking that the PAM module is installed.
fn pam_install_in(base: &Path, service: &str, yes: bool, no_prompt: bool) -> anyhow::Result<()> {
    // 3. Refuse sensitive services without --yes
    if SENSITIVE_SERVICES.contains(&service) && !yes {
        bail!(
            "Refusing to modify '{service}' without --yes flag.\n\
             This is a sensitive PAM service. Use: facelock setup --pam --service {service} --yes"
        );
    }

    let pam_file = base.join(service);
    let pam_path = pam_file.display().to_string();
    let pam_file = pam_file.as_path();

    if !pam_file.exists() {
        bail!("PAM service file not found: {pam_path}");
    }

    // Read existing content
    let content =
        fs::read_to_string(pam_file).with_context(|| format!("failed to read {pam_path}"))?;

    // Check idempotency — match on the module name, not exact spacing
    if content.lines().any(is_facelock_pam_line) {
        Terminal.info(&PamMessage::PamLineAlreadyPresent {
            path: pam_path.clone(),
        });
        return Ok(());
    }

    // 4. Preview change and confirm before any modification
    let backup_path = format!("{pam_path}.facelock-backup");

    // Decide where the line will go BEFORE prompting, so the preview is accurate.
    let insertion_hint = if content.lines().any(|l| l.trim_start().starts_with("auth")) {
        PamMessage::PamInsertBeforeAuthHint
    } else {
        PamMessage::PamInsertAtTopHint
    };

    Terminal.info(&PamMessage::PamModifyPreview {
        path: pam_path.clone(),
        line: PAM_LINE.to_string(),
        hint: insertion_hint.localized(),
        backup: backup_path.clone(),
    });

    let proceed = if yes || no_prompt || !std::io::stdin().is_terminal() {
        true
    } else {
        Confirm::new()
            .with_prompt(PamMessage::ConfirmProceed.localized())
            .default(true)
            .interact()
            .context("failed to read confirmation")?
    };

    if !proceed {
        Terminal.info(&PamMessage::PamSkippedFile {
            path: pam_path.clone(),
        });
        return Ok(());
    }

    // 4b. Create backup (always, before any modification)
    fs::copy(pam_file, &backup_path)
        .with_context(|| format!("failed to back up {pam_path} to {backup_path}"))?;
    Terminal.info(&PamMessage::PamBackedUp {
        path: pam_path.clone(),
        backup: backup_path.clone(),
    });

    // 5. Prepend PAM line before first auth line
    let mut new_lines: Vec<String> = Vec::new();
    let mut inserted = false;

    for line in content.lines() {
        if !inserted && line.trim_start().starts_with("auth") {
            new_lines.push(PAM_LINE.to_string());
            inserted = true;
        }
        new_lines.push(line.to_string());
    }

    if !inserted {
        // No auth line found; append at the top
        new_lines.insert(0, PAM_LINE.to_string());
    }

    // Preserve trailing newline
    let mut output = new_lines.join("\n");
    if content.ends_with('\n') {
        output.push('\n');
    }

    fs::write(pam_file, &output).with_context(|| format!("failed to write {pam_path}"))?;

    Terminal.info(&PamMessage::PamInstalled {
        path: pam_path.clone(),
        backup: backup_path.clone(),
        service: service.to_string(),
    });

    Ok(())
}

fn pam_remove(service: &str, if_present: bool) -> anyhow::Result<()> {
    pam_remove_in(Path::new(PAM_DIR), service, if_present)
}

fn pam_remove_in(base: &Path, service: &str, if_present: bool) -> anyhow::Result<()> {
    let pam_file = base.join(service.trim_start_matches('/'));
    let pam_path = pam_file.display().to_string();

    let content = match fs::read_to_string(&pam_file) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if if_present {
                Terminal.info(&PamMessage::PamServiceAbsent {
                    path: pam_path.clone(),
                });
                return Ok(());
            }
            bail!("PAM service file not found: {pam_path}");
        }
        Err(error) => return Err(error).with_context(|| format!("failed to read {pam_path}")),
    };

    let original_count = content.lines().count();
    let new_lines: Vec<&str> = content
        .lines()
        .filter(|line| !is_facelock_pam_line(line))
        .collect();

    if new_lines.len() == original_count {
        Terminal.info(&PamMessage::PamNoLineFound {
            path: pam_path.clone(),
        });
    } else {
        let mut output = new_lines.join("\n");
        if content.ends_with('\n') {
            output.push('\n');
        }

        fs::write(&pam_file, &output).with_context(|| format!("failed to write {pam_path}"))?;
        Terminal.info(&PamMessage::PamRemoved {
            path: pam_path.clone(),
        });
    }

    // Offer backup restore
    let backup_path = format!("{pam_path}.facelock-backup");
    if Path::new(&backup_path).exists() {
        Terminal.info(&PamMessage::PamBackupExists {
            path: pam_path.clone(),
            backup: backup_path.clone(),
        });
    }

    Ok(())
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
    println!("  Refreshed legacy {}", path.display());
    Ok(())
}

pub fn run_systemd(disable: bool) -> anyhow::Result<()> {
    check_root()?;
    check_systemd()?;

    if disable {
        println!("Disabling facelock-daemon systemd units...");
        run_cmd("systemctl", &["disable", "--now", "facelock-daemon"])?;
        println!("facelock-daemon service disabled and stopped.");
    } else {
        println!("Installing facelock-daemon systemd and D-Bus units...");

        // Install systemd service unit
        let unit_dir = Path::new(SYSTEMD_UNIT_DIR);
        fs::create_dir_all(unit_dir)
            .with_context(|| format!("failed to create {SYSTEMD_UNIT_DIR}"))?;

        let service_path = unit_dir.join(SERVICE_FILENAME);
        write_file(&service_path, SERVICE_UNIT.as_bytes(), 0o644)
            .with_context(|| format!("failed to write {}", service_path.display()))?;
        println!("  Wrote {}", service_path.display());
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
        println!("  Wrote {}", policy_path.display());
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
        println!("  Wrote {}", dbus_svc_path.display());
        refresh_legacy_copy_if_present(
            Path::new(LEGACY_DBUS_SYSTEM_SERVICE_PATH),
            DBUS_SERVICE,
            "org.facelock.Daemon",
        )?;

        run_cmd("systemctl", &["daemon-reload"])?;
        println!("  systemctl daemon-reload done.");

        run_cmd("systemctl", &["enable", SERVICE_FILENAME])?;
        println!("  systemctl enable {SERVICE_FILENAME} done.");

        println!("\nfacelock-daemon D-Bus activation is now enabled.");
    }

    Ok(())
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

    fn cam(path: &str, name: &str, is_ir: bool) -> CameraCandidate {
        CameraCandidate {
            path: path.to_string(),
            name: name.to_string(),
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
            cam("/dev/video0", "Integrated Camera", false),
            cam("/dev/video2", "Integrated IR Camera", true),
        ];
        assert_eq!(select_ir_camera(&candidates).unwrap(), "/dev/video2");
    }

    #[test]
    fn camera_auto_errors_when_no_ir_device() {
        let candidates = [cam("/dev/video0", "Integrated Camera", false)];
        let err = select_ir_camera(&candidates).unwrap_err().to_string();
        assert!(err.contains("no IR-capable camera"), "{err}");
        // The message has to be actionable: name what was found and the way out.
        assert!(err.contains("/dev/video0"), "{err}");
        assert!(err.contains("--camera="), "{err}");
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
            cam("/dev/video2", "Integrated IR Camera", true),
            cam("/dev/video4", "BRIO IR", true),
        ];
        let err = select_ir_camera(&candidates).unwrap_err().to_string();
        assert!(err.contains("2 IR-capable cameras"), "{err}");
        assert!(
            err.contains("/dev/video2") && err.contains("/dev/video4"),
            "{err}"
        );
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
    fn pam_insert_before_first_auth_line() {
        let original = "\
#%PAM-1.0
auth    include   system-local-login
auth    include   system-login
account include   system-login
";
        // Simulate the insertion logic
        let mut new_lines: Vec<String> = Vec::new();
        let mut inserted = false;
        for line in original.lines() {
            if !inserted && line.trim_start().starts_with("auth") {
                new_lines.push(PAM_LINE.to_string());
                inserted = true;
            }
            new_lines.push(line.to_string());
        }
        let mut output = new_lines.join("\n");
        if original.ends_with('\n') {
            output.push('\n');
        }

        assert!(inserted);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines[0], "#%PAM-1.0");
        assert_eq!(lines[1], PAM_LINE);
        assert_eq!(lines[2], "auth    include   system-local-login");
    }

    #[test]
    fn pam_idempotent_detection() {
        // Exact match
        let content = format!("#%PAM-1.0\n{PAM_LINE}\nauth    include   system-login\n");
        assert!(content.lines().any(is_facelock_pam_line));

        // Different spacing should still match
        let content2 =
            "#%PAM-1.0\nauth  sufficient  pam_facelock.so\nauth    include   system-login\n";
        assert!(content2.lines().any(is_facelock_pam_line));

        // Commented-out line should not match
        let content3 =
            "#%PAM-1.0\n#auth sufficient pam_facelock.so\nauth    include   system-login\n";
        assert!(!content3.lines().any(is_facelock_pam_line));
    }

    #[test]
    fn pam_remove_filters_line() {
        // Should remove regardless of spacing
        let content = "#%PAM-1.0\nauth  sufficient  pam_facelock.so\nauth    include   system-login\naccount include   system-login\n";
        let new_lines: Vec<&str> = content
            .lines()
            .filter(|line| !is_facelock_pam_line(line))
            .collect();
        assert_eq!(new_lines.len(), 3);
        assert!(!new_lines.iter().any(|l| is_facelock_pam_line(l)));
    }

    #[test]
    fn sensitive_services_detected() {
        assert!(SENSITIVE_SERVICES.contains(&"system-auth"));
        assert!(SENSITIVE_SERVICES.contains(&"login"));
        assert!(SENSITIVE_SERVICES.contains(&"sshd"));
        assert!(!SENSITIVE_SERVICES.contains(&"sudo"));
    }

    #[test]
    fn detect_candidates_filters_by_presence() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("sudo"), "").unwrap();
        std::fs::write(tmp.path().join("hyprlock"), "").unwrap();
        let found: Vec<_> = candidates_in(tmp.path())
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
            candidates_in(base)
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
            "common-auth",
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

    /// The uninstall paths of all four packagers each hardcode the set of
    /// `/etc/pam.d/<service>` files to strip `pam_facelock.so` out of, because
    /// they are shell run by a package manager and cannot read
    /// `PAM_CANDIDATES`. That duplication has already rotted once: the lists
    /// covered three services while setup offered eight, so five services kept
    /// a stale facelock line after the package was removed. Nothing failed
    /// loudly — the drift is only visible on an uninstall nobody runs in CI.
    ///
    /// This pins the direction that matters: every service setup *offers* must
    /// be *covered* by every packager. It is deliberately a superset check and
    /// not set equality — the packaging lists also carry `SENSITIVE_SERVICES`
    /// (reachable via `setup --pam --service <name> --yes`) and services that
    /// are not candidates yet, and naming a service that no host has is inert
    /// because every loop is guarded by `[ -f "$PAM_FILE" ]`. Equality would
    /// turn "packaging cleans up more than setup writes" — always safe — into
    /// a failure, and would break whenever a candidate lands in one branch and
    /// the packaging list in another.
    #[test]
    fn packaging_uninstall_covers_every_pam_candidate() {
        // Assembled so this test's own prose is not what it matches on.
        let decl = format!("FACELOCK_PAM_SERVICES={}", '"');
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

        for rel in [
            "dist/facelock.install",
            "dist/facelock.spec",
            "dist/debian/prerm",
            "justfile",
        ] {
            let path = root.join(rel);
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{rel} must be readable from the workspace: {e}"));

            let listed: Vec<&str> = source
                .lines()
                .find_map(|l| l.trim_start().strip_prefix(&decl)?.split_once('"'))
                .map(|(value, _)| value.split_whitespace().collect())
                .unwrap_or_else(|| {
                    panic!(
                        "{rel} must declare its PAM service list on one line as \
                         FACELOCK_PAM_SERVICES=\"svc svc ...\". It is the single \
                         literal both uninstall loops (pam_facelock.so lines and \
                         .facelock-backup files) read, and this test's only handle \
                         on it."
                    )
                });

            for candidate in PAM_CANDIDATES {
                assert!(
                    listed.contains(&candidate.service),
                    "{rel} does not clean up /etc/pam.d/{svc}, but `facelock setup` \
                     offers to write a pam_facelock.so line into it. Uninstalling \
                     would leave that line behind, pointing at a module that is no \
                     longer on disk. Add {svc} to FACELOCK_PAM_SERVICES in {rel} \
                     (naming a service the host lacks is inert — the loops skip \
                     missing files). Currently listed there: {listed:?}",
                    svc = candidate.service,
                );
            }
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
    #[test]
    fn no_pam_leaves_every_pam_file_byte_identical() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = pam_plan(PamPref::Skip);
        let configured = pam_step_in(dir.path(), &plan, &ColorfulTheme::default(), true).unwrap();

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
            })
        };
        let configured = pam_step_in(dir.path(), &plan, &ColorfulTheme::default(), true).unwrap();

        assert_eq!(configured, vec!["sudo".to_string()]);
        let after = hash_dir(dir.path());
        assert_ne!(before["sudo"], after["sudo"], "sudo must have changed");
        assert!(
            after.contains_key("sudo.facelock-backup"),
            "a backup file must have appeared: {after:?}"
        );
        assert!(
            fs::read_to_string(dir.path().join("sudo"))
                .unwrap()
                .lines()
                .any(is_facelock_pam_line)
        );
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
            pam_step_in(dir.path(), &plan, &ColorfulTheme::default(), true)
                .unwrap()
                .is_empty()
        );
        assert_eq!(before, hash_dir(dir.path()));
    }

    #[test]
    fn pam_remove_if_present_missing_service_is_a_noop() {
        let dir = tempfile::TempDir::new().unwrap();
        let before = hash_dir(dir.path());

        pam_remove_in(dir.path(), "omarchy-lock-face", true).unwrap();

        assert_eq!(before, hash_dir(dir.path()));
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn pam_remove_missing_service_without_if_present_still_errors() {
        let dir = tempfile::TempDir::new().unwrap();

        let error = pam_remove_in(dir.path(), "omarchy-lock-face", false).unwrap_err();

        let rendered = error.to_string();
        assert!(rendered.contains("PAM service file not found:"));
        assert!(rendered.contains("omarchy-lock-face"));
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn pam_remove_if_present_removes_an_existing_facelock_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let pam_file = dir.path().join("omarchy-lock-face");
        fs::write(
            &pam_file,
            format!("#%PAM-1.0\n{PAM_LINE}\nauth include system-auth\n"),
        )
        .unwrap();

        pam_remove_in(dir.path(), "omarchy-lock-face", true).unwrap();

        assert_eq!(
            fs::read_to_string(pam_file).unwrap(),
            "#%PAM-1.0\nauth include system-auth\n"
        );
    }

    #[test]
    fn pam_remove_if_present_preserves_a_file_without_a_facelock_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let pam_file = dir.path().join("omarchy-lock-face");
        let original = b"#%PAM-1.0\nauth include system-auth\n";
        fs::write(&pam_file, original).unwrap();

        pam_remove_in(dir.path(), "omarchy-lock-face", true).unwrap();

        assert_eq!(fs::read(pam_file).unwrap(), original);
    }

    #[test]
    fn pam_remove_if_present_does_not_suppress_other_read_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let before = hash_dir(dir.path());

        let error = pam_remove_in(dir.path(), ".", true).unwrap_err();

        assert!(error.to_string().contains("failed to read"));
        assert_eq!(before, hash_dir(dir.path()));
    }

    #[test]
    fn pam_remove_absolute_service_stays_anchored_under_base() {
        let dir = tempfile::TempDir::new().unwrap();
        let base = dir.path().join("pam.d");
        fs::create_dir(&base).unwrap();
        let absolute_target = dir.path().join("outside-service");
        let original = format!("#%PAM-1.0\n{PAM_LINE}\nauth include system-auth\n");
        fs::write(&absolute_target, &original).unwrap();

        pam_remove_in(&base, absolute_target.to_str().unwrap(), true).unwrap();

        assert_eq!(fs::read_to_string(absolute_target).unwrap(), original);
        assert!(fs::read_dir(base).unwrap().next().is_none());
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
            })
        };
        let configured = pam_step_in(dir.path(), &plan, &ColorfulTheme::default(), true).unwrap();

        assert_eq!(configured, vec!["hyprlock".to_string()]);
        let after = hash_dir(dir.path());
        assert_ne!(before["hyprlock"], after["hyprlock"]);
        assert_eq!(before["sudo"], after["sudo"]);
        assert_eq!(before["polkit-1"], after["polkit-1"]);
    }

    /// Bare `--pam` means `sudo`, not the candidates' `default_enabled` set.
    #[test]
    fn pam_without_service_means_sudo_not_the_default_enabled_set() {
        let plan = pam_plan(PamPref::Install { service: None });
        assert_eq!(
            pam_step_for(&plan),
            PamStep::Install(DEFAULT_PAM_SERVICE.to_string())
        );

        let dir = fake_pam_d();
        let before = hash_dir(dir.path());
        let configured = pam_step_in(dir.path(), &plan, &ColorfulTheme::default(), true).unwrap();
        assert_eq!(configured, vec![DEFAULT_PAM_SERVICE.to_string()]);

        // hyprlock is `default_enabled` but was not named, so it is untouched.
        let after = hash_dir(dir.path());
        assert_eq!(before["hyprlock"], after["hyprlock"]);
        assert_eq!(before["polkit-1"], after["polkit-1"]);
    }

    /// The sensitive-service gate still requires `--yes`, and refusing it
    /// writes nothing.
    #[test]
    fn sensitive_service_refused_without_yes() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(
            dir.path().join("sshd"),
            "#%PAM-1.0\nauth\t\tinclude\t\tsystem-auth\n",
        )
        .unwrap();
        let before = hash_dir(dir.path());

        // `no_prompt` (what `--non-interactive` sets) must not weaken the gate.
        let err = pam_install_in(dir.path(), "sshd", false, true).unwrap_err();
        assert!(err.to_string().contains("Refusing to modify 'sshd'"));
        assert_eq!(before, hash_dir(dir.path()));
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

        // `wizard_systemd_setup` is the wizard's only path to unit files and
        // `systemctl`, and it runs only when `wizard_may_install()` holds.
        assert!(!step.wizard_may_install());
        // `run_with_plan` acts only on Install/Disable.
        assert!(!matches!(
            plan.systemd,
            SystemdPref::Install | SystemdPref::Disable
        ));
    }

    /// `--systemd` is applied by `run_with_plan`, so step 8 must defer rather
    /// than install a second time.
    #[test]
    fn systemd_flag_is_deferred_to_run_with_plan() {
        for pref in [SystemdPref::Install, SystemdPref::Disable] {
            let plan = SetupPlan {
                systemd: pref,
                ..SetupPlan::default()
            };
            let step = systemd_step_for(&plan);
            assert_eq!(step, SystemdStep::Deferred);
            assert!(!step.wizard_may_install());
        }
    }

    // -- `--no-enroll` ------------------------------------------------------

    /// `--no-enroll` suppresses step 6 *and* step 7, which depends on it.
    #[test]
    fn no_enroll_suppresses_steps_6_and_7() {
        let plan = SetupPlan {
            enroll: Some(false),
            ..SetupPlan::default()
        };
        let steps = enroll_steps_for(&plan);

        assert!(!steps.enroll, "step 6 must not run");
        // Step 7 is gated on step 6, so it cannot run whatever `enrolled` says.
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
        assert!(steps.enroll, "step 6");
        assert!(!steps.assume_yes, "step 6 still prompts");
        assert!(test_recognition_runs(steps, true), "step 7");
        assert_eq!(systemd_step_for(&plan), SystemdStep::Ask, "step 8");
        assert!(systemd_step_for(&plan).wizard_may_install());
        assert_eq!(pam_step_for(&plan), PamStep::Ask, "step 9");
        assert!(pam_step_for(&plan).touches_pam_d());
    }

    /// ...and step 9 under a default plan really does write. Under `cargo test`
    /// stdin is not a terminal, so it falls back to the candidates'
    /// `default_enabled` set — which is exactly what `--no-pam` must prevent.
    #[test]
    fn default_plan_still_configures_pam() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = SetupPlan::default();
        let configured = pam_step_in(dir.path(), &plan, &ColorfulTheme::default(), true).unwrap();

        assert!(configured.contains(&"sudo".to_string()));
        assert!(configured.contains(&"hyprlock".to_string()));
        assert_ne!(before, hash_dir(dir.path()));
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

        // `-y` is what feeds `assume_yes` at the step 6 call site; steps 7 and 8
        // are passed `plan.yes` directly.
        let plan = SetupPlan {
            yes: true,
            ..SetupPlan::default()
        };
        assert!(enroll_steps_for(&plan).assume_yes);
    }

    /// A PAM module that is not installed short-circuits step 9 before any
    /// write — the check the tests above hoist out of `pam_install_in`.
    #[test]
    fn missing_pam_module_writes_nothing() {
        let dir = fake_pam_d();
        let before = hash_dir(dir.path());

        let plan = SetupPlan::default();
        let configured = pam_step_in(dir.path(), &plan, &ColorfulTheme::default(), false).unwrap();

        assert!(configured.is_empty());
        assert_eq!(before, hash_dir(dir.path()));
    }

    /// `pam_install` edits `/etc/pam.d` and is reachable from `run_with_plan`
    /// without going through `run_pam`, so it must refuse non-root itself.
    /// Regression: routing standalone `--pam` through `pam_install` once let an
    /// unprivileged `facelock setup --pam` read and report on `/etc/pam.d/sudo`.
    #[test]
    fn pam_install_refuses_without_root() {
        if nix::unistd::Uid::current().is_root() {
            return; // the check cannot fire; nothing to assert
        }
        let err = pam_install("sudo", true, true).unwrap_err().to_string();
        assert!(
            err.contains("requires root"),
            "expected the root refusal before any other check, got: {err}"
        );
    }
}
