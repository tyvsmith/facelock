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

/// The refusal that stops any key writer here from orphaning stored templates,
/// or `None` when nothing is at risk.
///
/// `open_existing`, never `create`: a database that is simply not there yet
/// has no templates to orphan, and probing must not bring one into being at
/// whatever path a typo'd config names. Every other failure class is "facelock
/// cannot tell", which the shared gate turns into a refusal.
fn key_replacement_refusal(config: &Config) -> Result<Option<String>> {
    match crate::direct::open_store_existing(config) {
        Ok(store) => Ok(facelock_daemon::key_policy::key_creation_refusal(
            &store, config,
        )),
        Err(facelock_store::StoreError::Absent { .. }) => Ok(None),
        Err(e) => Ok(Some(format!(
            "refusing to write an encryption key: the face database at {} could not be \
             read ({e}), so facelock cannot tell whether existing templates would be \
             orphaned. Fix access to it and retry, or clear the enrollments with \
             `facelock clear`.",
            config.storage.db_path
        ))),
    }
}

pub fn run_encrypt(config: &Config, generate_key: bool) -> Result<()> {
    // Root is established by `main`'s `require_root_for` gate (C6) before
    // `tpm::run` dispatches here.
    if generate_key {
        // `--generate-key` is an explicit request to *replace* the key, which
        // is exactly the act that makes rows written under the old one
        // permanently unrecoverable. It is allowed on a database with nothing
        // encrypted in it, and otherwise refused with `facelock clear` named
        // as the destructive step the operator can take deliberately (#231).
        if let Some(refusal) = key_replacement_refusal(config)? {
            bail!("{refusal}");
        }
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
                    "\nTo encrypt embeddings, run: sudo facelock tpm encrypt\n\
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

    // This command re-seals rows from a query that carries no device id, so
    // under hard binding it could only produce id-bearing rows with no AAD,
    // which the auth path then fails to open (#312). A bound row is something
    // only enrollment can write.
    if config.hard_binding_active() {
        bail!(
            "refusing to encrypt in place: security.bind_device_aad = true seals each \
             template under its enrolling camera's device id, which this command cannot \
             supply. Re-enroll to bind these templates, or set security.bind_device_aad = \
             false before running `facelock tpm encrypt`."
        );
    }

    // `open_existing`, never `create`: nothing to encrypt or decrypt means a
    // missing database is an error to report, not a file to bring into being
    // at whatever path a typo'd config names.
    //
    // Opened *before* any key is written. This command is the one the setup
    // hint points operators at when encryption looks broken, so it is the one
    // they run on a system whose key artifact has gone missing — and it used
    // to mint a replacement over their encrypted rows and then report
    // "Nothing to do", which is the ratchet the daemon refusal exists to
    // prevent, reachable from a single privileged command.
    let store = FaceStore::open_existing(Path::new(&config.storage.db_path))
        .context("failed to open face database")?;

    // For non-generate runs, if method is keyfile and key doesn't exist,
    // generate it — through the gate shared with the daemon, the one-shot path
    // and `facelock setup`.
    if config.encryption.method != EncryptionMethod::Tpm {
        let key_path = Path::new(&config.encryption.key_path);
        let existed = key_path.exists();
        if let Some(refusal) =
            facelock_daemon::key_policy::ensure_encrypt_by_default_key(&store, config).refusal()
        {
            bail!("{refusal}");
        }
        if !existed {
            println!("Generated encryption key at {}.", key_path.display());
            println!("Proceeding to encrypt embeddings...");
        }
    }

    let sealer = obtain_sealer(config).context("failed to obtain encryption sealer")?;

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

pub fn run_decrypt(config: &Config) -> Result<()> {
    // Root is established by `main`'s `require_root_for` gate (C6) before
    // `tpm::run` dispatches here.
    // `open_existing`, never `create`: nothing to encrypt or decrypt means a
    // missing database is an error to report, not a file to bring into being
    // at whatever path a typo'd config names.
    let store = FaceStore::open_existing(Path::new(&config.storage.db_path))
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
        let sealer = obtain_sealer(config).context("failed to obtain encryption sealer")?;

        println!(
            "Decrypting {} software-encrypted embedding(s)...",
            sw_encrypted.len()
        );

        for (id, _user, blob, _sealed) in &sw_encrypted {
            // Unsealed plaintext template bytes, zeroized when `raw` drops —
            // the store-update error path included (#293). `Zeroizing`, not
            // `Wiped`: that guard is typed for embedding sets, and raw byte
            // buffers already implement `Zeroize`.
            let raw =
                zeroize::Zeroizing::new(sealer.unseal_bytes(blob).with_context(|| {
                    format!("failed to decrypt software-encrypted embedding {id}")
                })?);

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
                // Same wipe-on-drop as the software branch above (#293).
                let raw = zeroize::Zeroizing::new(
                    tpm.unseal_bytes(blob)
                        .with_context(|| format!("failed to unseal TPM embedding {id}"))?,
                );

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
mod key_gate_tests {
    use facelock_core::config::Config;
    use facelock_store::FaceStore;

    fn temp_db(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "facelock-encrypt-{name}-{}-{unique}.db",
            std::process::id()
        ))
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    }

    fn config_for(key_path: &std::path::Path, db_path: &std::path::Path) -> Config {
        Config::parse(&format!(
            "[encryption]\nmethod = \"keyfile\"\nkey_path = \"{}\"\n\
             [storage]\ndb_path = \"{}\"\n",
            key_path.display(),
            db_path.display()
        ))
        .unwrap()
    }

    fn row_sealed_under(key: [u8; 32]) -> Vec<u8> {
        facelock_tpm::SoftwareSealer::from_key(key)
            .seal_embedding(&[0.5f32; 512])
            .unwrap()
    }

    /// `message/setup.rs` points operators at this command by name when
    /// encryption looks broken, so it is the command they run on a system
    /// whose key artifact went missing — and it used to mint a replacement
    /// over their encrypted rows and report "Nothing to do". One privileged
    /// command reached the exact ratchet the daemon refusal exists to prevent.
    #[test]
    fn encrypt_refuses_to_mint_a_replacement_key_over_encrypted_rows() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let db_path = temp_db("mint-hole");
        {
            let store = FaceStore::create(&db_path).unwrap();
            store
                .add_model_raw("alice", "front", &row_sealed_under([0x11; 32]), true, "e")
                .unwrap();
        }

        let error = format!(
            "{:#}",
            super::run_encrypt(&config_for(&key_path, &db_path), false).unwrap_err()
        );
        assert!(
            error.contains("software-encrypted") && error.contains("facelock clear"),
            "the refusal must name what is at risk and the remedy: {error}"
        );
        assert!(!key_path.exists(), "a replacement key was written");
        cleanup(&db_path);
    }

    /// `--generate-key` truncates in place, which is right when an operator
    /// asked for a new key and catastrophic when rows were written under the
    /// old one. It stays allowed on a database with nothing encrypted in it.
    #[test]
    fn generate_key_refuses_to_truncate_a_live_key_over_encrypted_rows() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let db_path = temp_db("generate-key");
        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let before = std::fs::read(&key_path).unwrap();
        {
            let store = FaceStore::create(&db_path).unwrap();
            store
                .add_model_raw("alice", "front", &row_sealed_under([0x11; 32]), true, "e")
                .unwrap();
        }

        let error = format!(
            "{:#}",
            super::run_encrypt(&config_for(&key_path, &db_path), true).unwrap_err()
        );
        assert!(error.contains("facelock clear"), "{error}");
        assert_eq!(
            std::fs::read(&key_path).unwrap(),
            before,
            "the live key was overwritten in place"
        );
        cleanup(&db_path);
    }

    #[test]
    fn generate_key_still_works_when_nothing_is_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let db_path = temp_db("generate-key-clean");
        {
            let store = FaceStore::create(&db_path).unwrap();
            store
                .add_model("alice", "front", &[0.5f32; 512], "e")
                .unwrap();
        }

        super::run_encrypt(&config_for(&key_path, &db_path), true).unwrap();
        assert_eq!(std::fs::metadata(&key_path).unwrap().len(), 32);
        cleanup(&db_path);
    }

    /// The command's own job still works: a plaintext database gets its key
    /// minted and its rows encrypted.
    #[test]
    fn encrypt_still_mints_a_key_for_a_plaintext_database() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let db_path = temp_db("plaintext");
        {
            let store = FaceStore::create(&db_path).unwrap();
            store
                .add_model("alice", "front", &[0.5f32; 512], "e")
                .unwrap();
        }

        super::run_encrypt(&config_for(&key_path, &db_path), false).unwrap();
        assert_eq!(std::fs::metadata(&key_path).unwrap().len(), 32);

        let store = FaceStore::open_existing(&db_path).unwrap();
        let (sealed, unsealed) = store.count_sealed().unwrap();
        assert_eq!((sealed, unsealed), (1, 0));
        cleanup(&db_path);
    }
}

