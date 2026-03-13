use std::path::Path;

use anyhow::{Context, Result};
use facelock_core::config::Config;
use facelock_store::FaceStore;

pub fn run_encrypt(generate_key: bool) -> Result<()> {
    crate::ipc_client::require_root("sudo facelock encrypt")?;
    let config = Config::load()?;
    let key_path = Path::new(&config.encryption.key_path);

    if generate_key || !key_path.exists() {
        println!("Generating encryption key at {}...", key_path.display());
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to generate encryption key")?;
        println!("Key generated (permissions: 0600 root-only).");

        if !generate_key {
            println!("Proceeding to encrypt embeddings...");
        } else {
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

    let sealer = facelock_tpm::SoftwareSealer::from_key_file(key_path)
        .context("failed to load encryption key")?;

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

    println!("Encrypting {} unencrypted embedding(s)...", unencrypted.len());

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
    let key_path = Path::new(&config.encryption.key_path);

    let store = FaceStore::open(Path::new(&config.storage.db_path))
        .context("failed to open face database")?;

    let all = store
        .get_all_embeddings_raw()
        .context("failed to read embeddings")?;

    // Filter to software-encrypted embeddings
    let encrypted: Vec<_> = all
        .iter()
        .filter(|(_, _, blob, _)| facelock_tpm::is_software_encrypted(blob))
        .collect();

    if encrypted.is_empty() {
        println!("No software-encrypted embeddings found. Nothing to do.");
        return Ok(());
    }

    let sealer = facelock_tpm::SoftwareSealer::from_key_file(key_path)
        .context("failed to load encryption key")?;

    println!(
        "Decrypting {} software-encrypted embedding(s)...",
        encrypted.len()
    );

    let mut decrypted_count = 0u32;
    for (id, _user, blob, _sealed) in &encrypted {
        let raw = sealer
            .unseal_bytes(blob)
            .with_context(|| format!("failed to decrypt embedding {id}"))?;

        store
            .update_embedding_sealed(*id, &raw, false)
            .with_context(|| format!("failed to update embedding {id}"))?;

        decrypted_count += 1;
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
        store.add_model("alice", "front", &emb).unwrap();
        store.add_model("alice", "side", &emb).unwrap();

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
            store.update_embedding_sealed(*id, &encrypted_blob, true).unwrap();
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
        store.add_model("alice", "raw", &emb).unwrap();

        // Pre-encrypt one embedding manually
        let key = [0x42u8; 32];
        let sealer = facelock_tpm::SoftwareSealer::from_key(key);
        let raw_bytes: Vec<u8> = emb.iter().flat_map(|f| f.to_le_bytes()).collect();
        let encrypted_blob = sealer.seal_bytes(&raw_bytes).unwrap();
        store.add_model_raw("bob", "encrypted", &encrypted_blob, true).unwrap();

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
