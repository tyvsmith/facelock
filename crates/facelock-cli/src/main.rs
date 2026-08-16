//! The `facelock` binary: clap wiring plus top-level dispatch into the
//! `facelock_cli` library. The domain layer (backend, health, message,
//! resolved, logging, …) lives in `lib.rs` so it stays testable and shareable
//! (gap D6); this file keeps only the `Cli`/`Commands` types and `main`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use facelock_cli::commands::TpmCommand;
use facelock_cli::commands::bench::BenchCommand;
use facelock_cli::commands::hyprlock::HyprlockCommand;
use facelock_cli::commands::setup::{
    EncryptionChoice, ExecutionProviderChoice, ModelPreset, SetupArgs, resolve_setup_plan,
};
use facelock_cli::{commands, logging, message, notifications, resolved};

#[derive(Parser)]
#[command(name = "facelock", about = "Linux face authentication", version)]
struct Cli {
    /// Path to config file
    #[arg(long, global = true)]
    config: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download models and create directories
    Setup {
        /// Run in non-interactive mode (skip wizard)
        #[arg(long)]
        non_interactive: bool,
        /// Skip confirmation prompts, e.g. before modifying PAM files (also: --no-confirm)
        #[arg(short, long, alias = "no-confirm")]
        yes: bool,

        // -- Action pairs. A later flag wins over an earlier one, so a wrapper
        //    script can append an override to a command it did not construct.
        /// Install or manage PAM module configuration
        #[arg(long, overrides_with = "no_pam")]
        pam: bool,
        /// Do not touch PAM configuration at all (no prompt, no write)
        #[arg(long = "no-pam", overrides_with = "pam")]
        no_pam: bool,
        /// Install and enable systemd units
        #[arg(long, overrides_with = "no_systemd")]
        systemd: bool,
        /// Do not install or enable systemd units
        #[arg(long = "no-systemd", overrides_with = "systemd")]
        no_systemd: bool,
        /// Enroll a face during setup
        #[arg(long, overrides_with = "no_enroll")]
        enroll: bool,
        /// Do not enroll a face during setup
        #[arg(long = "no-enroll", overrides_with = "enroll")]
        no_enroll: bool,

        // -- Action modifiers
        /// Used with --systemd: disable and stop systemd units instead
        #[arg(long, requires = "systemd")]
        disable: bool,
        /// Used with --pam: target PAM service (default: sudo)
        #[arg(long, requires = "pam")]
        service: Option<String>,
        /// Used with --pam: remove the PAM line instead of adding it
        #[arg(long, requires = "pam")]
        remove: bool,
        /// Used with --pam --remove: treat an absent service file as success
        #[arg(long = "if-present", requires = "remove")]
        if_present: bool,

        // -- Choice flags. Supplying a value answers the question, and so skips
        //    the matching wizard step.
        /// Camera device path, or `auto` to re-detect from hardware
        #[arg(long)]
        camera: Option<String>,
        /// Model quality preset
        #[arg(long, value_enum)]
        models: Option<ModelPreset>,
        /// ONNX Runtime execution provider
        #[arg(long, value_enum)]
        execution_provider: Option<ExecutionProviderChoice>,
        /// Embedding encryption method
        #[arg(long, value_enum)]
        encryption: Option<EncryptionChoice>,
    },
    /// Report whether a user has a usable face enrollment (exit 0 = enrolled, 1 = not enrolled, 2 = error)
    IsEnrolled {
        /// Username (default: current user)
        #[arg(short, long)]
        user: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Suppress stdout; report only via the exit code
        #[arg(long)]
        quiet: bool,
    },
    /// Capture and store a face
    Enroll {
        /// Username to enroll (default: current user)
        #[arg(short, long)]
        user: Option<String>,
        /// Label for this face model
        #[arg(short, long)]
        label: Option<String>,
        /// Skip the setup completion check
        #[arg(long)]
        skip_setup_check: bool,
    },
    /// Remove a face model
    Remove {
        /// Model ID to remove
        model_id: u32,
        /// Username (default: current user)
        #[arg(short, long)]
        user: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// Remove all face models for a user
    Clear {
        /// Username (default: current user)
        #[arg(short, long)]
        user: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// List enrolled face models
    List {
        /// Username (default: current user)
        #[arg(short, long)]
        user: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Test face recognition
    Test {
        /// Username (default: current user)
        #[arg(short, long)]
        user: Option<String>,
    },
    /// Live camera preview with detection overlay
    Preview {
        /// Print detection results to stdout instead of graphical preview
        #[arg(long)]
        text_only: bool,
        /// User to match faces against (defaults to current user)
        #[arg(short, long)]
        user: Option<String>,
    },
    /// Show or edit configuration
    Config {
        /// Open config file in editor
        #[arg(long)]
        edit: bool,
    },
    /// Check system status
    Status,
    /// List available camera devices
    Devices {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Run the persistent authentication daemon
    Daemon {
        /// Path to config file
        #[arg(short, long)]
        config: Option<String>,
    },
    /// One-shot authentication (used by PAM module)
    Auth {
        /// Username to authenticate
        #[arg(long)]
        user: String,
        /// Path to config file
        #[arg(long)]
        config: Option<String>,
    },
    /// Benchmark and calibration tools
    Bench {
        #[command(subcommand)]
        command: BenchCommand,
    },
    /// TPM integration status and management
    Tpm {
        #[command(subcommand)]
        command: TpmCommand,
    },
    /// Manage hyprlock lock-screen integration (no root required)
    Hyprlock {
        #[command(subcommand)]
        command: HyprlockCommand,
    },
    /// Encrypt all unencrypted embeddings with AES-256-GCM
    Encrypt {
        /// Generate a new encryption key (does not encrypt)
        #[arg(long)]
        generate_key: bool,
    },
    /// Decrypt all software-encrypted embeddings
    Decrypt,
    /// Re-seal the TPM AES key under current PCRs (recovery after a firmware/kernel change)
    Reseal,
    /// Restart the facelock daemon
    Restart,
    /// View structured audit log
    Audit {
        /// Follow mode: watch for new entries
        #[arg(short = 'f', long)]
        follow: bool,
        /// Number of recent entries to show
        #[arg(short, long, default_value = "20")]
        lines: usize,
    },
}

fn main() -> anyhow::Result<()> {
    // Localization first: user-facing text (D10) may render before any
    // subcommand dispatch. Log/tracing output is unaffected by design (D2).
    message::init();

    let cli = Cli::parse();
    let Cli { config, command } = cli;

    if let Some(path) = config {
        facelock_core::paths::set_process_config_override(PathBuf::from(path));
    }

    match command {
        // Daemon and auth init their own tracing, so handle them separately
        Commands::Daemon { config } => {
            // The override is how `--config` reaches the daemon: startup, the
            // live reload and the mtime watch all resolve the path through it.
            if let Some(path) = config {
                facelock_core::paths::set_process_config_override(PathBuf::from(path));
            }
            commands::daemon::run(notifications::daemon_notifier_factory())
        }
        Commands::Auth { user, config } => {
            if let Some(ref path) = config {
                facelock_core::paths::set_process_config_override(PathBuf::from(path));
            }
            let exit_code = commands::auth::run(user, config);
            std::process::exit(exit_code);
        }
        other => {
            // Default tracing init for all other commands
            tracing_subscriber::fmt()
                .with_env_filter(logging::default_env_filter())
                .with_target(false)
                .init();

            match other {
                // -- Dispatched before the shared config parse (D7). --
                //
                // `is-enrolled` runs unprivileged on lock screens and must stay
                // in front of all config/resolution machinery: it tolerates a
                // missing or broken config and probes nothing (see
                // commands/is_enrolled.rs). `hyprlock` edits the user's own
                // dotfiles, `config` operates on the config file itself, and
                // `restart` only talks to systemd — none consume a parsed
                // Config.
                Commands::IsEnrolled { user, json, quiet } => {
                    std::process::exit(commands::is_enrolled::run(user, json, quiet))
                }
                Commands::Hyprlock { command } => commands::hyprlock::run(command),
                Commands::Config { edit } => commands::config::run(edit),
                Commands::Restart => commands::config::restart(),

                // Setup bootstraps the config file — creates the default when
                // missing and edits it in place — so it owns its own load; see
                // the commented sites in commands/setup.rs.
                Commands::Setup {
                    non_interactive,
                    yes,
                    pam,
                    no_pam,
                    systemd,
                    no_systemd,
                    enroll,
                    no_enroll,
                    disable,
                    service,
                    remove,
                    if_present,
                    camera,
                    models,
                    execution_provider,
                    encryption,
                } => commands::setup::run_with_plan(resolve_setup_plan(SetupArgs {
                    non_interactive,
                    yes,
                    pam,
                    no_pam,
                    systemd,
                    no_systemd,
                    enroll,
                    no_enroll,
                    disable,
                    service,
                    remove,
                    if_present,
                    camera,
                    models,
                    execution_provider,
                    encryption,
                })),

                other => {
                    // The one parse for this process (D7): every remaining
                    // command consumes this Config and none re-reads the file.
                    let loaded = resolved::ConfigLoad::read();

                    // `status` reports on the config file itself, so a load
                    // failure is a finding to render, not an exit.
                    if matches!(other, Commands::Status) {
                        return commands::status::run(loaded);
                    }
                    let config = loaded.require()?;

                    match other {
                        Commands::Enroll {
                            user,
                            label,
                            skip_setup_check,
                        } => commands::enroll::run(&config, user, label, skip_setup_check),
                        Commands::Remove {
                            model_id,
                            user,
                            yes,
                        } => commands::remove::run(&config, model_id, user, yes),
                        Commands::Clear { user, yes } => commands::clear::run(&config, user, yes),
                        Commands::List { user, json } => commands::list::run(&config, user, json),
                        Commands::Test { user } => commands::test_cmd::run(&config, user),
                        Commands::Preview { text_only, user } => {
                            commands::preview::run(&config, text_only, user)
                        }
                        Commands::Devices { json } => commands::devices::run(&config, json),
                        Commands::Bench { command } => commands::bench::run(&config, command),
                        Commands::Tpm { command } => commands::tpm::run(&config, command),
                        Commands::Encrypt { generate_key } => {
                            commands::encrypt::run_encrypt(&config, generate_key)
                        }
                        Commands::Decrypt => commands::encrypt::run_decrypt(&config),
                        Commands::Reseal => commands::tpm::run_reseal(&config),
                        Commands::Audit { follow, lines } => {
                            commands::audit::run(&config, follow, lines)
                        }
                        // Already handled above
                        Commands::Daemon { .. }
                        | Commands::Auth { .. }
                        | Commands::IsEnrolled { .. }
                        | Commands::Hyprlock { .. }
                        | Commands::Config { .. }
                        | Commands::Restart
                        | Commands::Setup { .. }
                        | Commands::Status => unreachable!(),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use commands::setup::{BaseMode, CameraChoice, PamPref, SetupPlan, SystemdPref};

    #[test]
    fn verify_cli() {
        // Validates the clap derive structure
        Cli::command().debug_assert();
    }

    /// Parse a `facelock setup ...` command line and resolve it, exercising the
    /// exact path `main` takes. Pure: no root, no camera, no network.
    fn plan(args: &[&str]) -> SetupPlan {
        let argv: Vec<&str> = ["facelock", "setup"].iter().chain(args).copied().collect();
        let cli = Cli::try_parse_from(argv).expect("expected these args to parse");
        let Commands::Setup {
            non_interactive,
            yes,
            pam,
            no_pam,
            systemd,
            no_systemd,
            enroll,
            no_enroll,
            disable,
            service,
            remove,
            if_present,
            camera,
            models,
            execution_provider,
            encryption,
        } = cli.command
        else {
            panic!("expected the Setup variant");
        };
        resolve_setup_plan(SetupArgs {
            non_interactive,
            yes,
            pam,
            no_pam,
            systemd,
            no_systemd,
            enroll,
            no_enroll,
            disable,
            service,
            remove,
            if_present,
            camera,
            models,
            execution_provider,
            encryption,
        })
    }

    fn parse_error(args: &[&str]) -> clap::Error {
        let argv: Vec<&str> = ["facelock", "setup"].iter().chain(args).copied().collect();
        Cli::try_parse_from(argv)
            .err()
            .expect("expected a parse error")
    }

    fn install(service: Option<&str>) -> PamPref {
        PamPref::Install {
            service: service.map(str::to_string),
        }
    }

    // -----------------------------------------------------------------------
    // §2.4 compatibility matrix — one test per row.
    // -----------------------------------------------------------------------

    #[test]
    fn matrix_row_bare_setup_is_the_full_wizard() {
        assert_eq!(plan(&[]), SetupPlan::default());
        // Spelled out, since `default()` is what every other row is diffed against.
        let p = plan(&[]);
        assert_eq!(p.base, Some(BaseMode::Wizard));
        assert_eq!(p.systemd, SystemdPref::Ask);
        assert_eq!(p.pam, PamPref::Ask);
        assert_eq!(p.enroll, None);
        assert_eq!(p.camera, None);
        assert_eq!(p.models, None);
        assert_eq!(p.execution_provider, None);
        assert_eq!(p.encryption, None);
        assert!(!p.yes);
    }

    #[test]
    fn matrix_row_non_interactive_skips_every_action() {
        let p = plan(&["--non-interactive"]);
        assert_eq!(p.base, Some(BaseMode::NonInteractive));
        // Ask under a non-interactive base means "do nothing", as today.
        assert_eq!(p.systemd, SystemdPref::Ask);
        assert_eq!(p.pam, PamPref::Ask);
        assert_eq!(p.enroll, None);
    }

    #[test]
    fn matrix_row_pam_with_service_is_standalone() {
        let p = plan(&["--pam", "--service", "sudo"]);
        assert_eq!(p.base, None);
        assert_eq!(p.pam, install(Some("sudo")));
        assert_eq!(p.systemd, SystemdPref::Ask);
    }

    /// Standalone `--pam` / `--systemd` must NOT go through the interactive
    /// root pre-check: it prompts and re-execs under sudo on a TTY, which those
    /// invocations never did. They bail from their own root checks instead.
    /// A base setup, which always did prompt, still must.
    #[test]
    fn only_a_base_setup_takes_the_interactive_root_precheck() {
        for args in [
            vec!["--pam"],
            vec!["--pam", "--service", "sudo"],
            vec!["--pam", "--remove"],
            vec!["--systemd"],
            vec!["--systemd", "--disable"],
            vec!["--systemd", "--pam"],
        ] {
            let p = plan(&args);
            assert_eq!(p.base, None, "{args:?} must stay standalone");
            assert!(
                !commands::setup::needs_root_precheck(&p),
                "{args:?} must not trigger the sudo re-exec prompt"
            );
        }

        for args in [
            vec![],
            vec!["--non-interactive"],
            vec!["--no-pam"],
            vec!["--non-interactive", "--pam"],
            vec!["--camera", "auto"],
        ] {
            let p = plan(&args);
            assert!(
                commands::setup::needs_root_precheck(&p),
                "{args:?} runs a base setup and must keep the root pre-check"
            );
        }
    }

    #[test]
    fn matrix_row_systemd_is_standalone() {
        let p = plan(&["--systemd"]);
        assert_eq!(p.base, None);
        assert_eq!(p.systemd, SystemdPref::Install);
        assert_eq!(p.pam, PamPref::Ask);
    }

    #[test]
    fn matrix_row_systemd_and_pam_runs_both() {
        // Today `--pam` is silently dropped here.
        let p = plan(&["--systemd", "--pam"]);
        assert_eq!(p.base, None);
        assert_eq!(p.systemd, SystemdPref::Install);
        assert_eq!(p.pam, install(None));
    }

    #[test]
    fn matrix_row_non_interactive_and_pam_runs_base_and_pam() {
        // Today `--non-interactive` is silently dropped here.
        let p = plan(&["--non-interactive", "--pam"]);
        assert_eq!(p.base, Some(BaseMode::NonInteractive));
        assert_eq!(p.pam, install(None));
    }

    #[test]
    fn matrix_row_no_pam_suppresses_step_nine() {
        let p = plan(&["--no-pam"]);
        assert_eq!(p.base, Some(BaseMode::Wizard));
        assert_eq!(p.pam, PamPref::Skip);
    }

    #[test]
    fn matrix_row_yes_with_execution_provider() {
        let p = plan(&["-y", "--execution-provider=cuda"]);
        assert_eq!(p.base, Some(BaseMode::Wizard));
        assert!(p.yes);
        assert_eq!(p.execution_provider, Some(ExecutionProviderChoice::Cuda));
    }

    // -----------------------------------------------------------------------
    // Action modifiers
    // -----------------------------------------------------------------------

    #[test]
    fn pam_remove_with_explicit_service() {
        let p = plan(&["--pam", "--remove", "--service", "sudo"]);
        assert_eq!(p.base, None);
        assert_eq!(
            p.pam,
            PamPref::Remove {
                service: "sudo".to_string(),
                if_present: false,
            }
        );
    }

    #[test]
    fn pam_remove_defaults_to_sudo() {
        // Removal needs a concrete service, so the default is applied eagerly.
        assert_eq!(
            plan(&["--pam", "--remove"]).pam,
            PamPref::Remove {
                service: "sudo".to_string(),
                if_present: false,
            }
        );
    }

    #[test]
    fn pam_remove_if_present_reaches_the_resolved_plan() {
        assert_eq!(
            plan(&[
                "--pam",
                "--service",
                "omarchy-lock-face",
                "--remove",
                "--if-present",
            ])
            .pam,
            PamPref::Remove {
                service: "omarchy-lock-face".to_string(),
                if_present: true,
            }
        );
    }

    #[test]
    fn systemd_disable_is_standalone() {
        let p = plan(&["--systemd", "--disable"]);
        assert_eq!(p.base, None);
        assert_eq!(p.systemd, SystemdPref::Disable);
    }

    // -----------------------------------------------------------------------
    // `overrides_with`: the later flag wins for every action pair.
    // -----------------------------------------------------------------------

    #[test]
    fn later_pam_flag_wins() {
        assert_eq!(plan(&["--pam", "--no-pam"]).pam, PamPref::Skip);
        assert_eq!(plan(&["--no-pam", "--pam"]).pam, install(None));
    }

    #[test]
    fn later_systemd_flag_wins() {
        assert_eq!(
            plan(&["--systemd", "--no-systemd"]).systemd,
            SystemdPref::Skip
        );
        assert_eq!(
            plan(&["--no-systemd", "--systemd"]).systemd,
            SystemdPref::Install
        );
    }

    #[test]
    fn later_enroll_flag_wins() {
        assert_eq!(plan(&["--enroll", "--no-enroll"]).enroll, Some(false));
        assert_eq!(plan(&["--no-enroll", "--enroll"]).enroll, Some(true));
    }

    // -----------------------------------------------------------------------
    // Choice flags
    // -----------------------------------------------------------------------

    #[test]
    fn choice_flag_forces_the_base_setup_to_run() {
        // The regression guard: `--camera` must not be silently dropped in
        // favour of PAM-only mode the way the old `else if` chain did.
        let p = plan(&["--camera=/dev/video2", "--pam"]);
        assert_eq!(p.base, Some(BaseMode::Wizard));
        assert_eq!(p.pam, install(None));
        assert_eq!(
            p.camera,
            Some(CameraChoice::Path("/dev/video2".to_string()))
        );
    }

    #[test]
    fn camera_auto_is_distinct_from_a_path() {
        assert_eq!(plan(&["--camera", "auto"]).camera, Some(CameraChoice::Auto));
        assert_eq!(
            plan(&["--camera", "/dev/video2"]).camera,
            Some(CameraChoice::Path("/dev/video2".to_string()))
        );
    }

    #[test]
    fn models_preset_parses() {
        assert_eq!(plan(&["--models", "high"]).models, Some(ModelPreset::High));
        assert_eq!(
            plan(&["--models", "balanced"]).models,
            Some(ModelPreset::Balanced)
        );
        assert_eq!(
            plan(&["--models", "standard"]).models,
            Some(ModelPreset::Standard)
        );
    }

    #[test]
    fn encryption_choice_parses() {
        assert_eq!(
            plan(&["--encryption", "tpm"]).encryption,
            Some(EncryptionChoice::Tpm)
        );
        assert_eq!(
            plan(&["--encryption", "auto"]).encryption,
            Some(EncryptionChoice::Auto)
        );
        assert_eq!(
            plan(&["--encryption", "none"]).encryption,
            Some(EncryptionChoice::None)
        );
    }

    // -----------------------------------------------------------------------
    // Action modifiers require their action. Today these are silently dropped.
    // -----------------------------------------------------------------------

    #[test]
    fn remove_without_pam_is_a_parse_error() {
        assert_eq!(
            parse_error(&["--remove"]).kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn if_present_requires_remove_and_pam() {
        for args in [
            &["--if-present"][..],
            &["--pam", "--if-present"],
            &["--remove", "--if-present"],
        ] {
            assert_eq!(
                parse_error(args).kind(),
                clap::error::ErrorKind::MissingRequiredArgument,
                "unexpected error kind for {args:?}"
            );
        }
    }

    #[test]
    fn setup_help_documents_if_present() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("setup")
            .expect("setup subcommand")
            .render_long_help()
            .to_string();

        assert!(help.contains("--if-present"));
        assert!(help.contains("treat an absent service file as success"));
    }

    #[test]
    fn service_without_pam_is_a_parse_error() {
        assert_eq!(
            parse_error(&["--service", "sudo"]).kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn disable_without_systemd_is_a_parse_error() {
        assert_eq!(
            parse_error(&["--disable"]).kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    // -----------------------------------------------------------------------
    // is-enrolled
    // -----------------------------------------------------------------------

    /// Clap derives the subcommand name from the variant, so `IsEnrolled` must
    /// spell itself `is-enrolled` on the command line.
    #[test]
    fn is_enrolled_flags_parse() {
        let cli = Cli::try_parse_from([
            "facelock",
            "is-enrolled",
            "--user",
            "alice",
            "--json",
            "--quiet",
        ])
        .expect("is-enrolled args should parse");
        let Commands::IsEnrolled { user, json, quiet } = cli.command else {
            panic!("expected the IsEnrolled variant");
        };
        assert_eq!(user.as_deref(), Some("alice"));
        assert!(json);
        assert!(quiet);
    }
}
