use std::path::Path;

use facelock_core::error::{FacelockError, Result};
use facelock_core::types::FaceEmbedding;
#[cfg(not(feature = "tpm"))]
use tracing::warn;
#[cfg(feature = "tpm")]
use tracing::{debug, info};

/// Version byte prefixed to TPM-sealed blobs with no PCR policy (unseal needs
/// only the object's userWithAuth, replayed via a null-auth session).
const SEALED_VERSION_BYTE: u8 = 0x01;

/// Version byte prefixed to software-encrypted blobs (AES-256-GCM).
const SOFTWARE_ENCRYPTED_VERSION_BYTE: u8 = 0x02;

/// Version byte prefixed to TPM-sealed blobs bound to a PCR policy. The blob is
/// self-describing: it records the PCR index list so unseal can rebuild and
/// replay the exact `PolicyPCR` selection, and the object is created with
/// `userWithAuth = false` so unseal MUST satisfy that policy (finding #5).
///
/// Layout: `0x03 | pcr_count(u8) | pcr_index(u32 LE) * pcr_count | pub_len(u32 LE) | pub | priv`
const SEALED_PCR_VERSION_BYTE: u8 = 0x03;

/// AES-256-GCM nonce size in bytes.
const AES_NONCE_SIZE: usize = 12;

/// AES-256-GCM key size in bytes (256 bits).
const AES_KEY_SIZE: usize = 32;

/// Raw embedding size: 512 f32 values = 2048 bytes.
const RAW_EMBEDDING_SIZE: usize = 512 * 4;

/// TPM-based embedding sealer.
///
/// With the `tpm` feature enabled, this performs real TPM 2.0 seal/unseal operations
/// using an ECC P-256 primary key under the storage hierarchy.
///
/// Without the `tpm` feature, this operates in passthrough mode where embeddings
/// are stored and returned as-is with no encryption.
#[cfg(feature = "tpm")]
pub struct TpmSealer {
    context: tss_esapi::Context,
    primary_key: tss_esapi::handles::KeyHandle,
}

#[cfg(not(feature = "tpm"))]
pub struct TpmSealer {
    #[allow(dead_code)]
    tcti: String,
}

// ---------------------------------------------------------------------------
// Real TPM implementation
// ---------------------------------------------------------------------------
#[cfg(feature = "tpm")]
impl TpmSealer {
    /// Create a new TpmSealer connected to a real TPM via the given TCTI string.
    ///
    /// The TCTI string is typically `"device:/dev/tpmrm0"` for the kernel resource
    /// manager, or `"swtpm:host=localhost,port=2321"` for a software TPM.
    pub fn new(tcti: &str) -> Result<Self> {
        use tss_esapi::tcti_ldr::TctiNameConf;

        let tcti_conf: TctiNameConf = tcti
            .parse()
            .map_err(|e| FacelockError::Tpm(format!("invalid TCTI string '{tcti}': {e}")))?;

        let mut context = tss_esapi::Context::new(tcti_conf)
            .map_err(|e| FacelockError::Tpm(format!("failed to create TPM context: {e}")))?;

        let primary_key = Self::create_primary(&mut context)?;
        info!("TPM sealer initialized via {tcti}");

        Ok(Self {
            context,
            primary_key,
        })
    }

    /// Whether a real TPM is available (always true when constructed successfully).
    pub fn is_available(&self) -> bool {
        true
    }

    /// Seal an embedding using TPM.
    ///
    /// The sealed blob is prefixed with a version byte (0x01) followed by the
    /// serialized TPM2B_PUBLIC and TPM2B_PRIVATE structures.
    pub fn seal_embedding(
        &mut self,
        embedding: &FaceEmbedding,
        pcr_indices: Option<&[u32]>,
    ) -> Result<Vec<u8>> {
        let raw = embedding_to_bytes(embedding);
        self.seal_bytes(&raw, pcr_indices)
    }

    /// Unseal an embedding from a sealed blob.
    ///
    /// Handles format detection:
    /// - Blob starting with 0x01: TPM-sealed, unseal via TPM
    /// - Blob of exactly 2048 bytes with no 0x01 prefix: raw passthrough (migration compat)
    pub fn unseal_embedding(&mut self, sealed: &[u8]) -> Result<FaceEmbedding> {
        let raw = self.unseal_or_passthrough(sealed)?;
        bytes_to_embedding(&raw)
    }

    /// Seal arbitrary bytes. Returns version-prefixed sealed blob.
    pub fn seal_bytes(&mut self, data: &[u8], pcr_indices: Option<&[u32]>) -> Result<Vec<u8>> {
        use tss_esapi::{
            attributes::ObjectAttributesBuilder,
            interface_types::algorithm::HashingAlgorithm,
            structures::{PublicBuilder, SensitiveData},
        };

        let sensitive_data = SensitiveData::try_from(data.to_vec())
            .map_err(|e| FacelockError::Tpm(format!("data too large for TPM seal: {e}")))?;

        // Build a sealed object (keyedhash with no sign/decrypt).
        //
        // Finding #5: for a PCR-bound object we set userWithAuth = FALSE so the
        // only way to satisfy USER-role auth (required by TPM2_Unseal) is to run
        // the authPolicy — i.e. replay PolicyPCR over the same selection. With
        // userWithAuth = true (the old behaviour) the empty object auth value
        // alone satisfied unseal and the PCR policy was never enforced.
        let mut obj_attrs = ObjectAttributesBuilder::new()
            .with_fixed_tpm(true)
            .with_fixed_parent(true);

        if pcr_indices.is_none() {
            // No policy: empty-auth object, exempt from dictionary-attack lockout.
            obj_attrs = obj_attrs.with_user_with_auth(true).with_no_da(true);
        } else {
            // Policy-bound: force USER-role auth through the policy session.
            obj_attrs = obj_attrs.with_user_with_auth(false);
        }

        let obj_attrs = obj_attrs
            .build()
            .map_err(|e| FacelockError::Tpm(format!("failed to build object attributes: {e}")))?;

        let mut pub_builder = PublicBuilder::new()
            .with_public_algorithm(
                tss_esapi::interface_types::algorithm::PublicAlgorithm::KeyedHash,
            )
            .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
            .with_object_attributes(obj_attrs)
            .with_keyed_hash_parameters(tss_esapi::structures::PublicKeyedHashParameters::new(
                tss_esapi::structures::KeyedHashScheme::Null,
            ))
            .with_keyed_hash_unique_identifier(Default::default());

        // Add PCR policy if requested
        if let Some(indices) = pcr_indices {
            let pcr_policy = self.build_pcr_policy_digest(indices)?;
            pub_builder = pub_builder.with_auth_policy(pcr_policy);
        }

        let public = pub_builder.build().map_err(|e| {
            FacelockError::Tpm(format!("failed to build sealed object public: {e}"))
        })?;

        let (private, public_out) = self
            .context
            .execute_with_nullauth_session(|ctx| {
                ctx.create(
                    self.primary_key,
                    public,
                    None,
                    Some(sensitive_data),
                    None,
                    None,
                )
            })
            .map_err(|e| FacelockError::Tpm(format!("TPM seal failed: {e}")))
            .map(|result| (result.out_private, result.out_public))?;

        // Serialize. The no-PCR format stays byte-compatible with existing 0x01
        // blobs; PCR-bound objects use 0x03 and additionally record the index
        // list so unseal can rebuild the selection without external config.
        let pub_bytes = serialize_public(&public_out)?;
        let priv_bytes = serialize_private(&private)?;

        let mut blob = Vec::with_capacity(8 + pub_bytes.len() + priv_bytes.len());
        match pcr_indices {
            None => {
                blob.push(SEALED_VERSION_BYTE);
            }
            Some(indices) => {
                blob.push(SEALED_PCR_VERSION_BYTE);
                blob.push(indices.len() as u8);
                for &idx in indices {
                    blob.extend_from_slice(&idx.to_le_bytes());
                }
            }
        }
        blob.extend_from_slice(&(pub_bytes.len() as u32).to_le_bytes());
        blob.extend_from_slice(&pub_bytes);
        blob.extend_from_slice(&priv_bytes);

        debug!(
            sealed_size = blob.len(),
            pcr_bound = pcr_indices.is_some(),
            "sealed data"
        );
        Ok(blob)
    }

