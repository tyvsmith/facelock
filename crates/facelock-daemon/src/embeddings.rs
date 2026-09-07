//! The one embedding-decrypt implementation (N10).
//!
//! The daemon handler and the CLI's direct path both load a user's stored
//! templates and decrypt software-encrypted (version byte 0x02) or TPM-sealed
//! (0x01/0x03) rows. The per-row logic used to live twice — `Handler` and
//! `facelock-cli`'s `direct::load_user_embeddings` — and drifted once already
//! (B3: the direct copy predated `seal_database` handling). This module is
//! that loop, once.
//!
//! What stays with the callers, deliberately:
//! - the fast path (`get_user_embeddings` when nothing could have written an
//!   encrypted row) and the raw-row fetch, because they belong to each
//!   caller's store-access policy;
//! - sealer *initialization* policy — the daemon fails enroll closed on a
//!   broken keyfile while auth continues, the direct path propagates — which
//!   is a real posture difference, not duplication.
//!
//! Errors are plain strings: the handler wraps them in
//! `DaemonResponse::Error`, the direct path in `anyhow`. They are
//! machine-facing (they cross D-Bus and land in logs) and must not localize.

use facelock_core::config::{Config, EncryptionMethod};
use facelock_core::types::{DeviceBinding, FaceEmbedding, Wiped};
use facelock_store::RawEmbeddingRow;
use tracing::{debug, warn};

use crate::keyring::SealingKeys;

/// How the decrypt loop reaches a TPM sealer when a TPM-sealed row appears.
///
/// The two callers hold the sealer differently and that difference is worth
/// keeping: the daemon initializes once at startup (a failure there is warned
/// once and every sealed row afterwards reports it), while the one-shot direct
/// path connects lazily so unsealed stores never pay a TPM round-trip.
pub enum TpmAccess<'a> {
    /// A long-lived sealer initialized at handler construction. `None` means
    /// `tpm.seal_database` is configured but initialization failed at startup.
    Held(Option<&'a mut facelock_tpm::TpmSealer>),
    /// Connect on the first sealed row, using the configured TCTI. Without
    /// the `tpm` feature the passthrough sealer connects trivially and each
    /// unseal reports a clear "compile with tpm" error instead of misreading
    /// the blob.
    Lazy {
        tcti: &'a str,
        sealer: Option<facelock_tpm::TpmSealer>,
    },
}

impl TpmAccess<'_> {
    fn sealer(&mut self) -> Result<&mut facelock_tpm::TpmSealer, String> {
        match self {
            TpmAccess::Held(Some(sealer)) => Ok(sealer),
            TpmAccess::Held(None) => {
                Err("TPM-sealed embeddings exist but TPM is not available".into())
            }
            TpmAccess::Lazy { tcti, sealer } => {
                if sealer.is_none() {
                    let connected = facelock_tpm::TpmSealer::new(tcti)
                        .map_err(|e| format!("TPM initialization failed: {e}"))?;
                    *sealer = Some(connected);
                }
                Ok(sealer.as_mut().expect("just connected"))
            }
        }
    }
}

/// True when the store must be read through the raw-row path: a software
/// sealer is active, `tpm.seal_database` says TPM-sealed rows may exist, or a
/// configured encryption method has no usable sealer at all.
///
/// The first two are about rows that *can* be decrypted. The third is about
/// rows that cannot, and it matters for the same reason: the fast path would
/// hand sealed blobs to callers as if they were raw embeddings, which at best
/// reports a misleading "corrupt store" and at worst misreads a template (the
/// B3 class — the handler previously made exactly this mistake). A daemon
/// refusing to mint a replacement key is exactly that state, and telling an
/// operator whose key went missing that their database is corrupt sends them
/// to reinstall the one file that still holds their enrollments.
///
/// The raw path is not a fallback to a worse answer here: a user whose rows
/// are all plaintext still authenticates through it, and a user whose rows are
/// encrypted gets a decrypt failure naming the missing key. Both beat a
/// corruption report.
pub fn needs_raw_rows(
    config: &Config,
    software_sealer_active: bool,
    sealer_unavailable: bool,
) -> bool {
    software_sealer_active || config.tpm.seal_database || sealer_unavailable
}

