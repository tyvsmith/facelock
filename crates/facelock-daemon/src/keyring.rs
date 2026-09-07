//! The runtime keyring: which key seals new rows, and which other keys can
//! still open rows a previous configuration sealed.
//!
//! `facelock setup` flipping `encryption.method` between `tpm` and `keyfile`
//! used to silently swap the AES key underneath every stored template (#354):
//! both methods write through the same [`SoftwareSealer`] (AES-256-GCM), and
//! the only difference is which 32 bytes back it, so switching methods leaves
//! every row sealed under the *other* key merely unreadable — surfaced as an
//! AAD-shaped decrypt error that named neither the missing key nor how to get
//! it back. Since wave 1 of #354, every row records the `key_id` of whichever
//! key sealed it, so a row that names one can be handed exactly that key
//! instead of a blind trial.
//!
//! [`SealingKeys`] is the one loader both the daemon (`Handler::new`) and the
//! CLI's one-shot path (`facelock-cli::direct`) call. A second copy of this
//! policy is exactly how #354 happened in the first place — two writers
//! minting keys independently — so it lives here once.

use std::path::Path;

use facelock_core::config::{Config, EncryptionMethod};
use facelock_store::FaceStore;
use facelock_tpm::SoftwareSealer;
#[cfg(feature = "tpm")]
use facelock_tpm::TpmSealer;
#[cfg(not(feature = "tpm"))]
use tracing::debug;
use tracing::info;
#[cfg(feature = "tpm")]
use tracing::warn;

/// The sealing keys available to a running daemon or a one-shot invocation:
/// one primary that seals every new row, and zero or more secondaries kept
/// only to open rows a previous configuration sealed under a different key.
pub struct SealingKeys {
    /// The configured method's key — what [`enroll`](crate::enroll::enroll)
    /// seals new rows under. `None` only when the method is `keyfile` and the
    /// key could not be resolved; the reason is in
    /// [`SealingKeys::primary_error`] instead. A `tpm` primary that cannot be
    /// resolved fails [`SealingKeys::load`] itself rather than leaving this
    /// `None` — see there.
    primary: Option<SoftwareSealer>,
    /// Why enroll must fail closed rather than silently downgrade to
    /// plaintext biometric storage: `Some` only when the configured method
    /// requires encryption and [`SealingKeys::primary`] could not be
    /// resolved. Replaces the old `Handler::sealer_init_error` field.
    primary_error: Option<String>,
    /// Other keys this process can still read rows under, by key id — loaded
    /// best-effort from the artifact the *other* encryption method would
    /// have used. Never minted: unlike the primary, this path never goes
    /// through `key_policy::ensure_encrypt_by_default_key`, which must not be
    /// given the chance to write a key here.
    secondary: Vec<(String, SoftwareSealer)>,
}

impl SealingKeys {
    /// `EncryptionMethod::None`: nothing seals new rows, nothing needs
    /// opening.
    pub fn none() -> Self {
        Self {
            primary: None,
            primary_error: None,
            secondary: Vec::new(),
        }
    }

