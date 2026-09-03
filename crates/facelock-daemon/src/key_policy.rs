//! The one gate every writer of the encrypt-by-default key goes through.
//!
//! Encrypt-by-default means a keyfile system with no key file gets one made
//! for it. That is right exactly once — on a system that has never encrypted
//! anything. On a system whose key artifact went *missing*, the same act
//! writes a replacement over a database full of rows encrypted under the old
//! key: the rows are unreadable either way, but a later restore of the real
//! key no longer matches what facelock has since written, and an operator who
//! still had a backup loses their enrollments for good.
//!
//! Four places used to mint that key independently — the daemon at startup,
//! the one-shot direct path, `facelock setup`'s auto policy, and `facelock
//! encrypt` — and a refusal wired into one of them is not a refusal. They all
//! come here now. The pure predicate ([`key_creation_refusal`]) and the step
//! that actually writes ([`ensure_encrypt_by_default_key`]) are separate so a
//! caller that only wants the decision — `facelock encrypt --generate-key`,
//! which replaces a key rather than creating one — can ask for it without a
//! function called "refusal" writing a file as a side effect.
//!
//! Errors are plain strings for the same reason `crate::embeddings`'s are:
//! they cross D-Bus, land in logs, and are wrapped by each caller's own error
//! type (`DaemonResponse::Error`, `anyhow`).

use std::path::Path;

use facelock_core::config::{Config, EncryptionMethod};
use facelock_store::FaceStore;
use tracing::info;

/// What the gate decided about the configured key file.
#[derive(Debug)]
pub enum KeyfileDecision {
    /// A key file is already at the path — either it was there all along, or a
    /// concurrent creator published one first. This call wrote nothing; read
    /// what is there.
    Present,
    /// This call created the key file.
    Created,
    /// No key was written. The string says why, which artifact is missing, and
    /// what the operator can do about it.
    Refused(String),
}

impl KeyfileDecision {
    /// The refusal text, for a caller that only needs to know whether the gate
    /// said no.
    pub fn refusal(&self) -> Option<&str> {
        match self {
            KeyfileDecision::Refused(message) => Some(message),
            KeyfileDecision::Present | KeyfileDecision::Created => None,
        }
    }
}

/// Whether a *new* encryption key may be minted for `store`. Writes nothing,
/// either way.
///
/// `None` means no stored template would be orphaned by a new key. `Some` is
/// the refusal, naming the encryption method that wrote the rows at risk, the
/// artifact to restore, and the explicit destructive alternative.
///
/// Two facts have to be kept apart. A store that answers "no encrypted rows"
/// and a store that *cannot be asked* are different things, and conflating
/// them is what makes a destructive path fail open — so a failing query is a
/// refusal, not a licence.
///
/// The question is asked of each row's blob, not of the `sealed` column. That
/// column is one bit every method sets: it cannot tell a TPM-sealed row from a
/// keyfile-sealed one, so the old message told operators of TPM systems to
/// restore a key file that had never existed, and it stayed set on a row whose
/// ciphertext had since been replaced with plaintext, refusing to mint a key
/// nothing needed. The version byte at the head of the blob is the fact.
pub fn key_creation_refusal(store: &FaceStore, config: &Config) -> Option<String> {
    let target = configured_key_artifact(config);
    let at_risk = encrypted_rows_at_risk(store, config)?;
    Some(format!(
        "refusing to write an encryption key at {target}: {at_risk}"
    ))
}

/// What a lost key would cost, and how to get it back — or `None` when the
/// store holds nothing a key could orphan.
///
/// Split from [`key_creation_refusal`] because the same facts are needed by a
/// caller that is *not* about to write a key: a key file that exists but
/// cannot be read leaves an operator in exactly the same predicament, and
/// "keyfile could not be read: expected 32 bytes, got 12" tells them nothing
/// about what is at risk or which artifact brings it back.
pub fn encrypted_rows_at_risk(store: &FaceStore, config: &Config) -> Option<String> {
    let shapes = match store.embedding_blob_shapes() {
        Ok(shapes) => shapes,
        Err(e) => {
            return Some(format!(
                "the face database could not be read ({e}), so facelock cannot tell \
                 whether encrypted templates would be orphaned. Fix access to {} and \
                 retry, or clear the enrollments with `facelock clear`.",
                config.storage.db_path
            ));
        }
    };

    let software = shapes
        .iter()
        .filter(|(head, len)| facelock_tpm::is_software_encrypted_shape(*head, *len))
        .count();
    let tpm = shapes
        .iter()
        .filter(|(head, len)| facelock_tpm::is_sealed_shape(*head, *len))
        .count();

    if software == 0 && tpm == 0 {
        return None;
    }

    let mut found = Vec::new();
    if software > 0 {
        found.push(format!(
            "{software} row(s) are software-encrypted (method \"keyfile\", key file {})",
            config.encryption.key_path
        ));
    }
    if tpm > 0 {
        found.push(format!(
            "{tpm} row(s) are TPM-sealed (method \"tpm\", sealed key {})",
            config.encryption.sealed_key_path
        ));
    }

    Some(format!(
        "{}. A new key cannot read them, and writing one makes a later restore of the \
         original useless. Restore the key artifacts for the encryption method that \
         wrote those rows, or clear the encrypted enrollments with `facelock clear` and \
         enrol again. The daemon re-checks the key on the next authentication or \
         enrollment attempt.",
        found.join("; ")
    ))
}

