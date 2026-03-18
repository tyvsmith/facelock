use std::path::Path;

use anyhow::{Context, Result, bail};
use facelock_core::config::{Config, EncryptionMethod};
use facelock_store::FaceStore;

/// Obtain a SoftwareSealer based on the configured encryption method.
fn obtain_sealer(config: &Config) -> Result<facelock_tpm::SoftwareSealer> {
    match config.encryption.method {
        EncryptionMethod::Keyfile => {
            let key_path = Path::new(&config.encryption.key_path);
            facelock_tpm::SoftwareSealer::from_key_file(key_path)
                .context("failed to load encryption key")
        }
        EncryptionMethod::Tpm => {
            #[cfg(feature = "tpm")]
            {
                let sealed_path = Path::new(&config.encryption.sealed_key_path);
                let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                    .context("failed to initialize TPM")?;
                let key = tpm.unseal_key_from_file(sealed_path).with_context(|| {
                    format!("failed to unseal key from {}", sealed_path.display())
                })?;
                Ok(facelock_tpm::SoftwareSealer::from_key(key))
            }
            #[cfg(not(feature = "tpm"))]
            {
                bail!(
                    "encryption method is 'tpm' but TPM support is not compiled in (rebuild with --features tpm)"
                );
            }
        }
        EncryptionMethod::None => {
            bail!("no encryption method configured. Set [encryption] method in config.");
        }
    }
}

pub fn run_encrypt(generate_key: bool) -> Result<()> {
    crate::ipc_client::require_root("sudo facelock encrypt")?;
    let config = Config::load()?;

    if generate_key {
        match config.encryption.method {
            EncryptionMethod::Tpm => {
                #[cfg(feature = "tpm")]
                {
                    let sealed_path = Path::new(&config.encryption.sealed_key_path);
                    println!(
                        "Generating and sealing AES key with TPM to {}...",
                        sealed_path.display()
                    );
                    let pcr = if config.tpm.pcr_binding {
                        Some(config.tpm.pcr_indices.as_slice())
                    } else {
                        None
                    };
                    let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                        .context("failed to initialize TPM")?;
                    facelock_tpm::generate_and_seal_key(&mut tpm, sealed_path, pcr)
                        .context("failed to generate and seal key")?;
                    println!("TPM-sealed key generated (permissions: 0600).");
                    return Ok(());
                }
                #[cfg(not(feature = "tpm"))]
                {
                    bail!("encryption method is 'tpm' but TPM support is not compiled in");
                }
            }
            _ => {
                let key_path = Path::new(&config.encryption.key_path);
                println!("Generating encryption key at {}...", key_path.display());
                facelock_tpm::SoftwareSealer::generate_key_file(key_path)
                    .context("failed to generate encryption key")?;
                println!("Key generated (permissions: 0600 root-only).");
                println!(
                    "\nTo encrypt embeddings, run: sudo facelock encrypt\n\
                     To enable auto-encryption, add to config:\n\
                     [encryption]\n\
                     method = \"keyfile\"\n\
                     key_path = \"{}\"",
                    key_path.display()
                );
                return Ok(());
            }
        }
    }

    // For non-generate runs, if method is keyfile and key doesn't exist, generate it
    let key_path = Path::new(&config.encryption.key_path);
    if config.encryption.method != EncryptionMethod::Tpm && !key_path.exists() {
        println!("Generating encryption key at {}...", key_path.display());
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to generate encryption key")?;
        println!("Key generated (permissions: 0600 root-only).");
        println!("Proceeding to encrypt embeddings...");
    }

    let sealer = obtain_sealer(&config).context("failed to obtain encryption sealer")?;

    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("failed to open face database")?;

    let all = store
        .get_all_embeddings_raw()
        .context("failed to read embeddings")?;

    // Filter to unencrypted embeddings only
    let unencrypted: Vec<_> = all
        .iter()
        .filter(|(_, _, blob, sealed)| !sealed && !facelock_tpm::is_software_encrypted(blob))
        .collect();

    if unencrypted.is_empty() {
        println!("All embeddings are already encrypted. Nothing to do.");
        return Ok(());
    }

    println!(
        "Encrypting {} unencrypted embedding(s)...",
        unencrypted.len()
    );

    let mut encrypted_count = 0u32;
    for (id, _user, blob, _sealed) in &unencrypted {
        let encrypted_blob = sealer
            .seal_bytes(blob)
            .with_context(|| format!("failed to encrypt embedding {id}"))?;

        // Store with sealed=true and sealed column value distinguishes TPM (1) from software (2)
        // We use sealed=true since the DB uses a boolean flag; the version byte in the blob
        // distinguishes TPM from software encryption.
        store
            .update_embedding_sealed(*id, &encrypted_blob, true)
            .with_context(|| format!("failed to update embedding {id}"))?;

        encrypted_count += 1;
    }

    println!("Encrypted {encrypted_count} embedding(s) with AES-256-GCM.");
    Ok(())
}

