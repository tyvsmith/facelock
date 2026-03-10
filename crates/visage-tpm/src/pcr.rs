use visage_core::error::{VisageError, Result};
use tracing::warn;

/// PCR (Platform Configuration Register) verifier.
///
/// Reads PCR values from the TPM and verifies them against a stored baseline.
/// Used to detect boot environment changes (e.g., firmware updates, rootkits).
///
/// Currently a stub — actual TPM PCR reading requires `tss-esapi`.
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

    /// Read current PCR values from the TPM.
    /// Returns one digest per configured PCR index.
    pub fn read_current(&self) -> Result<Vec<Vec<u8>>> {
        // TODO: Implement with tss-esapi
        warn!("TPM PCR reading not yet implemented");
        Err(VisageError::Daemon(
            "TPM PCR reading not yet implemented".into(),
        ))
    }

    /// Verify current PCR values against a stored baseline.
    /// Returns true if all PCR values match.
    pub fn verify_against_baseline(&self, baseline: &[Vec<u8>]) -> Result<bool> {
        if baseline.len() != self.pcr_indices.len() {
            return Err(VisageError::Daemon(format!(
                "baseline has {} entries but {} PCR indices configured",
                baseline.len(),
                self.pcr_indices.len()
            )));
        }
        // TODO: Read current PCRs and compare
        warn!("TPM PCR verification not yet implemented");
        Err(VisageError::Daemon(
            "TPM PCR verification not yet implemented".into(),
        ))
    }

    /// The PCR indices this verifier is configured for.
    pub fn indices(&self) -> &[u32] {
        &self.pcr_indices
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
    fn read_current_returns_not_implemented() {
        let verifier = PcrVerifier::new(&[0], "device:/dev/tpmrm0");
        assert!(verifier.read_current().is_err());
    }

    #[test]
    fn verify_rejects_mismatched_baseline_length() {
        let verifier = PcrVerifier::new(&[0, 1, 2], "device:/dev/tpmrm0");
        let baseline = vec![vec![0u8; 32]]; // only 1 entry for 3 indices
        let result = verifier.verify_against_baseline(&baseline);
        assert!(result.is_err());
    }

    #[test]
    fn verify_not_yet_implemented() {
        let verifier = PcrVerifier::new(&[0], "device:/dev/tpmrm0");
        let baseline = vec![vec![0u8; 32]];
        assert!(verifier.verify_against_baseline(&baseline).is_err());
    }
}
