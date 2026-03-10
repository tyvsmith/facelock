use std::path::Path;

use visage_core::error::{VisageError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MANIFEST_TOML: &str = include_str!("../../../models/manifest.toml");

#[derive(Debug, Deserialize)]
pub struct ModelManifest {
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ModelEntry {
    pub name: String,
    pub filename: String,
    pub purpose: String,
    pub size_mb: u64,
    pub sha256: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub optional: bool,
}

impl ModelManifest {
    pub fn load() -> Result<Self> {
        let manifest: ModelManifest =
            toml::from_str(MANIFEST_TOML).map_err(|e| VisageError::Detection(e.to_string()))?;
        Ok(manifest)
    }

    pub fn default_models(&self) -> Vec<&ModelEntry> {
        self.models.iter().filter(|m| !m.optional).collect()
    }
}

/// Verify a model file's SHA256 checksum.
/// If `expected_sha256` is empty, returns `Ok(true)` (no verification needed).
pub fn verify_model(path: &Path, expected_sha256: &str) -> Result<bool> {
    if expected_sha256.is_empty() {
        return Ok(true);
    }

    let data = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    let hex = format!("{result:x}");

    Ok(hex == expected_sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_manifest_parses_all_entries() {
        let manifest = ModelManifest::load().unwrap();
        assert_eq!(manifest.models.len(), 4);
        assert_eq!(manifest.models[0].name, "scrfd_2.5g");
        assert_eq!(manifest.models[1].name, "arcface_r50");
        assert_eq!(manifest.models[2].name, "scrfd_10g");
        assert_eq!(manifest.models[3].name, "arcface_r100");
    }

    #[test]
    fn default_models_excludes_optional() {
        let manifest = ModelManifest::load().unwrap();
        let defaults = manifest.default_models();
        assert_eq!(defaults.len(), 2);
        assert_eq!(defaults[0].name, "scrfd_2.5g");
        assert_eq!(defaults[1].name, "arcface_r50");
    }

    #[test]
    fn verify_model_empty_sha256_returns_true() {
        let result = verify_model(Path::new("/nonexistent"), "").unwrap();
        assert!(result);
    }

    #[test]
    fn verify_model_correct_sha256() {
        let dir = std::env::temp_dir().join("visage_test_verify");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_model.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello world").unwrap();
        drop(f);

        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_model(&path, expected).unwrap());
        assert!(!verify_model(&path, "0000000000000000000000000000000000000000000000000000000000000000").unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }
}