/// Create the encrypt-by-default key file if — and only if — the database
/// holds no encrypted template.
///
/// This is the gate the daemon and the one-shot path run before reading the
/// key. It is consulted *unconditionally*, not only when the key looks absent:
/// `Path::exists` follows a symlink, so a link planted at `key_path` used to
/// answer "the key is there" and skip every check, leaving the reader to load
/// whatever the link named.
pub fn ensure_encrypt_by_default_key(store: &FaceStore, config: &Config) -> KeyfileDecision {
    let key_path = Path::new(&config.encryption.key_path);

    match key_path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return KeyfileDecision::Refused(facelock_tpm::symlink_key_refusal(key_path));
        }
        Ok(_) => return KeyfileDecision::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // "The key is missing" and "the key path cannot be inspected" are
        // different facts, and only the first authorizes writing.
        Err(e) => {
            return KeyfileDecision::Refused(format!(
                "refusing to write an encryption key at {}: the path could not be \
                 inspected ({e}), so facelock cannot tell whether a key is already there.",
                key_path.display()
            ));
        }
    }

    if let Some(refusal) = key_creation_refusal(store, config) {
        return KeyfileDecision::Refused(refusal);
    }

    match facelock_tpm::SoftwareSealer::create_key_file_exclusive(key_path) {
        Ok(true) => {
            info!(
                "generated encryption key at {} (encrypt-by-default)",
                key_path.display()
            );
            KeyfileDecision::Created
        }
        // Another process published between the inspection and the create.
        // Its file is complete by construction; the caller reads it.
        Ok(false) => KeyfileDecision::Present,
        Err(e) => KeyfileDecision::Refused(format!(
            "no encryption key could be created at {}: {e}",
            key_path.display()
        )),
    }
}

