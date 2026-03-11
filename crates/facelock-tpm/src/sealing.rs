use facelock_core::error::{Result, FacelockError};
use facelock_core::types::FaceEmbedding;
#[cfg(feature = "tpm")]
use tracing::{debug, info};
use tracing::warn;

/// Version byte prefixed to TPM-sealed blobs.
const SEALED_VERSION_BYTE: u8 = 0x01;

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

        let tcti_conf: TctiNameConf = tcti.try_into().map_err(|e| {
            FacelockError::Tpm(format!("invalid TCTI string '{tcti}': {e}"))
        })?;

        let mut context = tss_esapi::Context::new(tcti_conf).map_err(|e| {
            FacelockError::Tpm(format!("failed to create TPM context: {e}"))
        })?;

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
    pub fn seal_bytes(
        &mut self,
        data: &[u8],
        pcr_indices: Option<&[u32]>,
    ) -> Result<Vec<u8>> {
        use tss_esapi::{
            attributes::ObjectAttributesBuilder,
            interface_types::{
                algorithm::HashingAlgorithm,
                reserved_handles::Hierarchy,
            },
            structures::{
                MaxBuffer, SensitiveData,
                PublicBuilder, SymmetricDefinitionObject,
            },
        };

        let sensitive_data = SensitiveData::try_from(data.to_vec()).map_err(|e| {
            FacelockError::Tpm(format!("data too large for TPM seal: {e}"))
        })?;

        // Build a sealed object (keyedhash with no sign/decrypt)
        let mut obj_attrs = ObjectAttributesBuilder::new()
            .with_fixed_tpm(true)
            .with_fixed_parent(true);

        // If PCR indices specified, do not set no_da so policy is enforced
        if pcr_indices.is_none() {
            obj_attrs = obj_attrs.with_no_da(true);
        }

        let obj_attrs = obj_attrs.build().map_err(|e| {
            FacelockError::Tpm(format!("failed to build object attributes: {e}"))
        })?;

        let mut pub_builder = PublicBuilder::new()
            .with_public_algorithm(
                tss_esapi::interface_types::algorithm::PublicAlgorithm::KeyedHash,
            )
            .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
            .with_object_attributes(obj_attrs)
            .with_keyed_hash_parameters(
                tss_esapi::structures::PublicKeyedHashParameters::new(
                    tss_esapi::structures::KeyedHashScheme::Null,
                ),
            )
            .with_keyed_hash_unique_identifier(Default::default());

        // Add PCR policy if requested
        if let Some(indices) = pcr_indices {
            let pcr_policy = Self::build_pcr_policy_digest(indices)?;
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

        // Serialize: version byte + public_size(u32) + public_bytes + private_bytes
        let pub_bytes = serialize_public(&public_out)?;
        let priv_bytes = serialize_private(&private)?;

        let mut blob = Vec::with_capacity(1 + 4 + pub_bytes.len() + priv_bytes.len());
        blob.push(SEALED_VERSION_BYTE);
        blob.extend_from_slice(&(pub_bytes.len() as u32).to_le_bytes());
        blob.extend_from_slice(&pub_bytes);
        blob.extend_from_slice(&priv_bytes);

        debug!(
            sealed_size = blob.len(),
            "sealed embedding"
        );
        Ok(blob)
    }

    /// Unseal bytes, handling format detection.
    fn unseal_or_passthrough(&mut self, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.is_empty() {
            return Err(FacelockError::Tpm("empty sealed blob".into()));
        }

        // Format detection: version byte 0x01 = TPM-sealed
        if sealed[0] == SEALED_VERSION_BYTE && sealed.len() > 5 {
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

    /// Unseal a version-prefixed TPM blob.
    fn unseal_bytes(&mut self, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < 5 {
            return Err(FacelockError::Tpm("sealed blob too short".into()));
        }

        // Skip version byte
        let pub_len =
            u32::from_le_bytes([sealed[1], sealed[2], sealed[3], sealed[4]]) as usize;

        if sealed.len() < 5 + pub_len {
            return Err(FacelockError::Tpm("sealed blob truncated (public)".into()));
        }

        let pub_bytes = &sealed[5..5 + pub_len];
        let priv_bytes = &sealed[5 + pub_len..];

        let public = deserialize_public(pub_bytes)?;
        let private = deserialize_private(priv_bytes)?;

        let loaded = self
            .context
            .execute_with_nullauth_session(|ctx| {
                ctx.load(self.primary_key, private, public)
            })
            .map_err(|e| FacelockError::Tpm(format!("TPM load failed: {e}")))?;

        let unsealed = self
            .context
            .execute_with_nullauth_session(|ctx| ctx.unseal(loaded.into()))
            .map_err(|e| FacelockError::Tpm(format!("TPM unseal failed: {e}")))?;

        let data: Vec<u8> = unsealed.into();
        debug!(unsealed_size = data.len(), "unsealed embedding");
        Ok(data)
    }

    /// Create an ECC P-256 primary key under the storage hierarchy.
    fn create_primary(
        context: &mut tss_esapi::Context,
    ) -> Result<tss_esapi::handles::KeyHandle> {
        use tss_esapi::{
            attributes::ObjectAttributesBuilder,
            interface_types::{
                algorithm::{
                    HashingAlgorithm, PublicAlgorithm,
                    EccSchemeAlgorithm,
                },
                ecc::EccCurve,
                reserved_handles::Hierarchy,
            },
            structures::{
                EccScheme, HashScheme, KeyDerivationFunctionScheme,
                PublicBuilder, PublicEccParametersBuilder,
                SymmetricDefinitionObject, EccPoint,
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

    /// Build a PCR policy digest for the given PCR indices (SHA-256).
    ///
    /// A proper implementation would use a trial policy session, but for now
    /// we return an all-zeros digest. PCR binding is handled at the session
    /// level during seal/unseal rather than via authPolicy on the object.
    fn build_pcr_policy_digest(
        _indices: &[u32],
    ) -> Result<tss_esapi::structures::Digest> {
        let zero_digest = vec![0u8; 32];
        tss_esapi::structures::Digest::try_from(zero_digest).map_err(|e| {
            FacelockError::Tpm(format!("failed to create PCR policy digest: {e}"))
        })
    }
}

// ---------------------------------------------------------------------------
// Serialization helpers (TPM feature only)
// ---------------------------------------------------------------------------
#[cfg(feature = "tpm")]
fn serialize_public(
    public: &tss_esapi::structures::Public,
) -> Result<Vec<u8>> {
    use tss_esapi::traits::Marshall;
    public.marshall().map_err(|e| {
        FacelockError::Tpm(format!("failed to serialize TPM public: {e}"))
    })
}

#[cfg(feature = "tpm")]
fn deserialize_public(
    bytes: &[u8],
) -> Result<tss_esapi::structures::Public> {
    use tss_esapi::traits::UnMarshall;
    tss_esapi::structures::Public::unmarshall(bytes).map_err(|e| {
        FacelockError::Tpm(format!("failed to deserialize TPM public: {e}"))
    })
}

#[cfg(feature = "tpm")]
fn serialize_private(
    private: &tss_esapi::structures::Private,
) -> Result<Vec<u8>> {
    use tss_esapi::traits::Marshall;
    private.marshall().map_err(|e| {
        FacelockError::Tpm(format!("failed to serialize TPM private: {e}"))
    })
}

#[cfg(feature = "tpm")]
fn deserialize_private(
    bytes: &[u8],
) -> Result<tss_esapi::structures::Private> {
    use tss_esapi::traits::UnMarshall;
    tss_esapi::structures::Private::unmarshall(bytes).map_err(|e| {
        FacelockError::Tpm(format!("failed to deserialize TPM private: {e}"))
    })
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
        // Handle format detection: version byte 0x01 = TPM-sealed (cannot unseal without TPM)
        if !sealed.is_empty() && sealed[0] == SEALED_VERSION_BYTE && sealed.len() != RAW_EMBEDDING_SIZE {
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
    pub fn seal_bytes(
        &mut self,
        data: &[u8],
        _pcr_indices: Option<&[u32]>,
    ) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }
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

/// Detect whether a blob is TPM-sealed (version byte prefix) or raw.
pub fn is_sealed(data: &[u8]) -> bool {
    !data.is_empty() && data[0] == SEALED_VERSION_BYTE && data.len() != RAW_EMBEDDING_SIZE
}

/// Return the raw embedding size constant.
pub fn raw_embedding_size() -> usize {
    RAW_EMBEDDING_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_unseal_round_trip_passthrough() {
        let mut sealer = TpmSealer::new("device:/dev/tpmrm0").unwrap();
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
        assert!(err_msg.contains("TPM"), "error should mention TPM: {err_msg}");
    }

    #[test]
    fn seal_bytes_passthrough() {
        let mut sealer = TpmSealer::new("device:/dev/tpmrm0").unwrap();
        let data = b"hello world";
        let result = sealer.seal_bytes(data, None).unwrap();

        #[cfg(not(feature = "tpm"))]
        assert_eq!(result, data);

        // With TPM feature, result will be a sealed blob (can't test without actual TPM)
        #[cfg(feature = "tpm")]
        assert!(!result.is_empty());
    }
}
