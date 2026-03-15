use std::path::Path;

use anyhow::{Context, Result};
use facelock_core::config::Config;

use crate::commands::TpmCommand;

pub fn run(command: TpmCommand) -> Result<()> {
    match command {
        TpmCommand::Status => status(),
        TpmCommand::SealKey => seal_key(),
        TpmCommand::UnsealKey => unseal_key(),
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

    let store = facelock_store::FaceStore::open_readonly(Path::new(&config.storage.db_path))
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
        "  pcr_binding:   {}",
        if config.tpm.pcr_binding {
            "enabled"
        } else {
            "disabled"
        }
    );

    // Show sealed key status
    let sealed_key_path = &config.encryption.sealed_key_path;
    let sealed_key_exists = Path::new(sealed_key_path).exists();
    println!(
        "  sealed key ({}): {}",
        sealed_key_path,
        if sealed_key_exists {
            "present"
        } else {
            "not found"
        }
    );

    let method = match config.encryption.method {
        facelock_core::config::EncryptionMethod::Tpm => "tpm (TPM-sealed AES key)",
        facelock_core::config::EncryptionMethod::Keyfile => "keyfile (plaintext AES key)",
        facelock_core::config::EncryptionMethod::None => "none",
    };
    println!("  encryption:    {method}");

    println!();
    println!("  Embeddings:");
    println!("    encrypted: {sealed_count}");
    println!("    plaintext: {unsealed_count}");
    println!("    total:     {}", sealed_count + unsealed_count);

    #[cfg(not(feature = "tpm"))]
    println!("\n  Note: compiled without TPM support (feature 'tpm' not enabled)");

    Ok(())
}

fn seal_key() -> Result<()> {
    crate::ipc_client::require_root("sudo facelock tpm seal-key")?;

    #[cfg(feature = "tpm")]
    {
        let config = Config::load()?;
        let key_path = Path::new(&config.encryption.key_path);
        let sealed_path = Path::new(&config.encryption.sealed_key_path);

        if !key_path.exists() {
            anyhow::bail!(
                "No plaintext key file found at {}.\n\
                 Generate one first with: sudo facelock encrypt --generate-key",
                key_path.display()
            );
        }

        if sealed_path.exists() {
            anyhow::bail!(
                "Sealed key already exists at {}.\n\
                 Remove it first if you want to re-seal.",
                sealed_path.display()
            );
        }

        // Read the plaintext key
        let key_data = std::fs::read(key_path)
            .with_context(|| format!("failed to read key file {}", key_path.display()))?;
        if key_data.len() != 32 {
            anyhow::bail!("key file must be exactly 32 bytes, got {}", key_data.len());
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_data);

        let pcr = if config.tpm.pcr_binding {
            Some(config.tpm.pcr_indices.as_slice())
        } else {
            None
        };

        println!("Sealing AES key with TPM...");
        let mut tpm =
            facelock_tpm::TpmSealer::new(&config.tpm.tcti).context("failed to initialize TPM")?;
        tpm.seal_key_to_file(&key, sealed_path, pcr)
            .context("failed to seal key with TPM")?;

        // Zeroize the in-memory copy
        use zeroize::Zeroize;
        key.zeroize();

        // Update config to use tpm method
        super::setup::update_config_encryption_method("tpm")?;

        println!(
            "Key sealed to {} (permissions: 0600).",
            sealed_path.display()
        );
        println!("Config updated: encryption.method = \"tpm\"");
        println!(
            "\nThe plaintext key at {} is no longer needed for auth.",
            key_path.display()
        );
        println!("You may remove it: sudo rm {}", key_path.display());
        println!("(Keep a backup if you want the ability to rollback without TPM)");

        Ok(())
    }

    #[cfg(not(feature = "tpm"))]
    {
        anyhow::bail!(
            "TPM support not compiled in (missing 'tpm' feature).\n\
             Rebuild with: cargo build --features tpm"
        );
    }
}

fn unseal_key() -> Result<()> {
    crate::ipc_client::require_root("sudo facelock tpm unseal-key")?;

    #[cfg(feature = "tpm")]
    {
        let config = Config::load()?;
        let key_path = Path::new(&config.encryption.key_path);
        let sealed_path = Path::new(&config.encryption.sealed_key_path);

        if !sealed_path.exists() {
            anyhow::bail!("No sealed key found at {}.", sealed_path.display());
        }

        if key_path.exists() {
            anyhow::bail!(
                "Plaintext key already exists at {}.\n\
                 Remove it first if you want to overwrite.",
                key_path.display()
            );
        }

        println!("Unsealing AES key from TPM...");
        let mut tpm =
            facelock_tpm::TpmSealer::new(&config.tpm.tcti).context("failed to initialize TPM")?;
        let key = tpm
            .unseal_key_from_file(sealed_path)
            .context("failed to unseal key from TPM")?;

        // Write plaintext key file
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to create key file")?;
        // Overwrite with the actual unsealed key (generate_key_file creates a random one)
        std::fs::write(key_path, key).context("failed to write unsealed key")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))
                .context("failed to set key file permissions")?;
        }

        // Update config to use keyfile method
        super::setup::update_config_encryption_method("keyfile")?;

        println!("Key written to {} (permissions: 0600).", key_path.display());
        println!("Config updated: encryption.method = \"keyfile\"");
        println!("\nEmbeddings remain encrypted with the same AES key — no re-encryption needed.");

        Ok(())
    }

    #[cfg(not(feature = "tpm"))]
    {
        anyhow::bail!(
            "TPM support not compiled in (missing 'tpm' feature).\n\
             Rebuild with: cargo build --features tpm"
        );
    }
}

fn pcr_baseline() -> Result<()> {
    let config = Config::load()?;

    println!("PCR Baseline (indices: {:?})", config.tpm.pcr_indices);
    println!("----------");

    #[cfg(feature = "tpm")]
    {
        use tss_esapi::tcti_ldr::TctiNameConf;

        let tcti_conf: TctiNameConf = config.tpm.tcti.parse().context("invalid TCTI string")?;
        let mut context = tss_esapi::Context::new(tcti_conf).context("failed to connect to TPM")?;

        let values = facelock_tpm::PcrVerifier::read_current(&mut context, &config.tpm.pcr_indices)
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