    /// Load the keyring for `config`. `store` is consulted only for the
    /// encrypt-by-default decision a keyfile primary may need to make
    /// ([`crate::key_policy::ensure_encrypt_by_default_key`]).
    ///
    /// - `method = "keyfile"`: the primary is resolved through the same
    ///   encrypt-by-default gate every writer of that key already shares. A
    ///   refusal there is recorded as [`SealingKeys::primary_error`] — with
    ///   the primary left `None` — but secondaries are still attempted, so a
    ///   daemon that cannot mint the new key can still read rows under a key
    ///   it already has.
    /// - `method = "tpm"`: a primary that cannot be resolved is fatal — the
    ///   caller decides whether that means the handler fails to build or the
    ///   one-shot path propagates.
    /// - `method = "none"`: [`SealingKeys::none`].
    ///
    /// Secondary loading never fails the call: a PCR-bound unseal that no
    /// longer matches this host, or a missing artifact, just means one fewer
    /// key is available, logged once and moved past.
    pub fn load(config: &Config, store: &FaceStore) -> Result<Self, String> {
        match config.encryption.method {
            EncryptionMethod::None => Ok(Self::none()),

            EncryptionMethod::Keyfile => {
                // Fail CLOSED on enroll: a caller records `primary_error` as
                // the reason to refuse rather than store a biometric
                // template as plaintext. Auth is untouched by a `None`
                // primary — it falls through to whatever secondaries loaded.
                //
                // Logging this is left to the caller, deliberately: the
                // daemon resolves this once at startup (and again only when
                // recovering from a refusal), so a `warn!` there is heard
                // once, while the one-shot CLI path resolves it on every
                // invocation — `facelock auth` runs on every login — so the
                // same `warn!` there would spam syslog over a refusal that
                // may not even be this caller's problem (#231: the refusal
                // is global to the store, one user's encrypted row can
                // trigger it for everyone).
                let (primary, primary_error) = match resolve_keyfile_sealer(config, store) {
                    Ok(sealer) => (Some(sealer), None),
                    Err(msg) => (None, Some(msg)),
                };
                let secondary = load_tpm_secondary(config, primary.as_ref());
                Ok(Self {
                    primary,
                    primary_error,
                    secondary,
                })
            }

            EncryptionMethod::Tpm => {
                #[cfg(feature = "tpm")]
                {
                    let sealed_path = Path::new(&config.encryption.sealed_key_path);
                    let mut tpm = TpmSealer::new(&config.tpm.tcti)
                        .map_err(|e| format!("TPM initialization failed: {e}"))?;
                    let key = tpm.unseal_key_from_file(sealed_path).map_err(|e| {
                        format!(
                            "failed to unseal AES key from {}: {e}",
                            sealed_path.display()
                        )
                    })?;
                    info!("AES key unsealed from TPM ({})", sealed_path.display());
                    let primary = SoftwareSealer::from_key(key);
                    let secondary = load_keyfile_secondary(config, &primary);
                    Ok(Self {
                        primary: Some(primary),
                        primary_error: None,
                        secondary,
                    })
                }
                #[cfg(not(feature = "tpm"))]
                {
                    Err(
                        "encryption method is 'tpm' but TPM support is not compiled in \
                         (rebuild with --features tpm)"
                            .into(),
                    )
                }
            }
        }
    }

    /// The configured method's key — what enroll seals new rows under.
    /// Re-resolve a keyfile primary that was refused or unreadable at the
    /// last load, leaving every secondary as it is.
    ///
    /// The daemon calls this before each authentication and enrollment while
    /// the primary is missing. Going through [`Self::load`] there would
    /// re-open the TPM and retry the sealed-key unseal on every attempt,
    /// warning each time it fails, for a secondary that has not changed.
    /// A secondary that turns out to be the restored primary's own key is
    /// dropped so the legacy trial never tries the same key twice.
    ///
    /// Only the keyfile method has a refreshable primary: a TPM primary that
    /// fails fails [`Self::load`] itself, so no keyring exists to refresh.
    pub fn refresh_primary(&mut self, config: &Config, store: &FaceStore) {
        if config.encryption.method != EncryptionMethod::Keyfile || self.primary.is_some() {
            return;
        }
        match resolve_keyfile_sealer(config, store) {
            Ok(sealer) => {
                let id = sealer.key_id();
                self.secondary.retain(|(key_id, _)| *key_id != id);
                self.primary = Some(sealer);
                self.primary_error = None;
            }
            Err(msg) => self.primary_error = Some(msg),
        }
    }

    pub fn primary(&self) -> Option<&SoftwareSealer> {
        self.primary.as_ref()
    }

    /// Why enroll must fail closed, when it must.
    pub fn primary_error(&self) -> Option<&str> {
        self.primary_error.as_deref()
    }

