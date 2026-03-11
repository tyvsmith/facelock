use serde::{Deserialize, Serialize};
use facelock_core::error::{Result, FacelockError};
#[cfg(feature = "tpm")]
use tracing::debug;
use tracing::warn;

/// A captured baseline of PCR values for later verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PcrBaseline {
    /// (pcr_index, sha256_digest) pairs
    pub values: Vec<(u32, Vec<u8>)>,
}

/// PCR (Platform Configuration Register) verifier.
///
/// Reads PCR values from the TPM and verifies them against a stored baseline.
/// Used to detect boot environment changes (e.g., firmware updates, rootkits).
pub struct PcrVerifier {
    pcr_indices: Vec<u32>,
    #[allow(dead_code)]
    tcti: String,
}

impl PcrVerifier {
    /// Create a new PCR verifier for the given indices.
    pub fn new(pcr_indices: &[u32], tcti: &str) -> Self {
        Self {
            pcr_indices: pcr_indices.to_vec(),
            tcti: tcti.to_string(),
        }
    }

    /// The PCR indices this verifier is configured for.
    pub fn indices(&self) -> &[u32] {
        &self.pcr_indices
    }
}

// ---------------------------------------------------------------------------
// Real TPM implementation
// ---------------------------------------------------------------------------
#[cfg(feature = "tpm")]
impl PcrVerifier {
    /// Read current PCR values from the TPM.
    /// Returns (pcr_index, sha256_digest) pairs for each configured index.
    pub fn read_current(
        context: &mut tss_esapi::Context,
        indices: &[u32],
    ) -> Result<Vec<(u32, Vec<u8>)>> {
        use tss_esapi::{
            interface_types::algorithm::HashingAlgorithm,
            structures::{PcrSelectionListBuilder, PcrSlot},
        };

        if indices.is_empty() {
            return Ok(Vec::new());
        }

        let slots: Vec<PcrSlot> = indices
            .iter()
            .map(|&i| pcr_index_to_slot(i))
            .collect::<Result<Vec<_>>>()?;

        let selection = PcrSelectionListBuilder::new()
            .with_selection(HashingAlgorithm::Sha256, &slots)
            .build()
            .map_err(|e| FacelockError::Tpm(format!("failed to build PCR selection: {e}")))?;

        let (_, _, pcr_data) = context
            .execute_without_session(|ctx| ctx.pcr_read(selection))
            .map_err(|e| FacelockError::Tpm(format!("PCR read failed: {e}")))?;

        let digests: Vec<&tss_esapi::structures::Digest> = pcr_data.value().iter().collect();

        if digests.len() != indices.len() {
            return Err(FacelockError::Tpm(format!(
                "PCR read returned {} digests but expected {}",
                digests.len(),
                indices.len()
            )));
        }

        let result: Vec<(u32, Vec<u8>)> = indices
            .iter()
            .zip(digests.iter())
            .map(|(&idx, digest)| {
                let bytes: Vec<u8> = digest.as_ref().to_vec();
                (idx, bytes)
            })
            .collect();

        debug!(count = result.len(), "read PCR values");
        Ok(result)
    }

    /// Capture a baseline of current PCR values for the configured indices.
    pub fn capture_baseline(
        context: &mut tss_esapi::Context,
        indices: &[u32],
    ) -> Result<PcrBaseline> {
        let values = Self::read_current(context, indices)?;
        Ok(PcrBaseline { values })
    }

    /// Verify current PCR values against a stored baseline.
    /// Returns true if all PCR values match.
    pub fn verify_against_baseline(
        context: &mut tss_esapi::Context,
        baseline: &PcrBaseline,
    ) -> Result<bool> {
        if baseline.values.is_empty() {
            return Ok(true);
        }

        let indices: Vec<u32> = baseline.values.iter().map(|(i, _)| *i).collect();
        let current = Self::read_current(context, &indices)?;

        for ((idx, baseline_digest), (_, current_digest)) in
            baseline.values.iter().zip(current.iter())
        {
            if baseline_digest != current_digest {
                warn!(
                    pcr_index = idx,
                    "PCR value mismatch against baseline"
                );
                return Ok(false);
            }
        }

        debug!("all PCR values match baseline");
        Ok(true)
    }
}