pub fn run_decrypt() -> Result<()> {
    crate::ipc_client::require_root("sudo facelock decrypt")?;
    let config = Config::load()?;

    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("failed to open face database")?;

    let all = store
        .get_all_embeddings_raw()
        .context("failed to read embeddings")?;

    // Partition into software-encrypted and TPM-sealed embeddings
    let sw_encrypted: Vec<_> = all
        .iter()
        .filter(|(_, _, blob, _)| facelock_tpm::is_software_encrypted(blob))
        .collect();

    let tpm_sealed: Vec<_> = all
        .iter()
        .filter(|(_, _, blob, _)| facelock_tpm::is_sealed(blob))
        .collect();

    if sw_encrypted.is_empty() && tpm_sealed.is_empty() {
        println!("No encrypted embeddings found. Nothing to do.");
        return Ok(());
    }

    let mut decrypted_count = 0u32;

    // Decrypt software-encrypted embeddings
    if !sw_encrypted.is_empty() {
        let sealer = obtain_sealer(&config).context("failed to obtain encryption sealer")?;

        println!(
            "Decrypting {} software-encrypted embedding(s)...",
            sw_encrypted.len()
        );

        for (id, _user, blob, _sealed) in &sw_encrypted {
            let raw = sealer
                .unseal_bytes(blob)
                .with_context(|| format!("failed to decrypt software-encrypted embedding {id}"))?;

            store
                .update_embedding_sealed(*id, &raw, false)
                .with_context(|| format!("failed to update embedding {id}"))?;

            decrypted_count += 1;
        }
    }

    // Decrypt TPM-sealed embeddings
    if !tpm_sealed.is_empty() {
        println!("Decrypting {} TPM-sealed embedding(s)...", tpm_sealed.len());

        #[cfg(feature = "tpm")]
        {
            let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                .context("failed to initialize TPM for unsealing")?;

            for (id, _user, blob, _sealed) in &tpm_sealed {
                let raw = tpm
                    .unseal_bytes(blob)
                    .with_context(|| format!("failed to unseal TPM embedding {id}"))?;

                store
                    .update_embedding_sealed(*id, &raw, false)
                    .with_context(|| format!("failed to update embedding {id}"))?;

                decrypted_count += 1;
            }
        }

        #[cfg(not(feature = "tpm"))]
        {
            bail!(
                "found {} TPM-sealed embedding(s) but TPM support is not compiled in \
                 (rebuild with --features tpm)",
                tpm_sealed.len()
            );
        }
    }

    println!("Decrypted {decrypted_count} embedding(s) successfully.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use facelock_core::config::EncryptionMethod;

    #[test]
    fn encryption_method_default() {
        assert_eq!(EncryptionMethod::default(), EncryptionMethod::None);
    }

    /// Integration test: encrypt embeddings in memory DB, then decrypt them back.
    /// Exercises the core logic of run_encrypt/run_decrypt without Config::load().
    #[test]
    fn encrypt_decrypt_round_trip_in_memory() {
        let store = facelock_store::FaceStore::open_memory().unwrap();

        // Add some unencrypted embeddings
        let emb = [0.42f32; 512];
        store.add_model("alice", "front", &emb, "").unwrap();
        store.add_model("alice", "side", &emb, "").unwrap();

        let key = [0x42u8; 32];
        let sealer = facelock_tpm::SoftwareSealer::from_key(key);

        // Verify all start unencrypted
        let all = store.get_all_embeddings_raw().unwrap();
        assert_eq!(all.len(), 2);
        for (_, _, blob, sealed) in &all {
            assert!(!sealed);
            assert!(!facelock_tpm::is_software_encrypted(blob));
        }

        // Encrypt all unencrypted
        let unencrypted: Vec<_> = all
            .iter()
            .filter(|(_, _, blob, sealed)| !sealed && !facelock_tpm::is_software_encrypted(blob))
            .collect();
        assert_eq!(unencrypted.len(), 2);

        for (id, _, blob, _) in &unencrypted {
            let encrypted_blob = sealer.seal_bytes(blob).unwrap();
            store
                .update_embedding_sealed(*id, &encrypted_blob, true)
                .unwrap();
        }

        // Verify all are now encrypted
        let all = store.get_all_embeddings_raw().unwrap();
        for (_, _, blob, sealed) in &all {
            assert!(sealed);
            assert!(facelock_tpm::is_software_encrypted(blob));
        }

        // Decrypt all
        let encrypted: Vec<_> = all
            .iter()
            .filter(|(_, _, blob, _)| facelock_tpm::is_software_encrypted(blob))
            .collect();
        assert_eq!(encrypted.len(), 2);

        for (id, _, blob, _) in &encrypted {
            let raw = sealer.unseal_bytes(blob).unwrap();
            store.update_embedding_sealed(*id, &raw, false).unwrap();
        }

        // Verify all are decrypted and match original data
        let final_embs = store.get_user_embeddings("alice").unwrap();
        assert_eq!(final_embs.len(), 2);
        for (_, recovered) in &final_embs {
            assert_eq!(*recovered, emb, "decrypted embedding should match original");
        }
    }

    /// Test filtering logic: mixed encrypted/unencrypted embeddings
    #[test]
    fn encrypt_skips_already_encrypted() {
        let store = facelock_store::FaceStore::open_memory().unwrap();

        let emb = [0.5f32; 512];
        store.add_model("alice", "raw", &emb, "").unwrap();

        // Pre-encrypt one embedding manually
        let key = [0x42u8; 32];
        let sealer = facelock_tpm::SoftwareSealer::from_key(key);
        let raw_bytes: Vec<u8> = emb.iter().flat_map(|f| f.to_le_bytes()).collect();
        let encrypted_blob = sealer.seal_bytes(&raw_bytes).unwrap();
        store
            .add_model_raw("bob", "encrypted", &encrypted_blob, true, "")
            .unwrap();

        let all = store.get_all_embeddings_raw().unwrap();
        let unencrypted: Vec<_> = all
            .iter()
            .filter(|(_, _, blob, sealed)| !sealed && !facelock_tpm::is_software_encrypted(blob))
            .collect();

        // Only alice's embedding should be in the unencrypted list
        assert_eq!(unencrypted.len(), 1);
        assert_eq!(unencrypted[0].1, "alice");
    }
}