#[cfg(test)]
mod tests {
    use facelock_core::config::EncryptionMethod;

    #[test]
    fn encryption_method_default() {
        // Encrypt-by-default (finding #8): the default method is now keyfile.
        assert_eq!(EncryptionMethod::default(), EncryptionMethod::Keyfile);
    }

    /// #312: `tpm encrypt` re-seals rows without their device ids, so under
    /// hard binding it would manufacture id-bearing rows with no AAD that
    /// never open again. Refused before the key or the store is touched.
    #[test]
    fn encrypt_in_place_is_refused_under_hard_binding() {
        use facelock_core::config::Config;

        let mut config = Config::parse("[device]\npath = \"/dev/video0\"\n").unwrap();
        config.security.bind_device_aad = true;
        config.encryption.key_path = "/nonexistent/facelock-test/never-created.key".into();
        let err = super::run_encrypt(&config, false).unwrap_err().to_string();
        assert!(err.contains("security.bind_device_aad"), "{err}");
        assert!(err.contains("Re-enroll"), "{err}");
        assert!(
            !std::path::Path::new(&config.encryption.key_path).exists(),
            "refused before generating a key"
        );
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

    /// End-to-end opt-in hard device binding (Plan 04 AAD seam): a template
    /// sealed with AAD from its enrolling device id decrypts only under that
    /// same id, exercising the store + config + sealer path used by enroll/auth.
    #[test]
    fn aad_binding_ties_template_to_device() {
        use facelock_core::config::Config;

        let store = facelock_store::FaceStore::open_memory().unwrap();
        let mut config = Config::parse("[device]\npath = \"/dev/video0\"\n").unwrap();
        config.security.bind_device_aad = true;

        let sealer = facelock_tpm::SoftwareSealer::from_key([0x42u8; 32]);
        let device_id = "046d:085e:SER";
        let aad = config.security.device_aad(Some(device_id));
        assert!(aad.is_some(), "AAD must be derived when opt-in is on");

        let emb = [0.3f32; 512];
        let sealed = sealer
            .seal_embedding_with_aad(&emb, aad.as_deref())
            .unwrap();
        store
            .add_model_raw_with_device("alice", "cam", &sealed, true, "w600k", Some(device_id))
            .unwrap();

        // Load with each row's own device id → decrypts.
        let rows = store.get_user_embeddings_raw_with_device("alice").unwrap();
        let (_id, blob, sealed_flag, dev) = &rows[0];
        assert!(*sealed_flag);
        let good_aad = config.security.device_aad(dev.as_deref());
        let dec = sealer
            .unseal_embedding_with_aad(blob, good_aad.as_deref())
            .unwrap();
        assert_eq!(dec, emb);

        // A forged/swapped device id yields a different AAD → decryption fails.
        let forged_aad = config.security.device_aad(Some("ffff:ffff:forged"));
        assert!(
            sealer
                .unseal_embedding_with_aad(blob, forged_aad.as_deref())
                .is_err(),
            "template must not decrypt under a different camera id"
        );
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
