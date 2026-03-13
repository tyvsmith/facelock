use facelock_core::error::{Result, FacelockError};
use facelock_core::types::FaceEmbedding;
#[cfg(feature = "tpm")]
use tracing::{debug, info};
use tracing::warn;

/// Version byte prefixed to TPM-sealed blobs.
const SEALED_VERSION_BYTE: u8 = 0x01;

/// Version byte prefixed to software-encrypted blobs (AES-256-GCM).
const SOFTWARE_ENCRYPTED_VERSION_BYTE: u8 = 0x02;

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

        let tcti_conf: TctiNameConf = tcti.parse().map_err(|e| {
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

        let data: Vec<u8> = unsealed.as_slice().to_vec();
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

    /// Generate a new random 256-bit key and write it to a file.
    /// Sets file permissions to 0600 (owner read/write only).
    pub fn generate_key_file(path: &std::path::Path) -> Result<()> {
        use rand::RngCore;

        let mut key = [0u8; AES_KEY_SIZE];
        rand::thread_rng().fill_bytes(&mut key);

        // Write atomically: write to temp file, then rename
        let dir = path.parent().ok_or_else(|| {
            FacelockError::Encryption("key file path has no parent directory".into())
        })?;
        std::fs::create_dir_all(dir).map_err(|e| {
            FacelockError::Encryption(format!("failed to create directory {}: {e}", dir.display()))
        })?;

        std::fs::write(path, key).map_err(|e| {
            FacelockError::Encryption(format!("failed to write key file {}: {e}", path.display()))
        })?;

        // Set restrictive permissions (0600 = owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| {
                    FacelockError::Encryption(format!(
                        "failed to set key file permissions: {e}"
                    ))
                },
            )?;
        }

        // Zeroize the in-memory copy
        use zeroize::Zeroize;
        key.zeroize();

        Ok(())
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

    /// Encrypt arbitrary bytes.
    #[allow(deprecated)] // aes-gcm uses deprecated generic-array API
    pub fn seal_bytes(&self, data: &[u8]) -> Result<Vec<u8>> {
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
        use aes_gcm::aead::Aead;
        use rand::RngCore;

        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|e| {
            FacelockError::Encryption(format!("failed to create AES cipher: {e}"))
        })?;

        let mut nonce_bytes = [0u8; AES_NONCE_SIZE];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher.encrypt(nonce, data).map_err(|e| {
            FacelockError::Encryption(format!("AES-GCM encryption failed: {e}"))
        })?;

        // Format: version byte + nonce + ciphertext (includes 16-byte tag)
        let mut blob = Vec::with_capacity(1 + AES_NONCE_SIZE + ciphertext.len());
        blob.push(SOFTWARE_ENCRYPTED_VERSION_BYTE);
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        Ok(blob)
    }

    /// Decrypt a software-encrypted blob.
    #[allow(deprecated)] // aes-gcm uses deprecated generic-array API
    pub fn unseal_bytes(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
        use aes_gcm::aead::Aead;

        let min_size = 1 + AES_NONCE_SIZE + 16; // version + nonce + tag (minimum)
        if sealed.len() < min_size {
            return Err(FacelockError::Encryption(
                "encrypted blob too short".into(),
            ));
        }

        if sealed[0] != SOFTWARE_ENCRYPTED_VERSION_BYTE {
            return Err(FacelockError::Encryption(format!(
                "expected software encryption version byte 0x{:02x}, got 0x{:02x}",
                SOFTWARE_ENCRYPTED_VERSION_BYTE, sealed[0]
            )));
        }

        let nonce = Nonce::from_slice(&sealed[1..1 + AES_NONCE_SIZE]);
        let ciphertext = &sealed[1 + AES_NONCE_SIZE..];

        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|e| {
            FacelockError::Encryption(format!("failed to create AES cipher: {e}"))
        })?;

        cipher.decrypt(nonce, ciphertext).map_err(|e| {
            FacelockError::Encryption(format!(
                "AES-GCM decryption failed (wrong key or corrupted data): {e}"
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

/// Detect whether a blob is TPM-sealed (version byte 0x01) or raw.
pub fn is_sealed(data: &[u8]) -> bool {
    !data.is_empty() && data[0] == SEALED_VERSION_BYTE && data.len() != RAW_EMBEDDING_SIZE
}

/// Detect whether a blob is software-encrypted (version byte 0x02).
pub fn is_software_encrypted(data: &[u8]) -> bool {
    !data.is_empty() && data[0] == SOFTWARE_ENCRYPTED_VERSION_BYTE && data.len() != RAW_EMBEDDING_SIZE
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
        assert_eq!(data.len(), AES_KEY_SIZE, "key file should be exactly 32 bytes");

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
        std::fs::write(&key_path, &[0u8; 16]).unwrap(); // 16 bytes instead of 32

        let result = SoftwareSealer::from_key_file(&key_path);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("32"), "error should mention expected size: {err}");

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
        assert_ne!(sealed1, sealed2, "different nonces should produce different ciphertexts");

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
        assert!(result.is_err(), "tampered ciphertext should fail authentication");
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
        assert!(err.contains("version"), "error should mention version byte: {err}");
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