/// Decrypt raw [`RawEmbeddingRow`] rows into embeddings.
///
/// Per row: software-encrypted blobs unseal with this template's own
/// device-derived AAD (opt-in, matching enroll) and the key that sealed it —
/// a row naming a `key_id` (#354 wave 2 onward) is handed exactly that key
/// from `keys`, and a pre-V7 row naming none is tried against every key
/// `keys` has loaded, primary first. TPM-sealed blobs unseal through `tpm`;
/// plaintext rows are size-checked and cast. The first failing row fails the
/// whole load — a partial compare set would silently narrow authentication.
///
/// Under hard device binding, rows with no device id are legacy unbound:
/// they decrypt (never a lockout) and are reported once per load, before
/// decryption, so a row failing elsewhere in the store does not hide them
/// and the operator knows which templates to re-enroll (#312).
pub fn decrypt_user_embeddings(
    raw_rows: &[RawEmbeddingRow],
    config: &Config,
    keys: &SealingKeys,
    tpm: TpmAccess<'_>,
) -> Result<Vec<(u32, FaceEmbedding)>, String> {
    // Reported before decrypting, so the diagnostic reaches the log even
    // when another row fails the load (a mixed store, see #312).
    let unbound = unbound_model_ids(raw_rows, config);
    if !unbound.is_empty() {
        warn!(
            models = ?unbound,
            "hard device binding: templates with no device id authenticate as unbound (re-enroll to bind)"
        );
    }
    let mut results = Vec::with_capacity(raw_rows.len());
    decrypt_rows_into(raw_rows, config, keys, tpm, &mut results)?;
    Ok(results)
}

/// The models among `raw_rows` that stand as [`DeviceBinding::LegacyUnbound`]
/// under the configured policy, each listed once. Empty unless
/// `security.bind_device_aad` is on.
fn unbound_model_ids(raw_rows: &[RawEmbeddingRow], config: &Config) -> Vec<u32> {
    let mut unbound: Vec<u32> = raw_rows
        .iter()
        .filter(|row| {
            config.classify_device_binding(row.device_id.as_deref()) == DeviceBinding::LegacyUnbound
        })
        .map(|row| row.model_id)
        .collect();
    unbound.sort_unstable();
    unbound.dedup();
    unbound
}

/// The decrypt loop, accumulating into `out` through [`Wiped`]'s borrowed
/// form. On any exit before the last row — a failing row (AAD mismatch, TPM
/// unseal failure) or an unwind — every embedding already decrypted is
/// zeroized in place, so a mid-loop failure never strands rows 1..N
/// plaintext in freed heap (#293). On success the rows are handed back live:
/// from there the caller owns the plaintext, and every caller either wraps
/// it or passes it to the wiping auth loop (D11).
fn decrypt_rows_into(
    raw_rows: &[RawEmbeddingRow],
    config: &Config,
    keys: &SealingKeys,
    mut tpm: TpmAccess<'_>,
    out: &mut Vec<(u32, FaceEmbedding)>,
) -> Result<(), String> {
    let mut guarded = Wiped::new(&mut *out);
    for RawEmbeddingRow {
        model_id: id,
        blob,
        sealed,
        device_id,
        key_id,
    } in raw_rows
    {
        let embedding = if *sealed && facelock_tpm::is_software_encrypted(blob) {
            // Software-encrypted (version byte 0x02)
            decrypt_software_row(
                *id,
                blob,
                device_id.as_deref(),
                key_id.as_deref(),
                config,
                keys,
            )?
        } else if *sealed {
            // TPM-sealed (version byte 0x01/0x03)
            tpm.sealer()?
                .unseal_embedding(blob)
                .map_err(|e| format!("TPM unseal failed for embedding {id}: {e}"))?
        } else {
            // Plaintext raw embedding
            if blob.len() != 512 * 4 {
                return Err(format!(
                    "invalid raw embedding size for id {id}: expected {} bytes, got {}",
                    512 * 4,
                    blob.len()
                ));
            }
            let floats: &[f32] = bytemuck::cast_slice(blob);
            let mut emb = [0f32; 512];
            emb.copy_from_slice(floats);
            emb
        };
        guarded.push((*id, embedding));
    }
    // Every row decrypted: the caller takes the plaintext over. Forgetting
    // the guard skips its wipe without leaking — it owns only the borrow.
    std::mem::forget(guarded);
    Ok(())
}