    /// Unseal bytes, handling format detection.
    fn unseal_or_passthrough(&mut self, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.is_empty() {
            return Err(FacelockError::Tpm("empty sealed blob".into()));
        }

        // Format detection: version byte 0x01 (no PCR) or 0x03 (PCR-bound) = TPM-sealed
        if (sealed[0] == SEALED_VERSION_BYTE || sealed[0] == SEALED_PCR_VERSION_BYTE)
            && sealed.len() > 5
        {
            return self.unseal_bytes(sealed);
        }

        // Exactly 2048 bytes with no version prefix = raw passthrough (migration compat)
        if sealed.len() == RAW_EMBEDDING_SIZE {
            debug!("detected raw (unsealed) embedding, passing through");
            return Ok(sealed.to_vec());
        }

        Err(FacelockError::Tpm(format!(
            "unrecognized sealed blob format: size={}, first_byte=0x{:02x}",
            sealed.len(),
            sealed[0]
        )))
    }

    /// Unseal a version-prefixed TPM blob (0x01 no-PCR, or 0x03 PCR-bound).
    ///
    /// For a PCR-bound blob this starts a *real* (non-trial) policy session,
    /// replays `PolicyPCR` over the recorded selection against the CURRENT PCR
    /// values, and unseals under that session. If any bound PCR has changed
    /// since sealing, the session's policy digest no longer matches the object's
    /// authPolicy and `TPM2_Unseal` fails — the enforcement finding #5 requires.
    pub fn unseal_bytes(&mut self, sealed: &[u8]) -> Result<Vec<u8>> {
        let (pcr_indices, pub_bytes, priv_bytes) = parse_sealed_blob(sealed)?;

        let public = deserialize_public(pub_bytes)?;
        let private = deserialize_private(priv_bytes)?;

        let loaded = self
            .context
            .execute_with_nullauth_session(|ctx| ctx.load(self.primary_key, private, public))
            .map_err(|e| FacelockError::Tpm(format!("TPM load failed: {e}")))?;
        let object: tss_esapi::handles::ObjectHandle = loaded.into();

        let unseal_result = match pcr_indices {
            None => self
                .context
                .execute_with_nullauth_session(|ctx| ctx.unseal(object))
                .map_err(|e| FacelockError::Tpm(format!("TPM unseal failed: {e}"))),
            Some(ref indices) => self.unseal_with_pcr_policy(object, indices),
        };

        // Always flush the transiently-loaded object.
        let _ = self.context.flush_context(object);

        let unsealed = unseal_result?;
        let data: Vec<u8> = unsealed.as_slice().to_vec();
        debug!(
            unsealed_size = data.len(),
            pcr_bound = pcr_indices.is_some(),
            "unsealed data"
        );
        Ok(data)
    }

    /// Unseal an object whose USER-role auth is gated by a PCR policy.
    ///
    /// Starts a real policy session and replays `PolicyPCR` against the current
    /// PCR state. Unlike the trial session used at seal time, a real session's
    /// policy digest is what the TPM checks against the object's authPolicy at
    /// `TPM2_Unseal`, so a changed PCR makes the unseal fail rather than silently
    /// succeed.
    fn unseal_with_pcr_policy(
        &mut self,
        object: tss_esapi::handles::ObjectHandle,
        indices: &[u32],
    ) -> Result<tss_esapi::structures::SensitiveData> {
        use tss_esapi::{
            attributes::SessionAttributesBuilder,
            constants::SessionType,
            handles::SessionHandle,
            interface_types::{algorithm::HashingAlgorithm, session_handles::PolicySession},
            structures::SymmetricDefinition,
        };

        let (pcr_digest, pcr_selection_list) = self.current_pcr_digest(indices)?;

        let session = self
            .context
            .start_auth_session(
                None,
                None,
                None,
                SessionType::Policy,
                SymmetricDefinition::AES_256_CFB,
                HashingAlgorithm::Sha256,
            )
            .map_err(|e| FacelockError::Tpm(format!("failed to start policy session: {e}")))?
            .ok_or_else(|| FacelockError::Tpm("policy session returned None".into()))?;

        let (attrs, mask) = SessionAttributesBuilder::new()
            .with_decrypt(true)
            .with_encrypt(true)
            .build();

        let cleanup = |ctx: &mut tss_esapi::Context| {
            let _ = ctx.flush_context(SessionHandle::from(session).into());
        };

        if let Err(e) = self.context.tr_sess_set_attributes(session, attrs, mask) {
            cleanup(&mut self.context);
            return Err(FacelockError::Tpm(format!(
                "failed to set policy session attributes: {e}"
            )));
        }

        let policy_session = match PolicySession::try_from(session) {
            Ok(s) => s,
            Err(e) => {
                cleanup(&mut self.context);
                return Err(FacelockError::Tpm(format!(
                    "failed to convert to policy session: {e}"
                )));
            }
        };

        if let Err(e) = self
            .context
            .policy_pcr(policy_session, pcr_digest, pcr_selection_list)
        {
            cleanup(&mut self.context);
            return Err(FacelockError::Tpm(format!(
                "policy_pcr replay failed during unseal: {e}"
            )));
        }

        let result = self
            .context
            .execute_with_session(Some(session), |ctx| ctx.unseal(object))
            .map_err(|e| {
                FacelockError::Tpm(format!("TPM unseal failed (PCR policy not satisfied): {e}"))
            });

        cleanup(&mut self.context);
        result
    }

    /// Create an ECC P-256 primary key under the storage hierarchy.
    fn create_primary(context: &mut tss_esapi::Context) -> Result<tss_esapi::handles::KeyHandle> {
        use tss_esapi::{
            attributes::ObjectAttributesBuilder,
            interface_types::{
                algorithm::{HashingAlgorithm, PublicAlgorithm},
                ecc::EccCurve,
                reserved_handles::Hierarchy,
            },
            structures::{
                EccPoint, EccScheme, KeyDerivationFunctionScheme, PublicBuilder,
                PublicEccParametersBuilder, SymmetricDefinitionObject,
            },
        };

        let obj_attrs = ObjectAttributesBuilder::new()
            .with_fixed_tpm(true)
            .with_fixed_parent(true)
            .with_sensitive_data_origin(true)
            .with_user_with_auth(true)
            .with_restricted(true)
            .with_decrypt(true)
            .with_no_da(true)
            .build()
            .map_err(|e| FacelockError::Tpm(format!("failed to build primary attributes: {e}")))?;

        let ecc_params = PublicEccParametersBuilder::new()
            .with_ecc_scheme(EccScheme::Null)
            .with_curve(EccCurve::NistP256)
            .with_is_signing_key(false)
            .with_is_decryption_key(true)
            .with_restricted(true)
            .with_key_derivation_function_scheme(KeyDerivationFunctionScheme::Null)
            .with_symmetric(SymmetricDefinitionObject::AES_128_CFB)
            .build()
            .map_err(|e| FacelockError::Tpm(format!("failed to build ECC params: {e}")))?;

        let public = PublicBuilder::new()
            .with_public_algorithm(PublicAlgorithm::Ecc)
            .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
            .with_object_attributes(obj_attrs)
            .with_ecc_parameters(ecc_params)
            .with_ecc_unique_identifier(EccPoint::default())
            .build()
            .map_err(|e| FacelockError::Tpm(format!("failed to build primary public: {e}")))?;

        let primary = context
            .execute_with_nullauth_session(|ctx| {
                ctx.create_primary(Hierarchy::Owner, public, None, None, None, None)
            })
            .map_err(|e| FacelockError::Tpm(format!("failed to create primary key: {e}")))?;

        Ok(primary.key_handle)
    }

