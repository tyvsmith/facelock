pub mod auth;
pub mod bench;
pub mod clear;
pub mod config;
pub mod daemon;
pub mod devices;
pub mod enroll;
pub mod list;
pub mod preview;
pub mod remove;
pub mod setup;
pub mod status;
pub mod test_cmd;
pub mod tpm;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum TpmCommand {
    /// Report TPM availability and configuration
    Status,
    /// Seal all unsealed embeddings in the database using TPM
    SealDb,
    /// Unseal all sealed embeddings (migrate away from TPM)
    UnsealDb,
    /// Display current PCR values for configured indices
    PcrBaseline,
}