    /// The sealer for `id`: the primary first, then any loaded secondary.
    pub fn by_key_id(&self, id: &str) -> Option<&SoftwareSealer> {
        if let Some(primary) = &self.primary
            && primary.key_id() == id
        {
            return Some(primary);
        }
        self.secondary
            .iter()
            .find(|(key_id, _)| key_id == id)
            .map(|(_, sealer)| sealer)
    }

    /// Every loaded key, primary first, worth trying against a pre-V7 row
    /// that names none. Safe even for the wrong key: AES-GCM's tag check
    /// simply fails a trial under the wrong key rather than misreading it.
    pub fn candidates_for_legacy_row(&self) -> impl Iterator<Item = &SoftwareSealer> {
        self.primary
            .iter()
            .chain(self.secondary.iter().map(|(_, sealer)| sealer))
    }

    /// Whether any key — primary or secondary — is available at all.
    pub fn any_loaded(&self) -> bool {
        self.primary.is_some() || !self.secondary.is_empty()
    }

    /// Build a keyring directly from its parts, bypassing [`Self::load`]'s
    /// config-driven resolution. Only [`Self::load`] can produce a live
    /// cross-method secondary outside of tests (it needs a real TPM to unseal
    /// one), so unit tests that exercise `by_key_id`/`candidates_for_legacy_row`
    /// build the shape they need here instead.
    #[cfg(test)]
    pub(crate) fn from_parts(
        primary: Option<SoftwareSealer>,
        secondary: Vec<(String, SoftwareSealer)>,
    ) -> Self {
        Self {
            primary,
            primary_error: None,
            secondary,
        }
    }
}

/// Resolve the keyfile primary: mint the encrypt-by-default key when that is
/// allowed, then read it. `Err` is the reason enroll must fail closed.
///
/// Moved here from `Handler` unchanged (#354 wave 2a) — the daemon and the
/// CLI's one-shot path both called this same sequence independently before,
/// which is the drift this module exists to close.
fn resolve_keyfile_sealer(config: &Config, store: &FaceStore) -> Result<SoftwareSealer, String> {
    let key_path = Path::new(&config.encryption.key_path);
    if let Some(refusal) = crate::key_policy::ensure_encrypt_by_default_key(store, config).refusal()
    {
        return Err(refusal.to_string());
    }
    match SoftwareSealer::from_key_file(key_path) {
        Ok(sealer) => {
            info!(
                "software encryption sealer initialized from {}",
                key_path.display()
            );
            Ok(sealer)
        }
        Err(e) => {
            // A key that cannot be read leaves an operator holding encrypted
            // rows in exactly the predicament a missing one does. The generic
            // byte-count complaint names neither what is at risk nor the
            // artifact that brings it back.
            let complaint = format!("{} keyfile could not be read: {e}", key_path.display());
            match crate::key_policy::encrypted_rows_at_risk(store, config) {
                Some(at_risk) => Err(format!("{complaint} — {at_risk}")),
                None => Err(complaint),
            }
        }
    }
}