/// Decrypt one software-encrypted (version byte 0x02) row.
///
/// A row naming a `key_id` gets exactly that key — no trial against any
/// other, even when one is loaded — and a lookup miss fails by name rather
/// than falling back to "no key is configured", which was never true. A
/// pre-V7 row naming none is tried against every key `keys` has loaded,
/// primary first: AES-GCM makes this safe, because a wrong key fails the tag
/// check rather than misreading the blob.
fn decrypt_software_row(
    id: u32,
    blob: &[u8],
    device_id: Option<&str>,
    key_id: Option<&str>,
    config: &Config,
    keys: &SealingKeys,
) -> Result<FaceEmbedding, String> {
    let aad = config.security.device_aad(device_id);

    if let Some(wanted) = key_id {
        let sealer = keys.by_key_id(wanted).ok_or_else(|| {
            format!(
                "embedding {id} was sealed under key {wanted}, which is not loadable: restore \
                 {} (the artifact for the other encryption method) or re-enroll",
                other_key_artifact(config)
            )
        })?;
        return sealer
            .unseal_embedding_with_aad(blob, aad.as_deref())
            .map_err(|e| software_decrypt_error(id, &e.to_string(), device_id, aad.is_some()));
    }

    // Pre-V7 row: no key_id recorded, so try every key this process has,
    // primary first.
    if !keys.any_loaded() {
        return Err(format!(
            "embedding {id} is software-encrypted but no key is configured"
        ));
    }
    let mut last_error: Option<String> = None;
    for (index, candidate) in keys.candidates_for_legacy_row().enumerate() {
        match candidate.unseal_embedding_with_aad(blob, aad.as_deref()) {
            Ok(embedding) => {
                if index > 0 {
                    // `debug`, not `info`: this runs on every authentication
                    // for every legacy row, so at `info` a diverged host
                    // would log a line per row per attempt in steady state.
                    debug!(
                        model_id = id,
                        key_id = %candidate.key_id(),
                        "legacy embedding decrypted under a secondary key"
                    );
                }
                return Ok(embedding);
            }
            Err(e) => last_error = Some(e.to_string()),
        }
    }
    let mut message = software_decrypt_error(
        id,
        last_error
            .as_deref()
            .unwrap_or("no loadable key could open it"),
        device_id,
        aad.is_some(),
    );
    message.push_str("; or sealed under a key that is no longer loadable");
    Err(message)
}

/// The base "software decryption failed" message and its AAD hints, shared by
/// the named-key and legacy-trial paths.
///
/// One cause the cipher cannot tell from a wrong key or a corrupt blob: a row
/// that records a device id but was sealed before hard binding was enabled
/// carries no AAD, so it cannot open under the one derived now. Name that
/// possibility and its fix without asserting it.
fn software_decrypt_error(
    id: u32,
    cause: &str,
    device_id: Option<&str>,
    aad_present: bool,
) -> String {
    let mut message = format!("software decryption failed for embedding {id}: {cause}");
    let has_id = device_id.is_some_and(|d| !d.is_empty());
    if aad_present {
        message
            .push_str("; if this template predates security.bind_device_aad, re-enroll to bind it");
    } else if has_id {
        // The other direction: the flag was turned off, or encryption
        // disabled, over a store sealed under it.
        message.push_str(
            "; if this template was sealed under security.bind_device_aad, re-enable it or \
             re-enroll",
        );
    }
    message
}

