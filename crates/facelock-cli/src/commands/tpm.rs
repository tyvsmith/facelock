use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;
use facelock_core::config::Config;

/// Everything that manages the embedding encryption key.
///
/// `encrypt`, `decrypt` and `reseal` live here rather than at the top level
/// (ADR 009): the group owns the key's whole lifecycle, and `reseal` was
/// already implemented in this module. `encrypt`/`decrypt` run software
/// AES-256-GCM with no TPM involved (ADR 004) — the group's `about` says so.
#[derive(Subcommand)]
pub enum TpmCommand {
    /// Report TPM availability and configuration
    Status,
    /// Seal the AES encryption key with TPM (migrate keyfile → tpm)
    SealKey,
    /// Unseal the AES key from TPM back to a plaintext keyfile (migrate tpm → keyfile)
    UnsealKey,
    /// Read-only check that the sealed AES key currently unseals (verifies PCR
    /// policy is satisfied). Writes nothing; exits non-zero if unseal fails.
    UnsealCheck,
    /// Display current PCR values for configured indices
    PcrBaseline,
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
}

impl TpmCommand {
    /// The escalation hint `main`'s root gate (C6) names for each verb —
    /// the spelling `--help` teaches, one row per subcommand.
    pub fn sudo_hint(&self) -> &'static str {
        match self {
            TpmCommand::Status => "sudo facelock tpm status",
            TpmCommand::SealKey => "sudo facelock tpm seal-key",
            TpmCommand::UnsealKey => "sudo facelock tpm unseal-key",
            TpmCommand::UnsealCheck => "sudo facelock tpm unseal-check",
            TpmCommand::PcrBaseline => "sudo facelock tpm pcr-baseline",
            TpmCommand::Encrypt { .. } => "sudo facelock tpm encrypt",
            TpmCommand::Decrypt => "sudo facelock tpm decrypt",
            TpmCommand::Reseal => "sudo facelock tpm reseal",
        }
    }
}

pub fn run(config: &Config, command: TpmCommand) -> Result<()> {
    // Root for every verb is established by `main`'s `require_root_for`
    // gate, ahead of the config parse (C6, issue #191); `sudo_hint` above
    // is the spelling that gate escalates with.
    match command {
        TpmCommand::Status => status(config),
        TpmCommand::SealKey => seal_key(config),
        TpmCommand::UnsealKey => unseal_key(config),
        TpmCommand::UnsealCheck => unseal_check(config),
        TpmCommand::PcrBaseline => pcr_baseline(config),
        // Key material, not the TPM device: software AES-256-GCM either way
        // (ADR 004). The group owns the key's lifecycle, which is why these
        // three moved under it (ADR 009).
        TpmCommand::Encrypt { generate_key } => {
            crate::commands::encrypt::run_encrypt(config, generate_key)
        }
        TpmCommand::Decrypt => crate::commands::encrypt::run_decrypt(config),
        TpmCommand::Reseal => run_reseal(config),
    }
}

/// Read-only verification that the sealed AES key currently unseals.
///
/// Exercises the real unseal path (including PolicyPCR replay for PCR-bound
/// keys) without writing anything or mutating config. Returns an error (non-zero
/// exit) when unseal fails — e.g. after a bound PCR changed. Used by operators to
/// confirm whether `facelock tpm reseal` is needed, and by the TPM E2E suite.
fn unseal_check(config: &Config) -> Result<()> {
    if config.encryption.method != facelock_core::config::EncryptionMethod::Tpm {
        anyhow::bail!(
            "encryption.method is not \"tpm\" (current: {:?}); nothing to unseal.",
            config.encryption.method
        );
    }

    #[cfg(feature = "tpm")]
    {
        let sealed_path = Path::new(&config.encryption.sealed_key_path);
        if !sealed_path.exists() {
            anyhow::bail!("no sealed key found at {}.", sealed_path.display());
        }
        let mut tpm =
            facelock_tpm::TpmSealer::new(&config.tpm.tcti).context("failed to initialize TPM")?;
        match tpm.unseal_key_from_file(sealed_path) {
            Ok(mut key) => {
                use zeroize::Zeroize;
                key.zeroize();
                println!("sealed key unseals OK ({})", sealed_path.display());
                Ok(())
            }
            Err(e) => {
                anyhow::bail!(
                    "sealed key does NOT unseal ({}): {e}\n\
                     If a bound PCR changed (firmware/kernel update), run: sudo facelock tpm reseal",
                    sealed_path.display()
                );
            }
        }
    }

    #[cfg(not(feature = "tpm"))]
    {
        anyhow::bail!("TPM support not compiled in (missing 'tpm' feature).");
    }
}

