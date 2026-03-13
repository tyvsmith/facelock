use std::path::Path;

use anyhow::{Context, Result};
use facelock_core::config::Config;
use facelock_store::FaceStore;

use crate::commands::TpmCommand;

pub fn run(command: TpmCommand) -> Result<()> {
    match command {
        TpmCommand::Status => status(),
        TpmCommand::SealDb => seal_db(),
        TpmCommand::UnsealDb => unseal_db(),
        TpmCommand::PcrBaseline => pcr_baseline(),
    }
}

fn status() -> Result<()> {
    let config = Config::load().context("failed to load config (try: sudo facelock tpm status)")?;

    // Extract device path from TCTI string (e.g., "device:/dev/tpmrm0" -> "/dev/tpmrm0")
    let device_path = config
        .tpm
        .tcti
        .strip_prefix("device:")
        .unwrap_or(&config.tpm.tcti);

    let device_exists = Path::new(device_path).exists();

    let store = FaceStore::open_readonly(Path::new(&config.storage.db_path))
        .context("failed to open face database")?;

    let (sealed_count, unsealed_count) = store
        .count_sealed()
        .context("failed to count sealed embeddings")?;

    println!("TPM Status");
    println!("----------");
    println!(
        "  TPM device ({}): {}",
        device_path,
        if device_exists { "found" } else { "not found" }
    );
    println!(
        "  seal_database: {}",
        if config.tpm.seal_database {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  pcr_binding:   {}",
        if config.tpm.pcr_binding {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!();
    println!("  Embeddings:");
    println!("    sealed:   {sealed_count}");
    println!("    unsealed: {unsealed_count}");
    println!("    total:    {}", sealed_count + unsealed_count);

    #[cfg(not(feature = "tpm"))]
    println!("\n  Note: compiled without TPM support (feature 'tpm' not enabled)");

    // Show software encryption status too
    println!();
    println!("  Software Encryption:");
    println!(
        "    method:   {:?}",
        config.encryption.method
    );
    println!("    key_path: {}", config.encryption.key_path);
    let key_exists = std::path::Path::new(&config.encryption.key_path).exists();
    println!(
        "    key file: {}",
        if key_exists { "present" } else { "not found" }
    );

    Ok(())
}

fn seal_db() -> Result<()> {
    anyhow::bail!(
        "TPM direct sealing is not supported for face embeddings.\n\
         (2048-byte embeddings exceed the TPM's 256-byte SensitiveData limit)\n\n\
         Use software encryption instead:\n\
         \x20 sudo facelock encrypt --generate-key\n\
         \x20 sudo facelock encrypt\n\n\
         Or set in /etc/facelock/config.toml:\n\
         \x20 [encryption]\n\
         \x20 method = \"keyfile\""
    );
}

fn unseal_db() -> Result<()> {
    anyhow::bail!(
        "TPM direct sealing is not supported for face embeddings.\n\
         Use `sudo facelock decrypt` to decrypt software-encrypted embeddings."
    );
}

fn pcr_baseline() -> Result<()> {
    let config = Config::load()?;

    println!("PCR Baseline (indices: {:?})", config.tpm.pcr_indices);
    println!("----------");

    #[cfg(feature = "tpm")]
    {
        use tss_esapi::tcti_ldr::TctiNameConf;

        let tcti_conf: TctiNameConf = config.tpm.tcti.parse()
            .context("invalid TCTI string")?;
        let mut context = tss_esapi::Context::new(tcti_conf)
            .context("failed to connect to TPM")?;

        let values =
            facelock_tpm::PcrVerifier::read_current(&mut context, &config.tpm.pcr_indices)
                .context("failed to read PCR values")?;

        for (index, digest) in &values {
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            println!("  PCR[{index:>2}]: {hex}");
        }
    }

    #[cfg(not(feature = "tpm"))]
    {
        println!("  TPM support not compiled in (feature 'tpm' not enabled).");
        println!("  Rebuild with: cargo build --features tpm");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn extract_device_path_from_tcti() {
        let tcti = "device:/dev/tpmrm0";
        let path = tcti.strip_prefix("device:").unwrap_or(tcti);
        assert_eq!(path, "/dev/tpmrm0");
    }

    #[test]
    fn extract_device_path_without_prefix() {
        let tcti = "/dev/tpmrm0";
        let path = tcti.strip_prefix("device:").unwrap_or(tcti);
        assert_eq!(path, "/dev/tpmrm0");
    }
}
