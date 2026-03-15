mod commands;
pub mod direct;
mod ipc_client;
pub mod notifications;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use commands::TpmCommand;
use commands::bench::BenchCommand;

#[derive(Parser)]
#[command(name = "facelock", about = "Linux face authentication", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download models and create directories
    Setup {
        /// Install and enable systemd units
        #[arg(long)]
        systemd: bool,
        /// Used with --systemd: disable and stop systemd units instead
        #[arg(long, requires = "systemd")]
        disable: bool,
        /// Install or manage PAM module configuration
        #[arg(long)]
        pam: bool,
        /// Target PAM service (default: sudo)
        #[arg(long, default_value = "sudo")]
        service: String,
        /// Remove the PAM line instead of adding it
        #[arg(long)]
        remove: bool,
        /// Skip confirmation for sensitive services
        #[arg(short, long)]
        yes: bool,
        /// Run in non-interactive mode (skip wizard)
        #[arg(long)]
        non_interactive: bool,
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
    Devices,
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
    /// Encrypt all unencrypted embeddings with AES-256-GCM
    Encrypt {
        /// Generate a new encryption key (does not encrypt)
        #[arg(long)]
        generate_key: bool,
    },
    /// Decrypt all software-encrypted embeddings
    Decrypt,
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
    let cli = Cli::parse();

    match cli.command {
        // Daemon and auth init their own tracing, so handle them separately
        Commands::Daemon { config } => commands::daemon::run(config),
        Commands::Auth { user, config } => {
            let exit_code = commands::auth::run(user, config);
            std::process::exit(exit_code);
        }
        other => {
            // Default tracing init for all other commands
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .with_target(false)
                .init();

            match other {
                Commands::Setup {
                    systemd,
                    disable,
                    pam,
                    service,
                    remove,
                    yes,
                    non_interactive,
                } => {
                    if systemd {
                        commands::setup::run_systemd(disable)
                    } else if pam {
                        commands::setup::run_pam(&service, remove, yes)
                    } else {
                        commands::setup::run(non_interactive)
                    }
                }
                Commands::Enroll { user, label, skip_setup_check } => commands::enroll::run(user, label, skip_setup_check),
                Commands::Remove {
                    model_id,
                    user,
                    yes,
                } => commands::remove::run(model_id, user, yes),
                Commands::Clear { user, yes } => commands::clear::run(user, yes),
                Commands::List { user, json } => commands::list::run(user, json),
                Commands::Test { user } => commands::test_cmd::run(user),
                Commands::Preview { text_only, user } => commands::preview::run(text_only, user),
                Commands::Config { edit } => commands::config::run(edit),
                Commands::Status => commands::status::run(),
                Commands::Devices => commands::devices::run(),
                Commands::Bench { command } => commands::bench::run(command),
                Commands::Tpm { command } => commands::tpm::run(command),
                Commands::Encrypt { generate_key } => commands::encrypt::run_encrypt(generate_key),
                Commands::Decrypt => commands::encrypt::run_decrypt(),
                Commands::Audit { follow, lines } => commands::audit::run(follow, lines),
                // Already handled above
                Commands::Daemon { .. } | Commands::Auth { .. } => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn verify_cli() {
        // Validates the clap derive structure
        Cli::command().debug_assert();
    }
}