    /// Seal a 32-byte AES key to a file using TPM.
    ///
    /// The sealed blob is written to a file created at mode 0600 in a single
    /// `open(2)` (no create-then-`chmod` window, finding #11) and flushed with
    /// `sync_all`. An existing file at `path` is truncated in place, so this is
    /// safe to use for re-sealing (`facelock tpm reseal`).
    pub fn seal_key_to_file(
        &mut self,
        key: &[u8; 32],
        path: &Path,
        pcr_indices: Option<&[u32]>,
    ) -> Result<()> {
        use std::io::Write;

        let sealed = self.seal_bytes(key.as_slice(), pcr_indices)?;

        let mut file =
            facelock_core::fs_security::create_truncate_file(path, 0o600).map_err(|e| {
                FacelockError::Tpm(format!(
                    "failed to create sealed key file {}: {e}",
                    path.display()
                ))
            })?;
        file.write_all(&sealed)
            .and_then(|()| file.sync_all())
            .map_err(|e| {
                FacelockError::Tpm(format!(
                    "failed to write sealed key to {}: {e}",
                    path.display()
                ))
            })?;

        info!("sealed AES key to {}", path.display());
        Ok(())
    }

    /// Unseal a 32-byte AES key from a TPM-sealed file.
    pub fn unseal_key_from_file(&mut self, path: &Path) -> Result<[u8; 32]> {
        let blob = std::fs::read(path).map_err(|e| {
            FacelockError::Tpm(format!(
                "failed to read sealed key from {}: {e}",
                path.display()
            ))
        })?;

        let unsealed = self.unseal_bytes(&blob)?;
        if unsealed.len() != 32 {
            return Err(FacelockError::Tpm(format!(
                "unsealed key is {} bytes, expected 32",
                unsealed.len()
            )));
        }

        let mut key = [0u8; 32];
        key.copy_from_slice(&unsealed);
        debug!("unsealed AES key from {}", path.display());
        Ok(key)
    }

    /// Read the current PCR values for `indices` and return
    /// `(aggregated_digest, selection_list)`, where `aggregated_digest` is the
    /// SHA-256 of the concatenated PCR digests as `TPM2_PolicyPCR` expects.
    ///
    /// Shared by seal (trial session) and unseal (real session) so both derive
    /// the policy from an identical selection and digest formation.
    fn current_pcr_digest(
        &mut self,
        indices: &[u32],
    ) -> Result<(
        tss_esapi::structures::Digest,
        tss_esapi::structures::PcrSelectionList,
    )> {
        use tss_esapi::{
            interface_types::{algorithm::HashingAlgorithm, reserved_handles::Hierarchy},
            structures::{MaxBuffer, PcrSelectionListBuilder},
        };

        let slots: Vec<tss_esapi::structures::PcrSlot> = indices
            .iter()
            .map(|&i| crate::pcr::pcr_index_to_slot(i))
            .collect::<Result<Vec<_>>>()?;

        let pcr_selection_list = PcrSelectionListBuilder::new()
            .with_selection(HashingAlgorithm::Sha256, &slots)
            .build()
            .map_err(|e| FacelockError::Tpm(format!("failed to build PCR selection list: {e}")))?;

        let (_update_counter, _pcr_selection_out, pcr_digests) = self
            .context
            .execute_without_session(|ctx| ctx.pcr_read(pcr_selection_list.clone()))
            .map_err(|e| FacelockError::Tpm(format!("PCR read failed: {e}")))?;

        let concatenated: Vec<u8> = pcr_digests
            .value()
            .iter()
            .flat_map(|d| d.as_bytes())
            .copied()
            .collect();

        let (hashed_pcr_values, _ticket) = self
            .context
            .execute_without_session(|ctx| {
                ctx.hash(
                    MaxBuffer::try_from(concatenated).map_err(|_| {
                        tss_esapi::Error::WrapperError(
                            tss_esapi::error::WrapperErrorKind::WrongParamSize,
                        )
                    })?,
                    HashingAlgorithm::Sha256,
                    Hierarchy::Owner,
                )
            })
            .map_err(|e| {
                FacelockError::Tpm(format!("failed to hash concatenated PCR values: {e}"))
            })?;

        Ok((hashed_pcr_values, pcr_selection_list))
    }

    /// Build a PCR policy digest for the given PCR indices (SHA-256).
    ///
    /// Creates a trial policy session on the TPM, extends it with a PolicyPCR
    /// command for the specified indices bound to the current PCR values, then
    /// retrieves the accumulated policy digest. This digest is used as the
    /// `authPolicy` on sealed objects so that unsealing requires the same PCR
    /// state.
    fn build_pcr_policy_digest(
        &mut self,
        indices: &[u32],
    ) -> Result<tss_esapi::structures::Digest> {
        use tss_esapi::{
            attributes::SessionAttributesBuilder,
            constants::SessionType,
            interface_types::{algorithm::HashingAlgorithm, session_handles::PolicySession},
            structures::SymmetricDefinition,
        };

        // Read the current PCR state and its aggregated digest. The same helper
        // is replayed on a real session at unseal time, so seal and unseal agree
        // on exactly how the selection and digest are formed.
        let (hashed_pcr_values, pcr_selection_list) = self.current_pcr_digest(indices)?;

        // Start a trial policy session
        let trial_session = self
            .context
            .start_auth_session(
                None,
                None,
                None,
                SessionType::Trial,
                SymmetricDefinition::AES_256_CFB,
                HashingAlgorithm::Sha256,
            )
            .map_err(|e| FacelockError::Tpm(format!("failed to start trial policy session: {e}")))?
            .ok_or_else(|| FacelockError::Tpm("trial policy session returned None".into()))?;

        // Set session attributes for encrypt/decrypt
        let (attrs, mask) = SessionAttributesBuilder::new()
            .with_decrypt(true)
            .with_encrypt(true)
            .build();

        self.context
            .tr_sess_set_attributes(trial_session, attrs, mask)
            .map_err(|e| {
                // Clean up session before returning error
                let _ = self
                    .context
                    .flush_context(tss_esapi::handles::SessionHandle::from(trial_session).into());
                FacelockError::Tpm(format!("failed to set trial session attributes: {e}"))
            })?;

        let policy_session = PolicySession::try_from(trial_session).map_err(|e| {
            let _ = self
                .context
                .flush_context(tss_esapi::handles::SessionHandle::from(trial_session).into());
            FacelockError::Tpm(format!(
                "failed to convert auth session to policy session: {e}"
            ))
        })?;

        // Extend the trial policy with PolicyPCR using current PCR values
        self.context
            .policy_pcr(policy_session, hashed_pcr_values, pcr_selection_list)
            .map_err(|e| {
                let _ = self
                    .context
                    .flush_context(tss_esapi::handles::SessionHandle::from(trial_session).into());
                FacelockError::Tpm(format!("policy_pcr failed: {e}"))
            })?;

        // Retrieve the policy digest
        let digest = self
            .context
            .policy_get_digest(policy_session)
            .map_err(|e| {
                let _ = self
                    .context
                    .flush_context(tss_esapi::handles::SessionHandle::from(trial_session).into());
                FacelockError::Tpm(format!("policy_get_digest failed: {e}"))
            })?;

        // Clean up the trial session
        self.context
            .flush_context(tss_esapi::handles::SessionHandle::from(trial_session).into())
            .map_err(|e| FacelockError::Tpm(format!("failed to flush trial session: {e}")))?;

        debug!(
            digest_len = digest.len(),
            pcr_count = indices.len(),
            "computed PCR policy digest via trial session"
        );

        Ok(digest)
    }
}