/// Best-effort secondary for a keyfile primary: the TPM-sealed key at
/// `sealed_key_path`, left over from before `facelock setup` flipped the
/// method to `keyfile`. Never mints anything — only reads an artifact that is
/// already there — and never blocks or spams: a PCR-bound unseal failing on
/// this host is an expected way for an old key to simply not be available.
fn load_tpm_secondary(
    config: &Config,
    #[cfg_attr(not(feature = "tpm"), allow(unused_variables))] primary: Option<&SoftwareSealer>,
) -> Vec<(String, SoftwareSealer)> {
    let sealed_path = Path::new(&config.encryption.sealed_key_path);
    if !sealed_path.exists() {
        return Vec::new();
    }
    #[cfg(feature = "tpm")]
    {
        match TpmSealer::new(&config.tpm.tcti)
            .and_then(|mut tpm| tpm.unseal_key_from_file(sealed_path))
        {
            Ok(key) => {
                let sealer = SoftwareSealer::from_key(key);
                let id = sealer.key_id();
                // Same bytes as the primary (e.g. `facelock tpm seal-key`
                // sealed the very key `key_path` already holds): nothing a
                // secondary would add.
                if primary.is_some_and(|p| p.key_id() == id) {
                    return Vec::new();
                }
                info!(
                    "secondary sealing key loaded from {} (key id {id})",
                    sealed_path.display()
                );
                vec![(id, sealer)]
            }
            Err(e) => {
                warn!(
                    "secondary sealing key at {} could not be unsealed (PCR-bound unseal \
                     failures are expected on some hosts): {e}",
                    sealed_path.display()
                );
                Vec::new()
            }
        }
    }
    #[cfg(not(feature = "tpm"))]
    {
        debug!(
            "a sealed key is present at {} but cannot be unsealed without the tpm feature",
            sealed_path.display()
        );
        Vec::new()
    }
}