fn status(config: &Config) -> Result<()> {
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

/// Seal the plaintext keyfile at `config.encryption.key_path` to
/// `config.encryption.sealed_key_path`, carrying the SAME key bytes across —
/// no new key is minted. Does not touch the config file and does not print;
/// callers own both. Shared by `seal_key` below and by
/// `setup_encryption_tpm_key`'s `SealExistingKeyfile` path, so the two never
/// mint two different keys for the same host (issue #354).
#[cfg(feature = "tpm")]
pub(crate) fn seal_existing_keyfile(config: &Config) -> Result<()> {
    let key_path = Path::new(&config.encryption.key_path);
    let sealed_path = Path::new(&config.encryption.sealed_key_path);

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

    let mut tpm =
        facelock_tpm::TpmSealer::new(&config.tpm.tcti).context("failed to initialize TPM")?;
    let seal_result = tpm.seal_key_to_file(&key, sealed_path, pcr);

    // Zeroize the in-memory copy
    use zeroize::Zeroize;
    key.zeroize();

    seal_result.context("failed to seal key with TPM")
}

#[cfg_attr(not(feature = "tpm"), allow(unused_variables))]
fn seal_key(config: &Config) -> Result<()> {
    #[cfg(feature = "tpm")]
    {
        let key_path = Path::new(&config.encryption.key_path);
        let sealed_path = Path::new(&config.encryption.sealed_key_path);

        if !key_path.exists() {
            anyhow::bail!(
                "No plaintext key file found at {}.\n\
                 Generate one first with: sudo facelock tpm encrypt --generate-key",
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

        println!("Sealing AES key with TPM...");
        seal_existing_keyfile(config)?;

        // Update config to use tpm method
        super::setup::update_config_encryption_method(config, "tpm")?;

        println!(
            "Key sealed to {} (permissions: 0600).",
            sealed_path.display()
        );
        println!("Config updated: encryption.method = \"tpm\"");
        println!(
            "\nKeep the plaintext key backup at {}: it lets `sudo facelock tpm reseal`\n\
             recover face auth after a firmware/kernel PCR change (and roll back to the\n\
             keyfile method) WITHOUT re-enrolling.",
            key_path.display()
        );
        println!(
            "Tradeoff: while that backup exists, the tpm method's at-rest confidentiality\n\
             against anyone who can read the file reduces to its 0600 (root-only) protection.\n\
             PCR binding stays off by default (tpm.pcr_binding = false); enabling it commits\n\
             you to running `tpm reseal` after each bound-PCR change, so keeping the backup is the\n\
             recommended setup. Remove it only if you accept re-enrolling to recover."
        );

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

/// Unseal `config.encryption.sealed_key_path` and write the SAME key bytes to
/// `config.encryption.key_path` at 0600 — no new key is minted. Does not touch
/// the config file and does not print; callers own both. Shared by
/// `unseal_key` below and by `setup_encryption_keyfile`'s
/// `UnsealExistingSealed` path (issue #354).
///
/// Calls `generate_key_file` then overwrites it with the real unsealed key:
/// kept as-is (rather than writing the key directly) so the file is created
/// with the same one-`open(2)`-at-0600 path `generate_key_file` already uses,
/// with no separate `chmod` window.
#[cfg(feature = "tpm")]
pub(crate) fn unseal_into_keyfile(config: &Config) -> Result<()> {
    let key_path = Path::new(&config.encryption.key_path);
    let sealed_path = Path::new(&config.encryption.sealed_key_path);

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
    Ok(())
}

#[cfg_attr(not(feature = "tpm"), allow(unused_variables))]
fn unseal_key(config: &Config) -> Result<()> {
    #[cfg(feature = "tpm")]
    {
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
        unseal_into_keyfile(config)?;

        // Update config to use keyfile method
        super::setup::update_config_encryption_method(config, "keyfile")?;

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

/// Re-seal the AES key under the CURRENT PCR values (`facelock tpm reseal`).
///
/// This is the recovery path for TPM PCR binding: a firmware/kernel update that
/// changes a bound PCR makes the old sealed blob refuse to unseal. Password login
/// keeps working throughout; running `tpm reseal` restores face auth by re-sealing
/// the key against the new PCR state. The key is recovered from the existing
/// sealed blob when PCRs are still valid, otherwise from the plaintext key backup
/// at `encryption.key_path` if present.
pub fn run_reseal(config: &Config) -> Result<()> {
    if config.encryption.method != facelock_core::config::EncryptionMethod::Tpm {
        anyhow::bail!(
            "`facelock tpm reseal` only applies when encryption.method = \"tpm\" \
             (current: {:?}). Nothing to reseal.",
            config.encryption.method
        );
    }

    #[cfg(feature = "tpm")]
    {
        let sealed_path = Path::new(&config.encryption.sealed_key_path);
        let key_path = Path::new(&config.encryption.key_path);

        if !sealed_path.exists() {
            anyhow::bail!(
                "no sealed key found at {} to re-seal.",
                sealed_path.display()
            );
        }

        let mut tpm =
            facelock_tpm::TpmSealer::new(&config.tpm.tcti).context("failed to initialize TPM")?;

        // Recover the 32-byte AES key. Prefer unsealing the existing blob (works
        // when PCRs are still valid — e.g. proactively refreshing); fall back to
        // the plaintext key backup after an actual PCR change.
        let mut key = match tpm.unseal_key_from_file(sealed_path) {
            Ok(k) => {
                println!(
                    "Unsealed the current key (PCR policy still satisfied); \
                     re-sealing under current PCRs."
                );
                k
            }
            Err(e) => {
                println!("Could not unseal the existing key (likely a PCR change): {e}");
                if key_path.exists() {
                    let data = std::fs::read(key_path).with_context(|| {
                        format!("failed to read key backup {}", key_path.display())
                    })?;
                    if data.len() != 32 {
                        anyhow::bail!(
                            "key backup at {} is {} bytes, expected 32",
                            key_path.display(),
                            data.len()
                        );
                    }
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&data);
                    println!(
                        "Recovered the AES key from the plaintext backup at {}.",
                        key_path.display()
                    );
                    k
                } else {
                    anyhow::bail!(
                        "cannot recover the AES key: the sealed blob no longer unseals under \
                         the current PCRs and there is no plaintext backup at {}.\n\
                         Password login still works. Restore a key backup to {}, then re-run \
                         `sudo facelock tpm reseal`, or clear and re-enroll: sudo facelock clear --yes",
                        key_path.display(),
                        key_path.display()
                    );
                }
            }
        };

        let pcr = if config.tpm.pcr_binding {
            Some(config.tpm.pcr_indices.as_slice())
        } else {
            None
        };

        let seal_result = tpm
            .seal_key_to_file(&key, sealed_path, pcr)
            .context("failed to re-seal key under current PCRs");

        use zeroize::Zeroize;
        key.zeroize();
        seal_result?;

        println!(
            "Re-sealed the AES key to {} under the current PCR state (permissions: 0600).",
            sealed_path.display()
        );
        if pcr.is_none() {
            println!(
                "Note: tpm.pcr_binding is disabled, so the key is sealed without a PCR policy."
            );
        }
        println!("Face authentication should work again on this boot.");
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

fn pcr_baseline(config: &Config) -> Result<()> {
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