// ---------------------------------------------------------------------------
// Serialization helpers (TPM feature only)
// ---------------------------------------------------------------------------

/// Parse a version-prefixed sealed blob into `(pcr_indices, pub_bytes, priv_bytes)`.
///
/// - `0x01`: no PCR policy — `pcr_indices` is `None`.
/// - `0x03`: PCR-bound — `pcr_indices` carries the recorded selection so unseal
///   can rebuild and replay the exact `PolicyPCR`.
#[cfg(feature = "tpm")]
#[allow(clippy::type_complexity)]
fn parse_sealed_blob(sealed: &[u8]) -> Result<(Option<Vec<u32>>, &[u8], &[u8])> {
    if sealed.len() < 5 {
        return Err(FacelockError::Tpm("sealed blob too short".into()));
    }

    match sealed[0] {
        SEALED_VERSION_BYTE => {
            let pub_len = u32::from_le_bytes([sealed[1], sealed[2], sealed[3], sealed[4]]) as usize;
            let body = &sealed[5..];
            if body.len() < pub_len {
                return Err(FacelockError::Tpm("sealed blob truncated (public)".into()));
            }
            Ok((None, &body[..pub_len], &body[pub_len..]))
        }
        SEALED_PCR_VERSION_BYTE => {
            // 0x03 | pcr_count(u8) | index(u32 LE)*count | pub_len(u32 LE) | pub | priv
            let count = sealed[1] as usize;
            let idx_start = 2;
            let idx_end = idx_start + count * 4;
            let len_end = idx_end + 4;
            if sealed.len() < len_end {
                return Err(FacelockError::Tpm(
                    "PCR-sealed blob truncated (header)".into(),
                ));
            }
            let indices: Vec<u32> = (0..count)
                .map(|i| {
                    let o = idx_start + i * 4;
                    u32::from_le_bytes([sealed[o], sealed[o + 1], sealed[o + 2], sealed[o + 3]])
                })
                .collect();
            let pub_len = u32::from_le_bytes([
                sealed[idx_end],
                sealed[idx_end + 1],
                sealed[idx_end + 2],
                sealed[idx_end + 3],
            ]) as usize;
            let body = &sealed[len_end..];
            if body.len() < pub_len {
                return Err(FacelockError::Tpm(
                    "PCR-sealed blob truncated (public)".into(),
                ));
            }
            Ok((Some(indices), &body[..pub_len], &body[pub_len..]))
        }
        other => Err(FacelockError::Tpm(format!(
            "unrecognized sealed blob version byte: 0x{other:02x}"
        ))),
    }
}

#[cfg(feature = "tpm")]
fn serialize_public(public: &tss_esapi::structures::Public) -> Result<Vec<u8>> {
    use tss_esapi::traits::Marshall;
    public
        .marshall()
        .map_err(|e| FacelockError::Tpm(format!("failed to serialize TPM public: {e}")))
}

#[cfg(feature = "tpm")]
fn deserialize_public(bytes: &[u8]) -> Result<tss_esapi::structures::Public> {
    use tss_esapi::traits::UnMarshall;
    tss_esapi::structures::Public::unmarshall(bytes)
        .map_err(|e| FacelockError::Tpm(format!("failed to deserialize TPM public: {e}")))
}

#[cfg(feature = "tpm")]
fn serialize_private(private: &tss_esapi::structures::Private) -> Result<Vec<u8>> {
    use tss_esapi::traits::Marshall;
    private
        .marshall()
        .map_err(|e| FacelockError::Tpm(format!("failed to serialize TPM private: {e}")))
}

#[cfg(feature = "tpm")]
fn deserialize_private(bytes: &[u8]) -> Result<tss_esapi::structures::Private> {
    use tss_esapi::traits::UnMarshall;
    tss_esapi::structures::Private::unmarshall(bytes)
        .map_err(|e| FacelockError::Tpm(format!("failed to deserialize TPM private: {e}")))
}

// ---------------------------------------------------------------------------
// Passthrough (no-tpm) implementation
// ---------------------------------------------------------------------------
#[cfg(not(feature = "tpm"))]
impl TpmSealer {
    /// Create a new TpmSealer in passthrough mode.
    /// Always succeeds but TPM operations are not available.
    pub fn new(tcti: &str) -> Result<Self> {
        warn!("TPM support not compiled in (missing 'tpm' feature), operating in passthrough mode");
        Ok(Self {
            tcti: tcti.to_string(),
        })
    }

    /// Whether a real TPM is available (always false in passthrough mode).
    pub fn is_available(&self) -> bool {
        false
    }

    /// In passthrough mode, returns raw embedding bytes.
    pub fn seal_embedding(
        &mut self,
        embedding: &FaceEmbedding,
        _pcr_indices: Option<&[u32]>,
    ) -> Result<Vec<u8>> {
        Ok(embedding_to_bytes(embedding))
    }

    /// In passthrough mode, interprets bytes directly as an embedding.
    pub fn unseal_embedding(&mut self, sealed: &[u8]) -> Result<FaceEmbedding> {
        // Handle format detection: version byte 0x01/0x03 = TPM-sealed (cannot unseal without TPM)
        if !sealed.is_empty()
            && (sealed[0] == SEALED_VERSION_BYTE || sealed[0] == SEALED_PCR_VERSION_BYTE)
            && sealed.len() != RAW_EMBEDDING_SIZE
        {
            return Err(FacelockError::Tpm(
                "cannot unseal TPM-sealed embedding without TPM support (compile with 'tpm' feature)".into(),
            ));
        }

        if sealed.len() != RAW_EMBEDDING_SIZE {
            return Err(FacelockError::Storage(format!(
                "invalid embedding size: expected {RAW_EMBEDDING_SIZE}, got {}",
                sealed.len()
            )));
        }
        bytes_to_embedding(sealed)
    }

    /// Seal arbitrary bytes. In passthrough mode, returns bytes as-is.
    pub fn seal_bytes(&mut self, data: &[u8], _pcr_indices: Option<&[u32]>) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    /// Unseal a version-prefixed TPM blob. Not available without TPM feature.
    pub fn unseal_bytes(&mut self, _sealed: &[u8]) -> Result<Vec<u8>> {
        Err(FacelockError::Tpm(
            "cannot unseal TPM-sealed data without TPM support (compile with 'tpm' feature)".into(),
        ))
    }

    /// Seal a key to file. Not available without TPM feature.
    pub fn seal_key_to_file(
        &mut self,
        _key: &[u8; 32],
        _path: &Path,
        _pcr_indices: Option<&[u32]>,
    ) -> Result<()> {
        Err(FacelockError::Tpm(
            "TPM support not compiled in (missing 'tpm' feature)".into(),
        ))
    }

