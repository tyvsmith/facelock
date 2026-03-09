mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "howdy", about = "Linux face authentication", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up howdy (download models, configure camera)
    Setup,
    /// Enroll a new face model
    Enroll {
        /// Label for this face model
        #[arg(short, long)]
        label: Option<String>,
    },
    /// Remove a face model
    Remove {
        /// Model ID to remove
        model_id: u32,
    },
    /// Clear all face models for the current user
    Clear,
    /// List enrolled face models
    List,
    /// Test face recognition
    Test,
    /// Preview camera feed
    Preview,
    /// Show or edit configuration
    Config {
        /// Config key to get/set
        key: Option<String>,
        /// Value to set
        value: Option<String>,
    },
    /// Show daemon and camera status
    Status,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Setup => commands::setup::run(),
        Commands::Enroll { label } => commands::enroll::run(label),
        Commands::Remove { model_id } => commands::remove::run(model_id),
        Commands::Clear => commands::clear::run(),
        Commands::List => commands::list::run(),
        Commands::Test => commands::test_cmd::run(),
        Commands::Preview => commands::preview::run(),
        Commands::Config { key, value } => commands::config::run(key, value),
        Commands::Status => commands::status::run(),
    }
}
