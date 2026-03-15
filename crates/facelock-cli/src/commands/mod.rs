pub mod audit;
pub mod auth;
pub mod bench;
pub mod clear;
pub mod config;
pub mod daemon;
pub mod devices;
pub mod encrypt;
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
    /// Seal the AES encryption key with TPM (migrate keyfile → tpm)
    SealKey,
    /// Unseal the AES key from TPM back to a plaintext keyfile (migrate tpm → keyfile)
    UnsealKey,
    /// Display current PCR values for configured indices
    PcrBaseline,
}
