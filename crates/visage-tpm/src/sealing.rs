use visage_core::error::{VisageError, Result};
use visage_core::types::FaceEmbedding;
use tracing::warn;

/// TPM-based embedding sealer.
///
/// Currently operates in passthrough mode (no actual TPM operations).
/// When `tss-esapi` integration is added, this will seal/unseal
/// embeddings using a TPM-bound key.
pub struct TpmSealer {
    available: bool,
    #[allow(dead_code)]
    tcti: String,
}

impl TpmSealer {
    /// Create a new TpmSealer. Attempts to connect to TPM via the given TCTI string.
    /// If TPM is not available, operates in passthrough mode.
    pub fn new(tcti: &str) -> Self {
        // TODO: Connect to TPM via tss-esapi when dependency is added.
        // For now, always operate in passthrough mode.
        warn!("TPM support not yet implemented, operating in passthrough mode");
        Self {
            available: false,
            tcti: tcti.to_string(),
        }
    }

    /// Whether a real TPM is available.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Seal an embedding using TPM.
    /// In passthrough mode, returns raw bytes.
    pub fn seal_embedding(&mut self, embedding: &FaceEmbedding) -> Result<Vec<u8>> {
        if !self.available {
            return Ok(embedding_to_bytes(embedding));
        }
        // TODO: Actual TPM sealing with tss-esapi
        Ok(embedding_to_bytes(embedding))
    }

    /// Unseal an embedding.
    /// In passthrough mode, interprets bytes directly.
    pub fn unseal_embedding(&mut self, sealed: &[u8]) -> Result<FaceEmbedding> {
        if sealed.len() != 512 * 4 {
            return Err(VisageError::Storage(format!(
                "invalid sealed embedding size: expected {}, got {}",
                512 * 4,
                sealed.len()
            )));
        }
        bytes_to_embedding(sealed)
    }
}

fn embedding_to_bytes(embedding: &FaceEmbedding) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(512 * 4);
    for &val in embedding.iter() {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

fn bytes_to_embedding(data: &[u8]) -> Result<FaceEmbedding> {
    if data.len() != 512 * 4 {
        return Err(VisageError::Storage("invalid embedding data size".into()));
    }
    let mut embedding = [0f32; 512];
    for (i, chunk) in data.chunks_exact(4).enumerate() {
        embedding[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    Ok(embedding)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_unseal_round_trip() {
        let mut sealer = TpmSealer::new("device:/dev/tpmrm0");
        let mut emb = [0.0f32; 512];
        emb[0] = 1.0;
        emb[1] = -1.0;
        emb[511] = 42.0;

        let sealed = sealer.seal_embedding(&emb).unwrap();
        let unsealed = sealer.unseal_embedding(&sealed).unwrap();
        assert_eq!(emb, unsealed);
    }

    #[test]
    fn unseal_rejects_wrong_size() {
        let mut sealer = TpmSealer::new("device:/dev/tpmrm0");
        let result = sealer.unseal_embedding(&[0u8; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn passthrough_mode() {
        let sealer = TpmSealer::new("device:/dev/tpmrm0");
        assert!(!sealer.is_available());
    }

    #[test]
    fn embedding_byte_conversion() {
        let emb = [0.5f32; 512];
        let bytes = embedding_to_bytes(&emb);
        assert_eq!(bytes.len(), 512 * 4);
        let recovered = bytes_to_embedding(&bytes).unwrap();
        assert_eq!(emb, recovered);
    }
}