    /// Unseal a key from file. Not available without TPM feature.
    pub fn unseal_key_from_file(&mut self, _path: &Path) -> Result<[u8; 32]> {
        Err(FacelockError::Tpm(
            "TPM support not compiled in (missing 'tpm' feature)".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Software encryption (AES-256-GCM, non-TPM fallback)
// ---------------------------------------------------------------------------

/// AES-256-GCM based sealer for environments without a TPM.
///
/// Encrypts embeddings using a 256-bit key stored in a key file.
/// Sealed format: `0x02 | 12-byte nonce | ciphertext | 16-byte auth tag`
pub struct SoftwareSealer {
    key: [u8; AES_KEY_SIZE],
}

impl std::fmt::Debug for SoftwareSealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoftwareSealer")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl SoftwareSealer {
    /// Create a new SoftwareSealer from a key file.
    ///
    /// The key file must contain exactly 32 bytes (256 bits) of key material.
    pub fn from_key_file(path: &std::path::Path) -> Result<Self> {
        let data = std::fs::read(path).map_err(|e| {
            FacelockError::Encryption(format!("failed to read key file {}: {e}", path.display()))
        })?;
        if data.len() != AES_KEY_SIZE {
            return Err(FacelockError::Encryption(format!(
                "key file must be exactly {AES_KEY_SIZE} bytes, got {}",
                data.len()
            )));
        }
        let mut key = [0u8; AES_KEY_SIZE];
        key.copy_from_slice(&data);
        Ok(Self { key })
    }

    /// Create a SoftwareSealer from raw key bytes.
    pub fn from_key(key: [u8; AES_KEY_SIZE]) -> Self {
        Self { key }
    }

    /// Generate a new random 256-bit key and write it to a file at 0600.
    ///
    /// The file is created at mode 0600 in the same `open(2)` that creates it
    /// (via `fs_security::create_truncate_file`), so it is never momentarily
    /// world/group-readable — closing the create-then-`chmod` TOCTOU window
    /// (finding #11). The key material is flushed to disk with `sync_all` and
    /// zeroized from memory before returning.
    pub fn generate_key_file(path: &std::path::Path) -> Result<()> {
        use rand::Rng;
        use std::io::Write;
        use zeroize::Zeroize;

        let mut key = [0u8; AES_KEY_SIZE];
        rand::rng().fill_bytes(&mut key);

        let write_result = (|| -> Result<()> {
            let mut file =
                facelock_core::fs_security::create_truncate_file(path, 0o600).map_err(|e| {
                    FacelockError::Encryption(format!(
                        "failed to create key file {}: {e}",
                        path.display()
                    ))
                })?;
            file.write_all(&key)
                .and_then(|()| file.sync_all())
                .map_err(|e| {
                    FacelockError::Encryption(format!(
                        "failed to write key file {}: {e}",
                        path.display()
                    ))
                })
        })();

        key.zeroize();
        write_result
    }

    /// Encrypt an embedding using AES-256-GCM.
    ///
    /// Returns: `0x02 | 12-byte nonce | ciphertext | 16-byte tag`
    pub fn seal_embedding(&self, embedding: &FaceEmbedding) -> Result<Vec<u8>> {
        let raw = embedding_to_bytes(embedding);
        self.seal_bytes(&raw)
    }

    /// Decrypt an embedding from a software-encrypted blob.
    pub fn unseal_embedding(&self, sealed: &[u8]) -> Result<FaceEmbedding> {
        let raw = self.unseal_bytes(sealed)?;
        bytes_to_embedding(&raw)
    }

    /// Encrypt an embedding, binding it to `aad` (Additional Authenticated Data).
    ///
    /// When `aad` is `Some`, the ciphertext can only be decrypted by supplying
    /// the exact same AAD. Used for opt-in *hard* device binding (Plan 04): the
    /// caller passes `types::device_binding_aad(device_id)` so a template sealed
    /// under one camera cannot be decrypted under a different one. The AAD is NOT
    /// stored in the blob — it is re-derived at decrypt time.
    pub fn seal_embedding_with_aad(
        &self,
        embedding: &FaceEmbedding,
        aad: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let raw = embedding_to_bytes(embedding);
        self.seal_bytes_with_aad(&raw, aad)
    }

    /// Decrypt an embedding sealed with [`Self::seal_embedding_with_aad`].
    /// Decryption fails if `aad` differs from the value used at seal time.
    pub fn unseal_embedding_with_aad(
        &self,
        sealed: &[u8],
        aad: Option<&[u8]>,
    ) -> Result<FaceEmbedding> {
        let raw = self.unseal_bytes_with_aad(sealed, aad)?;
        bytes_to_embedding(&raw)
    }

    /// Encrypt arbitrary bytes (no AAD).
    pub fn seal_bytes(&self, data: &[u8]) -> Result<Vec<u8>> {
        self.seal_bytes_with_aad(data, None)
    }

    /// Decrypt a software-encrypted blob (no AAD).
    pub fn unseal_bytes(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        self.unseal_bytes_with_aad(sealed, None)
    }

    /// Encrypt arbitrary bytes, optionally binding to `aad`.
    pub fn seal_bytes_with_aad(&self, data: &[u8], aad: Option<&[u8]>) -> Result<Vec<u8>> {
        use aes_gcm::aead::{Aead, Nonce, Payload};
        use aes_gcm::{Aes256Gcm, KeyInit};
        use rand::Rng;

        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| FacelockError::Encryption(format!("failed to create AES cipher: {e}")))?;

        let mut nonce_bytes = [0u8; AES_NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = <&Nonce<Aes256Gcm>>::try_from(&nonce_bytes[..])
            .map_err(|e| FacelockError::Encryption(format!("invalid nonce: {e}")))?;

        let payload = Payload {
            msg: data,
            aad: aad.unwrap_or(&[]),
        };
        let ciphertext = cipher
            .encrypt(nonce, payload)
            .map_err(|e| FacelockError::Encryption(format!("AES-GCM encryption failed: {e}")))?;

        // Format: version byte + nonce + ciphertext (includes 16-byte tag).
        // The AAD is authenticated but not stored — it is re-derived at decrypt.
        let mut blob = Vec::with_capacity(1 + AES_NONCE_SIZE + ciphertext.len());
        blob.push(SOFTWARE_ENCRYPTED_VERSION_BYTE);
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        Ok(blob)
    }

    /// Decrypt a software-encrypted blob, optionally requiring `aad`.
    pub fn unseal_bytes_with_aad(&self, sealed: &[u8], aad: Option<&[u8]>) -> Result<Vec<u8>> {
        use aes_gcm::aead::{Aead, Nonce, Payload};
        use aes_gcm::{Aes256Gcm, KeyInit};

        let min_size = 1 + AES_NONCE_SIZE + 16; // version + nonce + tag (minimum)
        if sealed.len() < min_size {
            return Err(FacelockError::Encryption("encrypted blob too short".into()));
        }

        if sealed[0] != SOFTWARE_ENCRYPTED_VERSION_BYTE {
            return Err(FacelockError::Encryption(format!(
                "expected software encryption version byte 0x{:02x}, got 0x{:02x}",
                SOFTWARE_ENCRYPTED_VERSION_BYTE, sealed[0]
            )));
        }

        let nonce = <&Nonce<Aes256Gcm>>::try_from(&sealed[1..1 + AES_NONCE_SIZE])
            .map_err(|e| FacelockError::Encryption(format!("invalid nonce: {e}")))?;
        let ciphertext = &sealed[1 + AES_NONCE_SIZE..];

        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| FacelockError::Encryption(format!("failed to create AES cipher: {e}")))?;

        let payload = Payload {
            msg: ciphertext,
            aad: aad.unwrap_or(&[]),
        };
        cipher.decrypt(nonce, payload).map_err(|e| {
            FacelockError::Encryption(format!(
                "AES-GCM decryption failed (wrong key, AAD, or corrupted data): {e}"
            ))
        })
    }
}

impl Drop for SoftwareSealer {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
    }
}

/// Generate a random 32-byte AES key, seal it with TPM, and write to file.
///
/// The plaintext key is zeroized after sealing.
pub fn generate_and_seal_key(
    tpm: &mut TpmSealer,
    path: &Path,
    pcr_indices: Option<&[u32]>,
) -> Result<()> {
    use rand::Rng;
    use zeroize::Zeroize;

    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);

