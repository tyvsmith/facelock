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
    crate::ipc_client::require_root("sudo facelock tpm seal-db")?;
    let config = Config::load()?;

    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("failed to open face database")?;

    let mut sealer = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
        .context("failed to initialize TPM sealer")?;

    if !sealer.is_available() {
        anyhow::bail!("TPM is not available -- cannot seal embeddings without real TPM support");
    }

    let all = store
        .get_all_embeddings_raw()
        .context("failed to read embeddings")?;

    let unsealed: Vec<_> = all.iter().filter(|(_, _, _, sealed)| !sealed).collect();

    if unsealed.is_empty() {
        println!("All embeddings are already sealed. Nothing to do.");
        return Ok(());
    }

    println!(
        "Sealing {} unsealed embedding(s)...",
        unsealed.len()
    );

    let pcr_indices = if config.tpm.pcr_binding {
        Some(config.tpm.pcr_indices.as_slice())
    } else {
        None
    };

    let mut sealed_count = 0u32;
    for (id, _user, blob, _sealed) in &unsealed {
        let sealed_blob = sealer
            .seal_bytes(blob, pcr_indices)
            .with_context(|| format!("failed to seal embedding {id}"))?;

        store
            .update_embedding_sealed(*id, &sealed_blob, true)
            .with_context(|| format!("failed to update embedding {id}"))?;

        sealed_count += 1;
    }

    println!("Sealed {sealed_count} embedding(s) successfully.");
    Ok(())
}

fn unseal_db() -> Result<()> {
    crate::ipc_client::require_root("sudo facelock tpm unseal-db")?;
    let config = Config::load()?;

    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("failed to open face database")?;

    let mut sealer = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
        .context("failed to initialize TPM sealer")?;

    let all = store
        .get_all_embeddings_raw()
        .context("failed to read embeddings")?;

    let sealed: Vec<_> = all.iter().filter(|(_, _, _, s)| *s).collect();

    if sealed.is_empty() {
        println!("No sealed embeddings found. Nothing to do.");
        return Ok(());
    }

    if !sealer.is_available() {
        anyhow::bail!("TPM is not available -- cannot unseal embeddings without real TPM support");
    }

    println!(
        "Unsealing {} sealed embedding(s)...",
        sealed.len()
    );

    let mut unsealed_count = 0u32;
    for (id, _user, blob, _sealed) in &sealed {
        // Unseal to get raw embedding bytes, then store back as unsealed
        let embedding = sealer
            .unseal_embedding(blob)
            .with_context(|| format!("failed to unseal embedding {id}"))?;

        // Convert embedding back to raw bytes
        let raw_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        store
            .update_embedding_sealed(*id, &raw_bytes, false)
            .with_context(|| format!("failed to update embedding {id}"))?;

        unsealed_count += 1;
    }

    println!("Unsealed {unsealed_count} embedding(s) successfully.");
    Ok(())
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