/// The artifact the *other* encryption method would have written — named in
/// the refusal for a row whose `key_id` names a key nothing loaded, so the
/// operator restores the artifact that matches the row rather than the one
/// the current method already reads.
fn other_key_artifact(config: &Config) -> String {
    match config.encryption.method {
        EncryptionMethod::Tpm => config.encryption.key_path.clone(),
        EncryptionMethod::Keyfile | EncryptionMethod::None => {
            let sealed = &config.encryption.sealed_key_path;
            #[cfg(feature = "tpm")]
            {
                sealed.clone()
            }
            #[cfg(not(feature = "tpm"))]
            {
                // Restoring the sealed key alone cannot help here: this build
                // has no TPM support to unseal it with.
                format!("{sealed} under a build with the tpm feature (this one has none)")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a [`RawEmbeddingRow`] for these tests, with `key_id` left
    /// `None` (a pre-V7/legacy row). Tests that need a row naming a key
    /// construct a [`RawEmbeddingRow`] directly.
    fn raw_row(
        model_id: u32,
        blob: Vec<u8>,
        sealed: bool,
        device_id: Option<String>,
    ) -> RawEmbeddingRow {
        RawEmbeddingRow {
            model_id,
            blob,
            sealed,
            device_id,
            key_id: None,
        }
    }

    fn plaintext_row(id: u32, value: f32) -> RawEmbeddingRow {
        let emb: FaceEmbedding = [value; 512];
        raw_row(id, bytemuck::cast_slice(&emb).to_vec(), false, None)
    }

    #[test]
    fn plaintext_rows_round_trip() {
        let config = Config::parse("").unwrap();
        let rows = vec![plaintext_row(1, 0.25), plaintext_row(2, 0.75)];
        let out =
            decrypt_user_embeddings(&rows, &config, &SealingKeys::none(), TpmAccess::Held(None))
                .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], (1, [0.25; 512]));
        assert_eq!(out[1], (2, [0.75; 512]));
    }

    #[test]
    fn wrong_size_plaintext_row_fails_the_load() {
        let config = Config::parse("").unwrap();
        let rows = vec![raw_row(7, vec![0u8; 100], false, None)];
        let err =
            decrypt_user_embeddings(&rows, &config, &SealingKeys::none(), TpmAccess::Held(None))
                .unwrap_err();
        assert!(err.contains("invalid raw embedding size for id 7"), "{err}");
    }

    #[test]
    fn software_row_without_key_names_the_row() {
        let config = Config::parse("").unwrap();
        // Version byte 0x02 marks software encryption.
        let mut blob = vec![0x02u8];
        blob.extend_from_slice(&[0u8; 64]);
        let rows = vec![raw_row(3, blob, true, None)];
        let err =
            decrypt_user_embeddings(&rows, &config, &SealingKeys::none(), TpmAccess::Held(None))
                .unwrap_err();
        assert_eq!(
            err,
            "embedding 3 is software-encrypted but no key is configured"
        );
    }

    #[test]
    fn software_rows_decrypt_with_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let sealer = facelock_tpm::SoftwareSealer::from_key_file(&key_path).unwrap();

        let emb: FaceEmbedding = [0.5; 512];
        let blob = sealer.seal_embedding(&emb).unwrap();
        let rows = vec![raw_row(4, blob, true, None)];

        let config = Config::parse("").unwrap();
        let keys = SealingKeys::from_parts(Some(sealer), vec![]);
        let out = decrypt_user_embeddings(&rows, &config, &keys, TpmAccess::Held(None)).unwrap();
        assert_eq!(out, vec![(4, emb)]);
    }

    /// The daemon's held-sealer failure mode: seal_database configured but
    /// TPM init failed at startup — every sealed row reports it.
    #[test]
    fn tpm_row_with_no_held_sealer_reports_unavailable() {
        let config = Config::parse("").unwrap();
        let mut blob = vec![0x01u8];
        blob.extend_from_slice(&[0u8; 64]);
        let rows = vec![raw_row(5, blob, true, None)];
        let err =
            decrypt_user_embeddings(&rows, &config, &SealingKeys::none(), TpmAccess::Held(None))
                .unwrap_err();
        assert_eq!(err, "TPM-sealed embeddings exist but TPM is not available");
    }

    /// The direct path's lazy connection (B3 pin, relocated with the loop):
    /// without the `tpm` feature the passthrough sealer connects and reports
    /// a clear per-row error instead of misreading the blob.
    #[cfg(not(feature = "tpm"))]
    #[test]
    fn tpm_row_via_lazy_passthrough_errors_clearly() {
        let config = Config::parse("").unwrap();
        let mut blob = vec![0x01u8];
        blob.extend_from_slice(&[0u8; 64]);
        let rows = vec![raw_row(6, blob, true, None)];
        let err = decrypt_user_embeddings(
            &rows,
            &config,
            &SealingKeys::none(),
            TpmAccess::Lazy {
                tcti: "device",
                sealer: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("TPM unseal failed for embedding 6"), "{err}");
        assert!(err.contains("without TPM support"), "{err}");
    }

    /// The #293 auth-path case: a row failing mid-loop must not strand the
    /// rows already decrypted before it. The borrowed accumulation makes the
    /// wipe observable — without the guard, `out` holds rows 1-2 plaintext.
    #[test]
    fn mid_loop_failure_wipes_rows_already_decrypted() {
        let config = Config::parse("").unwrap();
        let mut rows = vec![plaintext_row(1, 0.25), plaintext_row(2, 0.75)];
        // Row 3 fails: software-encrypted with no key configured.
        let mut blob = vec![0x02u8];
        blob.extend_from_slice(&[0u8; 64]);
        rows.push(raw_row(3, blob, true, None));

        let mut out = Vec::new();
        let err = decrypt_rows_into(
            &rows,
            &config,
            &SealingKeys::none(),
            TpmAccess::Held(None),
            &mut out,
        )
        .unwrap_err();
        assert!(err.contains("software-encrypted"), "{err}");

        assert_eq!(out.len(), 2, "rows 1-2 were decrypted before row 3 failed");
        for (id, emb) in &out {
            assert!(
                emb.iter().all(|&v| v == 0.0),
                "row {id} must be zeroized when a later row fails the load"
            );
        }
    }

    /// The reproduction shape from #293: encrypted store with
    /// `bind_device_aad` and a replaced camera. Row 3's stored device id no
    /// longer matches the AAD it was sealed under, so its GCM check fails
    /// after rows 1-2 are already decrypted — and they must not sit
    /// plaintext in freed daemon heap.
    #[test]
    fn aad_mismatch_mid_loop_wipes_the_partial_set() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let sealer = facelock_tpm::SoftwareSealer::from_key_file(&key_path).unwrap();

        let config = Config::parse("[security]\nbind_device_aad = true\n").unwrap();
        let enrolled = "046d:085e:REAL";
        let aad = config.security.device_aad(Some(enrolled));

        let sealed_row = |id: u32, value: f32, device_id: &str| {
            let emb: FaceEmbedding = [value; 512];
            let blob = sealer
                .seal_embedding_with_aad(&emb, aad.as_deref())
                .unwrap();
            raw_row(id, blob, true, Some(device_id.to_string()))
        };
        // Rows 1-2 decrypt fine; row 3's recorded device id derives a
        // different AAD than it was sealed under.
        let rows = vec![
            sealed_row(1, 0.25, enrolled),
            sealed_row(2, 0.75, enrolled),
            sealed_row(3, 0.5, "ffff:ffff:OTHER"),
        ];

        let keys = SealingKeys::from_parts(Some(sealer), vec![]);
        let mut out = Vec::new();
        let err =
            decrypt_rows_into(&rows, &config, &keys, TpmAccess::Held(None), &mut out).unwrap_err();
        assert!(
            err.contains("software decryption failed for embedding 3"),
            "{err}"
        );

        assert_eq!(out.len(), 2);
        for (id, emb) in &out {
            assert!(
                emb.iter().all(|&v| v == 0.0),
                "row {id} must be zeroized on the AAD-mismatch path"
            );
        }
    }

    /// #312: enabling hard binding over an existing store. The row sealed
    /// without AAD (before the flag) still decrypts with the `None` its
    /// missing device id derives, so authentication goes on; and it is
    /// classified as unbound rather than passed off as satisfying the policy.
    #[test]
    fn legacy_unbound_row_still_authenticates_and_is_classified_unbound() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let sealer = facelock_tpm::SoftwareSealer::from_key_file(&key_path).unwrap();

        let hard = Config::parse("[security]\nbind_device_aad = true\n").unwrap();
        let bound_id = "046d:085e:";
        let bound_emb: FaceEmbedding = [0.25; 512];
        let bound_blob = sealer
            .seal_embedding_with_aad(
                &bound_emb,
                hard.security.device_aad(Some(bound_id)).as_deref(),
            )
            .unwrap();
        let legacy_emb: FaceEmbedding = [0.75; 512];
        let legacy_blob = sealer.seal_embedding_with_aad(&legacy_emb, None).unwrap();
        let rows = vec![
            raw_row(1, bound_blob, true, Some(bound_id.to_string())),
            raw_row(2, legacy_blob.clone(), true, None),
            raw_row(2, legacy_blob, true, Some(String::new())),
        ];

        let keys = SealingKeys::from_parts(Some(sealer), vec![]);
        let out = decrypt_user_embeddings(&rows, &hard, &keys, TpmAccess::Held(None))
            .expect("a legacy unbound row must never fail the load");
        assert_eq!(out.len(), 3, "every row authenticates");
        assert_eq!(out[1], (2, legacy_emb));

        assert_eq!(unbound_model_ids(&rows, &hard), vec![2]);
        // Ordinary encryption asks nothing of any row.
        let ordinary = Config::parse("").unwrap();
        assert!(unbound_model_ids(&rows, &ordinary).is_empty());
    }

    /// A row that records a device id but predates hard binding cannot open
    /// under the AAD derived now; the error says which re-enrollment fixes it.
    #[test]
    fn aad_mismatch_under_hard_binding_suggests_re_enrollment() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let sealer = facelock_tpm::SoftwareSealer::from_key_file(&key_path).unwrap();

        let emb: FaceEmbedding = [0.5; 512];
        let unbound_blob = sealer.seal_embedding_with_aad(&emb, None).unwrap();
        let rows = vec![raw_row(
            9,
            unbound_blob,
            true,
            Some("046d:085e:".to_string()),
        )];

        let hard = Config::parse("[security]\nbind_device_aad = true\n").unwrap();
        let keys = SealingKeys::from_parts(Some(sealer), vec![]);
        let err = decrypt_user_embeddings(&rows, &hard, &keys, TpmAccess::Held(None)).unwrap_err();
        assert!(
            err.contains("software decryption failed for embedding 9"),
            "{err}"
        );
        assert!(err.contains("re-enroll to bind"), "{err}");
        assert!(err.contains("security.bind_device_aad"), "{err}");
    }

    /// The other direction of #312: the flag turned off over a store sealed
    /// under it. The row cannot open without the AAD it was sealed under,
    /// and the error names the way back rather than leaving a "wrong key".
    #[test]
    fn hard_bound_row_loaded_with_the_flag_off_names_the_way_back() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let sealer = facelock_tpm::SoftwareSealer::from_key_file(&key_path).unwrap();

        let hard = Config::parse("[security]\nbind_device_aad = true\n").unwrap();
        let id = "046d:085e:";
        let emb: FaceEmbedding = [0.5; 512];
        let bound = sealer
            .seal_embedding_with_aad(&emb, hard.security.device_aad(Some(id)).as_deref())
            .unwrap();
        let rows = vec![raw_row(4, bound, true, Some(id.to_string()))];

        let off = Config::parse("").unwrap();
        let keys = SealingKeys::from_parts(Some(sealer), vec![]);
        let err = decrypt_user_embeddings(&rows, &off, &keys, TpmAccess::Held(None)).unwrap_err();
        assert!(
            err.contains("software decryption failed for embedding 4"),
            "{err}"
        );
        assert!(err.contains("re-enable it or re-enroll"), "{err}");
        assert!(err.contains("security.bind_device_aad"), "{err}");
    }

    /// A store mixing a NULL row with an id-bearing row sealed before the
    /// flag: the id-bearing row fails the load, the error names it and the
    /// re-enrollment, and the NULL row is still reported as unbound because
    /// that diagnostic runs before decryption.
    #[test]
    fn mixed_store_failure_names_the_id_row_and_still_reports_the_unbound_one() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
            type Writer = Capture;
            fn make_writer(&'a self) -> Capture {
                self.clone()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("facelock.key");
        facelock_tpm::SoftwareSealer::generate_key_file(&key_path).unwrap();
        let sealer = facelock_tpm::SoftwareSealer::from_key_file(&key_path).unwrap();

        let emb: FaceEmbedding = [0.5; 512];
        let unbound_blob = sealer.seal_embedding_with_aad(&emb, None).unwrap();
        let rows = vec![
            raw_row(1, unbound_blob.clone(), true, None),
            raw_row(2, unbound_blob, true, Some("046d:085e:".to_string())),
        ];
        let hard = Config::parse("[security]\nbind_device_aad = true\n").unwrap();
        let keys = SealingKeys::from_parts(Some(sealer), vec![]);

        let capture = Capture(Arc::new(Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        let result = tracing::subscriber::with_default(subscriber, || {
            decrypt_user_embeddings(&rows, &hard, &keys, TpmAccess::Held(None))
        });

        let err = result.expect_err("the pre-flag id row fails the load");
        assert!(
            err.contains("software decryption failed for embedding 2"),
            "{err}"
        );
        assert!(err.contains("predates security.bind_device_aad"), "{err}");
        assert_eq!(unbound_model_ids(&rows, &hard), vec![1]);
        let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("unbound (re-enroll to bind)"), "{log}");
        assert!(log.contains("models=[1]"), "{log}");
    }

    /// `needs_raw_rows` forces the slow path whenever sealed rows may exist,
    /// even with no working sealer — the fast path must never hand a sealed
    /// blob to a caller as a raw embedding.
    #[test]
    fn seal_database_forces_raw_rows_without_a_sealer() {
        let sealed = Config::parse("[tpm]\nseal_database = true\n").unwrap();
        assert!(needs_raw_rows(&sealed, false, false));

        let plain = Config::parse("").unwrap();
        assert!(!needs_raw_rows(&plain, false, false));
        assert!(needs_raw_rows(&plain, true, false));
    }

    /// A refused sealer is the third way encrypted rows can be present with
    /// nothing to decrypt them. Taking the fast path there hands a 2077-byte
    /// blob to a caller expecting 2048 raw bytes, and the store reports the
    /// database as corrupt — sending an operator whose key merely went missing
    /// to reinstall the file that still holds their enrollments.
    #[test]
    fn a_refused_sealer_forces_raw_rows() {
        let plain = Config::parse("").unwrap();
        assert!(needs_raw_rows(&plain, false, true));
    }

    // --- #354 wave 2: decrypt under the key that sealed the row ---

    /// A row naming a `key_id` that is a *secondary*, not the primary, still
    /// decrypts — the whole point of the keyring: a template sealed under a
    /// key this process no longer configures as primary keeps working as
    /// long as that key is still loadable from somewhere.
    #[test]
    fn row_naming_a_secondary_key_decrypts_under_it() {
        let primary = facelock_tpm::SoftwareSealer::from_key([0x11u8; 32]);
        let secondary = facelock_tpm::SoftwareSealer::from_key([0x22u8; 32]);
        let secondary_id = secondary.key_id();

        let emb: FaceEmbedding = [0.4; 512];
        let blob = secondary.seal_embedding(&emb).unwrap();
        let rows = vec![RawEmbeddingRow {
            model_id: 10,
            blob,
            sealed: true,
            device_id: None,
            key_id: Some(secondary_id.clone()),
        }];

        let config = Config::parse("").unwrap();
        let keys = SealingKeys::from_parts(Some(primary), vec![(secondary_id, secondary)]);
        let out = decrypt_user_embeddings(&rows, &config, &keys, TpmAccess::Held(None)).unwrap();
        assert_eq!(out, vec![(10, emb)]);
    }

    /// A row naming a key nothing loaded fails by name — not with "no key is
    /// configured" (untrue: a primary is loaded) and not by silently trying
    /// the primary anyway. The message names the artifact for the *other*
    /// encryption method, since that is where a lost cross-method key lives.
    #[test]
    fn row_naming_an_unknown_key_id_fails_naming_the_other_artifact() {
        let primary = facelock_tpm::SoftwareSealer::from_key([0x11u8; 32]);
        let elsewhere = facelock_tpm::SoftwareSealer::from_key([0x99u8; 32]);
        let unknown_id = elsewhere.key_id();

        let emb: FaceEmbedding = [0.4; 512];
        let blob = elsewhere.seal_embedding(&emb).unwrap();
        let rows = vec![RawEmbeddingRow {
            model_id: 11,
            blob,
            sealed: true,
            device_id: None,
            key_id: Some(unknown_id.clone()),
        }];

        // method = keyfile, so the "other artifact" named is sealed_key_path.
        let config = Config::parse("[encryption]\nmethod = \"keyfile\"\n").unwrap();
        let keys = SealingKeys::from_parts(Some(primary), vec![]);
        let err =
            decrypt_user_embeddings(&rows, &config, &keys, TpmAccess::Held(None)).unwrap_err();
        assert!(
            err.contains(&format!("embedding 11 was sealed under key {unknown_id}")),
            "{err}"
        );
        assert!(err.contains("not loadable"), "{err}");
        assert!(err.contains(&config.encryption.sealed_key_path), "{err}");
        assert!(err.contains("re-enroll"), "{err}");
    }

    /// A pre-V7 row (no `key_id`) sealed under a loaded secondary decrypts
    /// through the trial, and the win is logged naming the model and key —
    /// a store mid-migration (some rows tagged, some not) must not regress a
    /// row that still opens under a key this process kept around.
    #[test]
    fn legacy_row_sealed_under_a_secondary_decrypts_through_the_trial() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
            type Writer = Capture;
            fn make_writer(&'a self) -> Capture {
                self.clone()
            }
        }

        let primary = facelock_tpm::SoftwareSealer::from_key([0x11u8; 32]);
        let secondary = facelock_tpm::SoftwareSealer::from_key([0x22u8; 32]);
        let secondary_id = secondary.key_id();

        let emb: FaceEmbedding = [0.6; 512];
        let blob = secondary.seal_embedding(&emb).unwrap();
        let rows = vec![raw_row(12, blob, true, None)];

        let config = Config::parse("").unwrap();
        let keys = SealingKeys::from_parts(Some(primary), vec![(secondary_id.clone(), secondary)]);

        let capture = Capture(Arc::new(Mutex::new(Vec::new())));
        // The fallback logs at `debug` (it runs per row per authentication).
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let out = tracing::subscriber::with_default(subscriber, || {
            decrypt_user_embeddings(&rows, &config, &keys, TpmAccess::Held(None))
        })
        .unwrap();
        assert_eq!(out, vec![(12, emb)]);

        let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("secondary key"), "{log}");
        assert!(log.contains(&secondary_id), "{log}");
    }

    /// A legacy row sealed under a key that is neither the primary nor any
    /// loaded secondary fails with the trial's own clause, distinct from "no
    /// key is configured" (untrue: keys ARE loaded, just not this row's).
    #[test]
    fn legacy_row_sealed_under_an_unloaded_third_key_fails_with_the_trial_clause() {
        let primary = facelock_tpm::SoftwareSealer::from_key([0x11u8; 32]);
        let secondary = facelock_tpm::SoftwareSealer::from_key([0x22u8; 32]);
        let elsewhere = facelock_tpm::SoftwareSealer::from_key([0x33u8; 32]);

        let emb: FaceEmbedding = [0.7; 512];
        let blob = elsewhere.seal_embedding(&emb).unwrap();
        let rows = vec![raw_row(13, blob, true, None)];

        let config = Config::parse("").unwrap();
        let keys = SealingKeys::from_parts(Some(primary), vec![(secondary.key_id(), secondary)]);
        let err =
            decrypt_user_embeddings(&rows, &config, &keys, TpmAccess::Held(None)).unwrap_err();
        assert!(
            err.contains("software decryption failed for embedding 13"),
            "{err}"
        );
        assert!(
            err.contains("sealed under a key that is no longer loadable"),
            "{err}"
        );
    }

    /// Naming a `key_id` is not a bypass of hard device binding: the same AAD
    /// derivation applies whether or not the row names its key, and a forged
    /// device id still fails to open a named-key row.
    #[test]
    fn named_key_row_still_enforces_hard_binding_aad() {
        let primary = facelock_tpm::SoftwareSealer::from_key([0x11u8; 32]);
        let primary_id = primary.key_id();

        let hard = Config::parse("[security]\nbind_device_aad = true\n").unwrap();
        let device_id = "046d:085e:REAL";
        let aad = hard.security.device_aad(Some(device_id));
        let emb: FaceEmbedding = [0.3; 512];
        let blob = primary
            .seal_embedding_with_aad(&emb, aad.as_deref())
            .unwrap();
        let keys = SealingKeys::from_parts(Some(primary), vec![]);

        let good_row = RawEmbeddingRow {
            model_id: 20,
            blob: blob.clone(),
            sealed: true,
            device_id: Some(device_id.to_string()),
            key_id: Some(primary_id.clone()),
        };
        let out =
            decrypt_user_embeddings(&[good_row], &hard, &keys, TpmAccess::Held(None)).unwrap();
        assert_eq!(out, vec![(20, emb)]);

        // A forged device id derives a different AAD, so the same named key
        // still fails to open it: naming the key must not bypass binding.
        let forged_row = RawEmbeddingRow {
            model_id: 21,
            blob,
            sealed: true,
            device_id: Some("ffff:ffff:forged".to_string()),
            key_id: Some(primary_id),
        };
        let err = decrypt_user_embeddings(&[forged_row], &hard, &keys, TpmAccess::Held(None))
            .unwrap_err();
        assert!(
            err.contains("software decryption failed for embedding 21"),
            "{err}"
        );
    }
}