    let result = tpm.seal_key_to_file(&key, path, pcr_indices);
    key.zeroize();
    result
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn embedding_to_bytes(embedding: &FaceEmbedding) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RAW_EMBEDDING_SIZE);
    for &val in embedding.iter() {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

fn bytes_to_embedding(data: &[u8]) -> Result<FaceEmbedding> {
    if data.len() != RAW_EMBEDDING_SIZE {
        return Err(FacelockError::Storage(format!(
            "invalid embedding data size: expected {RAW_EMBEDDING_SIZE}, got {}",
            data.len()
        )));
    }
    let mut embedding = [0f32; 512];
    for (i, chunk) in data.chunks_exact(4).enumerate() {
        embedding[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    Ok(embedding)
}

/// Zero-on-drop wrapper for raw embedding bytes used during seal/unseal.
/// Ensures sensitive biometric data does not linger in memory.
pub struct ZeroizingBytes(Vec<u8>);

impl ZeroizingBytes {
    pub fn new(data: Vec<u8>) -> Self {
        Self(data)
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for ZeroizingBytes {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

/// Detect whether a blob is TPM-sealed (version byte 0x01 no-PCR, or 0x03
/// PCR-bound) rather than a raw 2048-byte embedding.
pub fn is_sealed(data: &[u8]) -> bool {
    !data.is_empty()
        && (data[0] == SEALED_VERSION_BYTE || data[0] == SEALED_PCR_VERSION_BYTE)
        && data.len() != RAW_EMBEDDING_SIZE
}

/// Detect whether a blob is software-encrypted (version byte 0x02).
pub fn is_software_encrypted(data: &[u8]) -> bool {
    !data.is_empty()
        && data[0] == SOFTWARE_ENCRYPTED_VERSION_BYTE
        && data.len() != RAW_EMBEDDING_SIZE
}

/// Detect whether a blob is encrypted (either TPM-sealed or software-encrypted).
pub fn is_encrypted(data: &[u8]) -> bool {
    is_sealed(data) || is_software_encrypted(data)
}

/// Return the raw embedding size constant.
pub fn raw_embedding_size() -> usize {
    RAW_EMBEDDING_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Get TCTI string from env (for swtpm in CI) or fall back to device path.
    #[cfg(not(feature = "tpm"))]
    fn test_tcti() -> String {
        std::env::var("TCTI").unwrap_or_else(|_| "device:/dev/tpmrm0".into())
    }

    #[test]
    #[cfg(not(feature = "tpm"))]
    fn seal_unseal_round_trip_passthrough() {
        let mut sealer = TpmSealer::new(&test_tcti()).unwrap();
        let mut emb = [0.0f32; 512];
        emb[0] = 1.0;
        emb[1] = -1.0;
        emb[511] = 42.0;

        let sealed = sealer.seal_embedding(&emb, None).unwrap();
        let unsealed = sealer.unseal_embedding(&sealed).unwrap();
        assert_eq!(emb, unsealed);
    }

    #[test]
    fn embedding_byte_conversion() {
        let emb = [0.5f32; 512];
        let bytes = embedding_to_bytes(&emb);
        assert_eq!(bytes.len(), RAW_EMBEDDING_SIZE);
        let recovered = bytes_to_embedding(&bytes).unwrap();
        assert_eq!(emb, recovered);
    }

    #[test]
    fn bytes_to_embedding_rejects_wrong_size() {
        let result = bytes_to_embedding(&[0u8; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn version_byte_detection() {
        // Raw embedding (2048 bytes) should not be detected as sealed
        let raw = vec![0u8; RAW_EMBEDDING_SIZE];
        assert!(!is_sealed(&raw));

        // Version-prefixed blob should be detected as sealed
        let mut sealed = vec![SEALED_VERSION_BYTE];
        sealed.extend_from_slice(&[0u8; 100]);
        assert!(is_sealed(&sealed));

        // Empty should not be sealed
        assert!(!is_sealed(&[]));

        // A raw embedding that happens to start with 0x01 and is exactly 2048 bytes
        // should NOT be detected as sealed (migration compat)
        let mut ambiguous = vec![0u8; RAW_EMBEDDING_SIZE];
        ambiguous[0] = SEALED_VERSION_BYTE;
        assert!(!is_sealed(&ambiguous));
    }

    #[cfg(not(feature = "tpm"))]
    #[test]
    fn passthrough_mode_reports_unavailable() {
        let sealer = TpmSealer::new("device:/dev/tpmrm0").unwrap();
        assert!(!sealer.is_available());
    }

    #[cfg(not(feature = "tpm"))]
    #[test]
    fn passthrough_rejects_sealed_blob() {
        let mut sealer = TpmSealer::new("device:/dev/tpmrm0").unwrap();
        // Construct a fake TPM-sealed blob (version byte + some data, not 2048 bytes)
        let mut fake_sealed = vec![SEALED_VERSION_BYTE];
        fake_sealed.extend_from_slice(&[0u8; 200]);
        let result = sealer.unseal_embedding(&fake_sealed);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("TPM"),
            "error should mention TPM: {err_msg}"
        );
    }

    #[test]
    fn software_seal_unseal_round_trip() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let mut emb = [0.0f32; 512];
        emb[0] = 1.0;
        emb[1] = -1.0;
        emb[511] = 42.0;

        let sealed = sealer.seal_embedding(&emb).unwrap();
        assert_eq!(sealed[0], SOFTWARE_ENCRYPTED_VERSION_BYTE);
        assert!(sealed.len() > RAW_EMBEDDING_SIZE); // encrypted is larger due to nonce + tag

        let unsealed = sealer.unseal_embedding(&sealed).unwrap();
        assert_eq!(emb, unsealed);
    }

    #[test]
    fn software_seal_wrong_key_fails() {
        let key1 = [0x42u8; 32];
        let key2 = [0x43u8; 32];
        let sealer1 = SoftwareSealer::from_key(key1);
        let sealer2 = SoftwareSealer::from_key(key2);

        let emb = [0.5f32; 512];
        let sealed = sealer1.seal_embedding(&emb).unwrap();

        let result = sealer2.unseal_embedding(&sealed);
        assert!(result.is_err(), "decryption with wrong key should fail");
    }

    #[test]
    fn software_encrypted_detection() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let data = b"test data";
        let encrypted = sealer.seal_bytes(data).unwrap();
        assert!(is_software_encrypted(&encrypted));
        assert!(!is_sealed(&encrypted));
        assert!(is_encrypted(&encrypted));
    }

    #[test]
    fn software_seal_bytes_round_trip() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let data = b"hello, biometric world!";
        let sealed = sealer.seal_bytes(data).unwrap();
        let unsealed = sealer.unseal_bytes(&sealed).unwrap();
        assert_eq!(unsealed, data);
    }

    #[test]
    fn software_seal_truncated_blob_fails() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        // Too short: version byte only
        let result = sealer.unseal_bytes(&[SOFTWARE_ENCRYPTED_VERSION_BYTE]);
        assert!(result.is_err());
    }

    #[test]
    fn software_sealer_generate_and_load_key_file() {
        let dir = std::env::temp_dir().join("facelock_key_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let key_path = dir.join("test.key");

        // Generate key file
        SoftwareSealer::generate_key_file(&key_path).unwrap();

        // Verify file exists and is 32 bytes
        let data = std::fs::read(&key_path).unwrap();
        assert_eq!(
            data.len(),
            AES_KEY_SIZE,
            "key file should be exactly 32 bytes"
        );

        // Verify permissions (0600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::metadata(&key_path).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o600, "key file should be 0600");
        }

        // Load key from file and verify round-trip
        let sealer = SoftwareSealer::from_key_file(&key_path).unwrap();
        let emb = [0.42f32; 512];
        let sealed = sealer.seal_embedding(&emb).unwrap();
        let unsealed = sealer.unseal_embedding(&sealed).unwrap();
        assert_eq!(emb, unsealed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn software_sealer_from_key_file_wrong_size() {
        let dir = std::env::temp_dir().join("facelock_key_wrong_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let key_path = dir.join("bad.key");
        std::fs::write(&key_path, [0u8; 16]).unwrap(); // 16 bytes instead of 32

        let result = SoftwareSealer::from_key_file(&key_path);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("32"),
            "error should mention expected size: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn software_sealer_from_key_file_missing() {
        let result = SoftwareSealer::from_key_file(std::path::Path::new("/nonexistent/key.file"));
        assert!(result.is_err());
    }

    #[test]
    fn zeroizing_bytes_clears_on_drop() {
        let data = vec![0xAA; 100];
        let wrapper = ZeroizingBytes::new(data);
        assert_eq!(wrapper.as_slice().len(), 100);
        assert!(wrapper.as_slice().iter().all(|&b| b == 0xAA));
        // Drop happens here; we can't verify memory after drop without unsafe,
        // but we verify the wrapper API works correctly
    }

    #[test]
    fn is_encrypted_covers_both_types() {
        // TPM-sealed
        let mut tpm_blob = vec![SEALED_VERSION_BYTE];
        tpm_blob.extend_from_slice(&[0u8; 100]);
        assert!(is_encrypted(&tpm_blob));
        assert!(is_sealed(&tpm_blob));
        assert!(!is_software_encrypted(&tpm_blob));

        // Software-encrypted
        let mut sw_blob = vec![SOFTWARE_ENCRYPTED_VERSION_BYTE];
        sw_blob.extend_from_slice(&[0u8; 100]);
        assert!(is_encrypted(&sw_blob));
        assert!(!is_sealed(&sw_blob));
        assert!(is_software_encrypted(&sw_blob));

        // Raw embedding (neither)
        let raw = vec![0u8; RAW_EMBEDDING_SIZE];
        assert!(!is_encrypted(&raw));

        // Empty
        assert!(!is_encrypted(&[]));
    }

    #[test]
    fn software_seal_different_nonces() {
        // Two seals with the same key and data should produce different ciphertexts
        // (due to random nonces)
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let data = b"same data";
        let sealed1 = sealer.seal_bytes(data).unwrap();
        let sealed2 = sealer.seal_bytes(data).unwrap();
        assert_ne!(
            sealed1, sealed2,
            "different nonces should produce different ciphertexts"
        );

        // Both should decrypt to the same data
        let unsealed1 = sealer.unseal_bytes(&sealed1).unwrap();
        let unsealed2 = sealer.unseal_bytes(&sealed2).unwrap();
        assert_eq!(unsealed1, unsealed2);
    }

    #[test]
    fn software_seal_tampered_ciphertext_fails() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let data = b"secret data";
        let mut sealed = sealer.seal_bytes(data).unwrap();

        // Tamper with ciphertext (flip a byte after the nonce)
        let tamper_idx = 1 + AES_NONCE_SIZE + 5;
        if tamper_idx < sealed.len() {
            sealed[tamper_idx] ^= 0xFF;
        }

        let result = sealer.unseal_bytes(&sealed);
        assert!(
            result.is_err(),
            "tampered ciphertext should fail authentication"
        );
    }

    #[test]
    fn software_unseal_wrong_version_byte() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        // Blob with version byte 0x03 (unknown)
        let mut blob = vec![0x03u8];
        blob.extend_from_slice(&[0u8; 50]);
        let result = sealer.unseal_bytes(&blob);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("version"),
            "error should mention version byte: {err}"
        );
    }

    #[test]
    fn software_seal_with_aad_round_trip() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let aad = b"facelock-device:046d:085e:ABC";

        let mut emb = [0.0f32; 512];
        emb[0] = 1.0;
        emb[7] = -0.5;
        let sealed = sealer.seal_embedding_with_aad(&emb, Some(aad)).unwrap();
        let unsealed = sealer
            .unseal_embedding_with_aad(&sealed, Some(aad))
            .unwrap();
        assert_eq!(emb, unsealed);
    }

    #[test]
    fn software_seal_with_wrong_aad_fails() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let emb = [0.25f32; 512];

        let sealed = sealer
            .seal_embedding_with_aad(&emb, Some(b"facelock-device:AAAA"))
            .unwrap();

        // Correct key but different AAD (different enrolling camera) must fail.
        assert!(
            sealer
                .unseal_embedding_with_aad(&sealed, Some(b"facelock-device:BBBB"))
                .is_err(),
            "decryption under a different AAD must fail (hard device binding)"
        );
        // Missing AAD must also fail once bound.
        assert!(
            sealer.unseal_embedding_with_aad(&sealed, None).is_err(),
            "decryption without the bound AAD must fail"
        );
    }

    #[test]
    fn software_seal_aad_none_is_compatible_with_plain() {
        // seal_bytes (no AAD) and unseal_bytes_with_aad(None) must interoperate,
        // so the default (unbound) path is unchanged by the AAD plumbing.
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let data = b"unbound blob";

        let sealed = sealer.seal_bytes(data).unwrap();
        let via_aad = sealer.unseal_bytes_with_aad(&sealed, None).unwrap();
        assert_eq!(via_aad, data);

        let sealed2 = sealer.seal_bytes_with_aad(data, None).unwrap();
        let via_plain = sealer.unseal_bytes(&sealed2).unwrap();
        assert_eq!(via_plain, data);
    }

    /// The ordinary-encryption contract (#312): an absent AAD and an empty
    /// AAD are the same thing to the cipher. Templates enrolled without hard
    /// device binding are sealed with `None`; a reader passing `Some(&[])`
    /// (or the reverse) must open them, and a bound blob must open under
    /// neither.
    #[test]
    fn software_seal_absent_aad_equals_empty_aad() {
        let key = [0x42u8; 32];
        let sealer = SoftwareSealer::from_key(key);
        let emb = [0.125f32; 512];

        let sealed_absent = sealer.seal_embedding_with_aad(&emb, None).unwrap();
        assert_eq!(
            sealer
                .unseal_embedding_with_aad(&sealed_absent, Some(&[]))
                .unwrap(),
            emb
        );
        let sealed_empty = sealer.seal_embedding_with_aad(&emb, Some(&[])).unwrap();
        assert_eq!(
            sealer
                .unseal_embedding_with_aad(&sealed_empty, None)
                .unwrap(),
            emb
        );

        let bound = sealer
            .seal_embedding_with_aad(&emb, Some(b"facelock-device:046d:085e:"))
            .unwrap();
        assert!(sealer.unseal_embedding_with_aad(&bound, None).is_err());
        assert!(sealer.unseal_embedding_with_aad(&bound, Some(&[])).is_err());
    }

    // Golden vectors for the software blob format: 0x02 | 12-byte nonce |
    // ciphertext || 16-byte tag. Both were produced by an AES-256-GCM
    // implementation outside the `aes-gcm` crate, so they pin the *on-disk*
    // format rather than whatever the current crate version happens to emit.
    // The round-trip tests above cannot catch a format change, because they
    // seal and unseal with the same code. Stored embeddings outlive any
    // dependency bump: if an `aes-gcm` upgrade ever changed nonce placement,
    // tag position, or AAD handling, every existing enrollment would silently
    // become undecryptable, and these are the tests that would say so.
    //
    // If these fail, the format moved: investigate, never update the constants.
    // Refreshing them to match new output converts a caught break into shipped
    // data loss. A deliberate format change needs a new version byte plus a
    // migration — `parse_sealed_blob` already dispatches 0x01 and 0x03.
    //
    // GOLDEN_KEY is synthetic and protects nothing, so publishing it is safe.
    // Never regenerate these from a real key file: that commits key material.
    const GOLDEN_KEY: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];
    const GOLDEN_PLAINTEXT: &[u8] = b"facelock software-sealed embedding v2";
    const GOLDEN_AAD: &[u8] = b"facelock-device:golden-cam";

    #[test]
    fn software_unseal_golden_blob_no_aad() {
        const GOLDEN_NO_AAD: [u8; 66] = [
            0x02, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x21,
            0x63, 0xb5, 0x7e, 0xa9, 0x8a, 0xa1, 0x70, 0xad, 0x32, 0xf8, 0xed, 0xc5, 0x9e, 0x19,
            0x1f, 0xe6, 0xfb, 0xf4, 0x51, 0x91, 0x17, 0x3a, 0x18, 0x18, 0x02, 0x88, 0xe7, 0x78,
            0x0d, 0x64, 0xdb, 0x6f, 0x77, 0x8e, 0x8a, 0x9d, 0xe9, 0x03, 0x38, 0x64, 0xb5, 0x0e,
            0x3c, 0xb5, 0x64, 0x9b, 0xd6, 0x69, 0x59, 0x6d, 0xb0, 0xac,
        ];

        let sealer = SoftwareSealer::from_key(GOLDEN_KEY);
        let plaintext = sealer
            .unseal_bytes(&GOLDEN_NO_AAD)
            .expect("golden unbound blob must still decrypt");
        assert_eq!(plaintext, GOLDEN_PLAINTEXT);
    }

    #[test]
    fn software_unseal_golden_blob_with_aad() {
        const GOLDEN_WITH_AAD: [u8; 66] = [
            0x02, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x21,
            0x63, 0xb5, 0x7e, 0xa9, 0x8a, 0xa1, 0x70, 0xad, 0x32, 0xf8, 0xed, 0xc5, 0x9e, 0x19,
            0x1f, 0xe6, 0xfb, 0xf4, 0x51, 0x91, 0x17, 0x3a, 0x18, 0x18, 0x02, 0x88, 0xe7, 0x78,
            0x0d, 0x64, 0xdb, 0x6f, 0x77, 0x8e, 0x8a, 0x9d, 0xa0, 0x39, 0x79, 0x50, 0xc3, 0xe0,
            0x74, 0xcb, 0x1b, 0x25, 0xc6, 0x69, 0x20, 0x78, 0xbc, 0xcf,
        ];

        let sealer = SoftwareSealer::from_key(GOLDEN_KEY);
        let plaintext = sealer
            .unseal_bytes_with_aad(&GOLDEN_WITH_AAD, Some(GOLDEN_AAD))
            .expect("golden device-bound blob must still decrypt");
        assert_eq!(plaintext, GOLDEN_PLAINTEXT);

        // The AAD is authenticated, not stored: decrypting without it must fail.
        assert!(
            sealer.unseal_bytes(&GOLDEN_WITH_AAD).is_err(),
            "golden device-bound blob must not decrypt without its AAD"
        );
    }

    #[cfg(unix)]
    #[test]
    fn generate_key_file_never_group_or_world_readable() {
        // Finding #11 (keyfile TOCTOU): a concurrent observer polling the key
        // path while it is repeatedly (re)generated must NEVER see it at a mode
        // wider than 0600. create-at-mode closes the create-then-chmod window.
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = std::env::temp_dir().join(format!("facelock_toctou_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("encryption.key");

        let stop = Arc::new(AtomicBool::new(false));
        let observed_bad = Arc::new(AtomicBool::new(false));
        let watch_path = key_path.clone();
        let stop_w = stop.clone();
        let bad_w = observed_bad.clone();
        let watcher = std::thread::spawn(move || {
            while !stop_w.load(Ordering::Relaxed) {
                if let Ok(meta) = std::fs::metadata(&watch_path) {
                    let mode = meta.permissions().mode() & 0o777;
                    // Any group/other bit set means the key was briefly exposed.
                    if mode & 0o077 != 0 {
                        bad_w.store(true, Ordering::Relaxed);
                    }
                }
            }
        });

        for _ in 0..2000 {
            SoftwareSealer::generate_key_file(&key_path).unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        watcher.join().unwrap();

        assert!(
            !observed_bad.load(Ordering::Relaxed),
            "key file was observed with group/other permission bits during generation"
        );
        let final_mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(final_mode, 0o600, "final key file mode must be 0600");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "tpm")]
    #[test]
    fn parse_sealed_blob_round_trips_both_formats() {
        // 0x01 (no PCR): 0x01 | pub_len(u32) | pub | priv
        let mut v1 = vec![super::SEALED_VERSION_BYTE];
        v1.extend_from_slice(&3u32.to_le_bytes());
        v1.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // pub
        v1.extend_from_slice(&[0x11, 0x22]); // priv
        let (idx, pubb, privb) = super::parse_sealed_blob(&v1).unwrap();
        assert!(idx.is_none());
        assert_eq!(pubb, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(privb, &[0x11, 0x22]);

        // 0x03 (PCR-bound): 0x03 | count | idx*count | pub_len(u32) | pub | priv
        let mut v3 = vec![super::SEALED_PCR_VERSION_BYTE, 2];
        v3.extend_from_slice(&0u32.to_le_bytes());
        v3.extend_from_slice(&7u32.to_le_bytes());
        v3.extend_from_slice(&2u32.to_le_bytes()); // pub_len
        v3.extend_from_slice(&[0xDE, 0xAD]); // pub
        v3.extend_from_slice(&[0xBE, 0xEF, 0x00]); // priv
        let (idx, pubb, privb) = super::parse_sealed_blob(&v3).unwrap();
        assert_eq!(idx, Some(vec![0, 7]));
        assert_eq!(pubb, &[0xDE, 0xAD]);
        assert_eq!(privb, &[0xBE, 0xEF, 0x00]);
    }

    #[test]
    #[cfg(not(feature = "tpm"))]
    fn seal_bytes_passthrough() {
        let mut sealer = TpmSealer::new(&test_tcti()).unwrap();
        let data = b"hello world";
        let result = sealer.seal_bytes(data, None).unwrap();

        #[cfg(not(feature = "tpm"))]
        assert_eq!(result, data);

        // With TPM feature, result will be a sealed blob (can't test without actual TPM)
        #[cfg(feature = "tpm")]
        assert!(!result.is_empty());
    }
}
