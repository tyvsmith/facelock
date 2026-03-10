mod commands;
pub mod direct;
mod ipc_client;
pub mod notifications;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use commands::bench::BenchCommand;

#[derive(Parser)]
#[command(name = "visage", about = "Linux face authentication", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download models and create directories
    Setup,
    /// Capture and store a face
    Enroll {
        /// Username to enroll (default: current user)
        #[arg(short, long)]
        user: Option<String>,
        /// Label for this face model
        #[arg(short, long)]
        label: Option<String>,
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        // Daemon and auth init their own tracing, so handle them separately
        Commands::Daemon { config } => {
            commands::daemon::run(config)
        }
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
                Commands::Setup => commands::setup::run(),
                Commands::Enroll { user, label } => commands::enroll::run(user, label),
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