#[cfg(feature = "tpm")]
fn pcr_index_to_slot(index: u32) -> Result<tss_esapi::structures::PcrSlot> {
    use tss_esapi::structures::PcrSlot;
    match index {
        0 => Ok(PcrSlot::Slot0),
        1 => Ok(PcrSlot::Slot1),
        2 => Ok(PcrSlot::Slot2),
        3 => Ok(PcrSlot::Slot3),
        4 => Ok(PcrSlot::Slot4),
        5 => Ok(PcrSlot::Slot5),
        6 => Ok(PcrSlot::Slot6),
        7 => Ok(PcrSlot::Slot7),
        8 => Ok(PcrSlot::Slot8),
        9 => Ok(PcrSlot::Slot9),
        10 => Ok(PcrSlot::Slot10),
        11 => Ok(PcrSlot::Slot11),
        12 => Ok(PcrSlot::Slot12),
        13 => Ok(PcrSlot::Slot13),
        14 => Ok(PcrSlot::Slot14),
        15 => Ok(PcrSlot::Slot15),
        16 => Ok(PcrSlot::Slot16),
        17 => Ok(PcrSlot::Slot17),
        18 => Ok(PcrSlot::Slot18),
        19 => Ok(PcrSlot::Slot19),
        20 => Ok(PcrSlot::Slot20),
        21 => Ok(PcrSlot::Slot21),
        22 => Ok(PcrSlot::Slot22),
        23 => Ok(PcrSlot::Slot23),
        _ => Err(FacelockError::Tpm(format!("invalid PCR index: {index} (must be 0-23)"))),
    }
}

// ---------------------------------------------------------------------------
// Stub implementation (no TPM)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "tpm"))]
impl PcrVerifier {
    /// Read current PCR values. Not available without TPM support.
    pub fn read_current_stub(&self) -> Result<Vec<(u32, Vec<u8>)>> {
        warn!("TPM PCR reading not available (compile with 'tpm' feature)");
        Err(FacelockError::Tpm(
            "TPM PCR reading not available (compile with 'tpm' feature)".into(),
        ))
    }

    /// Verify current PCR values against a stored baseline. Not available without TPM support.
    pub fn verify_against_baseline_stub(&self, baseline: &PcrBaseline) -> Result<bool> {
        if baseline.values.len() != self.pcr_indices.len() {
            return Err(FacelockError::Tpm(format!(
                "baseline has {} entries but {} PCR indices configured",
                baseline.values.len(),
                self.pcr_indices.len()
            )));
        }
        warn!("TPM PCR verification not available (compile with 'tpm' feature)");
        Err(FacelockError::Tpm(
            "TPM PCR verification not available (compile with 'tpm' feature)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcr_verifier_creation() {
        let verifier = PcrVerifier::new(&[0, 1, 2, 3, 7], "device:/dev/tpmrm0");
        assert_eq!(verifier.indices(), &[0, 1, 2, 3, 7]);
    }

    #[test]
    fn pcr_baseline_serialize_deserialize() {
        let baseline = PcrBaseline {
            values: vec![
                (0, vec![0xAA; 32]),
                (1, vec![0xBB; 32]),
                (7, vec![0xCC; 32]),
            ],
        };

        let json = serde_json::to_string(&baseline).unwrap();
        let recovered: PcrBaseline = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.values.len(), 3);
        assert_eq!(recovered.values[0].0, 0);
        assert_eq!(recovered.values[0].1, vec![0xAA; 32]);
    }

    #[test]
    fn empty_baseline() {
        let baseline = PcrBaseline { values: vec![] };
        assert!(baseline.values.is_empty());
    }

    #[cfg(not(feature = "tpm"))]
    #[test]
    fn read_current_stub_returns_error() {
        let verifier = PcrVerifier::new(&[0], "device:/dev/tpmrm0");
        assert!(verifier.read_current_stub().is_err());
    }

    #[cfg(not(feature = "tpm"))]
    #[test]
    fn verify_rejects_mismatched_baseline_length() {
        let verifier = PcrVerifier::new(&[0, 1, 2], "device:/dev/tpmrm0");
        let baseline = PcrBaseline {
            values: vec![(0, vec![0u8; 32])], // only 1 entry for 3 indices
        };
        let result = verifier.verify_against_baseline_stub(&baseline);
        assert!(result.is_err());
    }

    #[cfg(feature = "tpm")]
    #[test]
    fn pcr_index_to_slot_valid() {
        // Test a few valid conversions
        assert!(pcr_index_to_slot(0).is_ok());
        assert!(pcr_index_to_slot(7).is_ok());
        assert!(pcr_index_to_slot(23).is_ok());
    }

    #[cfg(feature = "tpm")]
    #[test]
    fn pcr_index_to_slot_invalid() {
        assert!(pcr_index_to_slot(24).is_err());
        assert!(pcr_index_to_slot(100).is_err());
    }
}