/// The key artifact the *configured* method would write, for the refusal's
/// opening clause. The rows at risk may have been written by the other method;
/// the refusal names both, and this names the one about to be replaced.
fn configured_key_artifact(config: &Config) -> &str {
    match config.encryption.method {
        EncryptionMethod::Tpm => &config.encryption.sealed_key_path,
        EncryptionMethod::Keyfile | EncryptionMethod::None => &config.encryption.key_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_for(key_path: &Path) -> Config {
        Config::parse(&format!(
            "[encryption]\nmethod = \"keyfile\"\nkey_path = \"{}\"\n",
            key_path.display()
        ))
        .unwrap()
    }

    fn software_encrypted_row() -> Vec<u8> {
        facelock_tpm::SoftwareSealer::from_key([0x11u8; 32])
            .seal_embedding(&[0.5f32; 512])
            .unwrap()
    }

    #[test]
    fn a_software_encrypted_row_refuses_and_names_the_keyfile() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let config = config_for(&key_path);
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model_raw("alice", "front", &software_encrypted_row(), true, "e")
            .unwrap();

        let refusal = key_creation_refusal(&store, &config).expect("must refuse");
        assert!(refusal.contains("software-encrypted"), "{refusal}");
        assert!(
            refusal.contains(&key_path.display().to_string()),
            "{refusal}"
        );
        assert!(refusal.contains("facelock clear"), "{refusal}");
        assert!(!key_path.exists(), "the predicate must write nothing");
    }

    /// The rows a keyfile system finds may have been written by the TPM
    /// method. Telling that operator to restore a key file they never had is
    /// a remedy that cannot be followed.
    #[test]
    fn tpm_sealed_rows_under_a_keyfile_config_name_the_tpm_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let mut config = config_for(&key_path);
        config.encryption.sealed_key_path = "/etc/facelock/sealed.key".into();
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model_raw("alice", "front", &[0x01u8; 200], true, "e")
            .unwrap();

        let refusal = key_creation_refusal(&store, &config).expect("must refuse");
        assert!(refusal.contains("TPM-sealed"), "{refusal}");
        assert!(refusal.contains("/etc/facelock/sealed.key"), "{refusal}");
    }

    /// The `sealed` flag alone is not evidence. A row whose ciphertext was
    /// replaced with a plaintext template — `facelock decrypt` writing a
    /// 2048-byte blob, a half-finished migration — keeps the bit set, and
    /// refusing over it strands a system that has nothing left to protect.
    #[test]
    fn a_sealed_flag_over_a_plaintext_template_is_not_an_encrypted_row() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let config = config_for(&key_path);
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model_raw("alice", "front", &[0u8; 2048], true, "e")
            .unwrap();

        assert!(key_creation_refusal(&store, &config).is_none());
        assert_eq!(
            store.count_sealed().unwrap(),
            (1, 0),
            "the flag is still set"
        );
    }

    #[test]
    fn a_plaintext_only_database_still_gets_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let config = config_for(&key_path);
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model_raw("alice", "front", &[0u8; 2048], false, "e")
            .unwrap();

        assert!(matches!(
            ensure_encrypt_by_default_key(&store, &config),
            KeyfileDecision::Created
        ));
        assert_eq!(std::fs::metadata(&key_path).unwrap().len(), 32);
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&key_path).unwrap().permissions()
            ) & 0o777,
            0o600
        );
    }

    #[test]
    fn an_existing_key_is_present_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        let config = config_for(&key_path);
        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let before = std::fs::read(&key_path).unwrap();
        let store = FaceStore::open_memory().unwrap();

        assert!(matches!(
            ensure_encrypt_by_default_key(&store, &config),
            KeyfileDecision::Present
        ));
        assert_eq!(std::fs::read(&key_path).unwrap(), before);
    }

    /// `docs/contracts.md` promises the symlink refusal. It was unreachable:
    /// the gate ran only when `exists()` was false, and `exists()` follows the
    /// link, so a link to a real file looked like a key that was already
    /// there.
    #[test]
    fn a_resolvable_symlink_at_the_key_path_is_its_own_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("attacker.key");
        facelock_tpm::SoftwareSealer::generate_key_file(&target).unwrap();
        let key_path = dir.path().join("encryption.key");
        std::os::unix::fs::symlink(&target, &key_path).unwrap();
        let config = config_for(&key_path);
        let store = FaceStore::open_memory().unwrap();

        let decision = ensure_encrypt_by_default_key(&store, &config);
        let refusal = decision.refusal().expect("a link is a refusal");
        assert!(refusal.contains("symlink"), "{refusal}");
    }

    #[test]
    fn a_dangling_symlink_at_the_key_path_is_the_same_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("encryption.key");
        std::os::unix::fs::symlink(dir.path().join("gone"), &key_path).unwrap();
        let config = config_for(&key_path);
        let store = FaceStore::open_memory().unwrap();

        // `exists()` is false for a dangling link, so this used to reach the
        // creator, which errored — and the caller downgraded that error to a
        // warning and carried on as though no refusal had been made.
        assert!(!key_path.exists());
        let decision = ensure_encrypt_by_default_key(&store, &config);
        let refusal = decision.refusal().expect("a link is a refusal");
        assert!(refusal.contains("symlink"), "{refusal}");
    }

    /// A store that cannot be asked is not a store that answered "nothing to
    /// protect". This is the arm that decides whether a destructive path fails
    /// open.
    #[test]
    fn a_store_that_cannot_be_asked_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("faces.db");
        let key_path = dir.path().join("encryption.key");
        let mut config = config_for(&key_path);
        config.storage.db_path = db_path.display().to_string();

        {
            let store = FaceStore::create(&db_path).unwrap();
            store
                .add_model_raw("alice", "front", &software_encrypted_row(), true, "e")
                .unwrap();
        }
        facelock_test_support::schema_faults::break_face_embeddings_table(&db_path);
        let store = FaceStore::open_existing(&db_path).unwrap();

        let decision = ensure_encrypt_by_default_key(&store, &config);
        let refusal = decision.refusal().expect("an unaskable store must refuse");
        assert!(
            refusal.contains("could not be read"),
            "the refusal must name the store failure, not invent an answer: {refusal}"
        );
        assert!(
            refusal.contains(&db_path.display().to_string()),
            "{refusal}"
        );
        assert!(!key_path.exists());
    }
}