/// Best-effort secondary for a TPM primary: the plain keyfile at `key_path`,
/// left over from before `facelock setup` flipped the method to `tpm`. Read
/// directly — this is just AES key bytes on disk, so no TPM feature or
/// hardware is needed to open it.
#[cfg(feature = "tpm")]
fn load_keyfile_secondary(
    config: &Config,
    primary: &SoftwareSealer,
) -> Vec<(String, SoftwareSealer)> {
    let key_path = Path::new(&config.encryption.key_path);
    if !key_path.exists() {
        return Vec::new();
    }
    match SoftwareSealer::from_key_file(key_path) {
        Ok(sealer) => {
            let id = sealer.key_id();
            if primary.key_id() == id {
                return Vec::new();
            }
            info!(
                "secondary sealing key loaded from {} (key id {id})",
                key_path.display()
            );
            vec![(id, sealer)]
        }
        Err(e) => {
            warn!(
                "secondary sealing key at {} could not be read: {e}",
                key_path.display()
            );
            Vec::new()
        }
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

    #[test]
    fn keyfile_primary_loads_from_a_generated_key() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        let config = config_for(&key_path);
        let store = FaceStore::open_memory().unwrap();

        let keys = SealingKeys::load(&config, &store).unwrap();
        assert!(keys.primary().is_some());
        assert!(keys.primary_error().is_none());
        assert!(keys.any_loaded());
    }

    /// #231: a keyfile refusal (encrypted rows at risk, key missing) must not
    /// stop `load` from returning — enroll fails closed on `primary_error`,
    /// but a running daemon still needs to be able to report the refusal
    /// rather than fail to build at all.
    /// A restored keyfile lifts the refusal through `refresh_primary`
    /// without touching the secondaries, and a secondary that held the
    /// restored key is dropped so the legacy trial never tries it twice.
    #[test]
    fn refresh_primary_lifts_the_refusal_and_drops_a_duplicate_secondary() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        let config = config_for(&key_path);
        let store = FaceStore::open_memory().unwrap();
        let restored = SoftwareSealer::from_key([0x11u8; 32]);
        store
            .add_model_raw(
                "alice",
                "front",
                &restored.seal_embedding(&[0.5f32; 512]).unwrap(),
                true,
                "embedder",
            )
            .unwrap();

        // Refused: encrypted rows exist and the keyfile is absent.
        let mut keys = SealingKeys::load(&config, &store).unwrap();
        assert!(keys.primary().is_none());
        let other = SoftwareSealer::from_key([0x22u8; 32]);
        keys.secondary = vec![
            (restored.key_id(), SoftwareSealer::from_key([0x11u8; 32])),
            (other.key_id(), other),
        ];

        // Still absent: the refresh keeps the refusal and the secondaries.
        keys.refresh_primary(&config, &store);
        assert!(keys.primary().is_none());
        assert!(keys.primary_error().is_some());
        assert_eq!(keys.secondary.len(), 2);

        SoftwareSealer::write_key_file(&key_path, &[0x11u8; 32]).unwrap();
        keys.refresh_primary(&config, &store);
        assert!(keys.primary_error().is_none());
        assert_eq!(keys.primary().unwrap().key_id(), restored.key_id());
        assert_eq!(
            keys.secondary.len(),
            1,
            "the duplicate of the restored key is dropped"
        );
        assert_eq!(keys.candidates_for_legacy_row().count(), 2);
    }

    #[test]
    fn keyfile_refusal_leaves_nothing_loaded_but_records_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        let config = config_for(&key_path);
        let store = FaceStore::open_memory().unwrap();
        store
            .add_model_raw(
                "alice",
                "front",
                &SoftwareSealer::from_key([0x11u8; 32])
                    .seal_embedding(&[0.5f32; 512])
                    .unwrap(),
                true,
                "embedder",
            )
            .unwrap();

        let keys = SealingKeys::load(&config, &store).unwrap();
        assert!(keys.primary().is_none());
        assert!(!keys.any_loaded());
        let reason = keys.primary_error().expect("the refusal must be recorded");
        assert!(reason.contains("facelock clear"), "{reason}");
        assert!(!key_path.exists(), "a replacement key was written");
    }

    #[test]
    fn none_method_has_nothing_loaded() {
        let keys = SealingKeys::none();
        assert!(keys.primary().is_none());
        assert!(!keys.any_loaded());
        assert!(keys.by_key_id("anything").is_none());
        assert_eq!(keys.candidates_for_legacy_row().count(), 0);
    }

    #[test]
    fn by_key_id_checks_the_primary_before_any_secondary() {
        let primary = SoftwareSealer::from_key([0x11u8; 32]);
        let secondary = SoftwareSealer::from_key([0x22u8; 32]);
        let primary_id = primary.key_id();
        let secondary_id = secondary.key_id();
        let keys = SealingKeys::from_parts(Some(primary), vec![(secondary_id.clone(), secondary)]);

        assert!(keys.by_key_id(&primary_id).is_some());
        assert!(keys.by_key_id(&secondary_id).is_some());
        assert!(keys.by_key_id("unknown").is_none());
    }

    #[test]
    fn candidates_for_legacy_row_tries_the_primary_first() {
        let primary = SoftwareSealer::from_key([0x11u8; 32]);
        let secondary = SoftwareSealer::from_key([0x22u8; 32]);
        let primary_id = primary.key_id();
        let secondary_id = secondary.key_id();
        let keys = SealingKeys::from_parts(Some(primary), vec![(secondary_id.clone(), secondary)]);

        let ids: Vec<String> = keys
            .candidates_for_legacy_row()
            .map(|s| s.key_id())
            .collect();
        assert_eq!(
            ids,
            vec![primary_id, secondary_id],
            "primary must come first"
        );
    }

    /// `facelock tpm seal-key`/`unseal-key`/`reseal` can leave both artifacts
    /// holding the *same* key material. A secondary equal to the primary adds
    /// nothing and must not appear twice in the candidate trial.
    #[test]
    fn a_secondary_equal_to_the_primary_is_not_a_second_candidate() {
        let key = [0x33u8; 32];
        let primary = SoftwareSealer::from_key(key);
        let same_key_again = SoftwareSealer::from_key(key);
        let id = primary.key_id();
        // Simulating what `load_tpm_secondary`/`load_keyfile_secondary`
        // already dedupe before ever constructing a `SealingKeys`: build the
        // keyring as if that dedupe had NOT happened, to pin the invariant
        // `by_key_id` relies on — a caller must never fail to find a key that
        // is present under either name.
        let keys = SealingKeys::from_parts(Some(primary), vec![(id.clone(), same_key_again)]);
        assert!(keys.by_key_id(&id).is_some());
    }
}
