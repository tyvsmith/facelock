use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, bail};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use howdy_core::Config;

/// Embedded model manifest (same source as howdy-face).
const MANIFEST_TOML: &str = include_str!("../../../../models/manifest.toml");

/// Base URL for model downloads (InsightFace models hosted on GitHub).
const MODEL_BASE_URL: &str =
    "https://github.com/nickvdyck/howdy-models/releases/download/v1.0.0";

#[derive(Debug, serde::Deserialize)]
struct ModelManifest {
    models: Vec<ModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct ModelEntry {
    name: String,
    filename: String,
    purpose: String,
    size_mb: u64,
    sha256: String,
    #[serde(default)]
    optional: bool,
}

pub fn run() -> anyhow::Result<()> {
    println!("howdy setup: preparing system...\n");

    // Load config (or use defaults for paths)
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not load config ({e}), using default paths");
            create_default_config()?;
            Config::load().context("failed to load config after creating default")?
        }
    };

    // 1. Create directories
    create_directories(&config)?;

    // 2. Parse model manifest
    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;

    let model_dir = Path::new(&config.daemon.model_dir);

    // 3. Check and download required models
    let required: Vec<&ModelEntry> = manifest.models.iter().filter(|m| !m.optional).collect();

    println!("Checking {} required model(s)...\n", required.len());

    for entry in &required {
        let model_path = model_dir.join(&entry.filename);
        let status = check_model(&model_path, &entry.sha256)?;

        match status {
            ModelStatus::Present => {
                println!("  [ok] {} ({})", entry.name, entry.purpose);
            }
            ModelStatus::Missing => {
                println!(
                    "  [download] {} (~{}MB) - {}",
                    entry.name, entry.size_mb, entry.purpose
                );
                download_model(entry, &model_path)?;
                verify_after_download(&model_path, &entry.sha256, &entry.name)?;
                println!("  [ok] {} downloaded and verified", entry.name);
            }
            ModelStatus::BadChecksum => {
                println!(
                    "  [redownload] {} - checksum mismatch, re-downloading",
                    entry.name
                );
                download_model(entry, &model_path)?;
                verify_after_download(&model_path, &entry.sha256, &entry.name)?;
                println!("  [ok] {} re-downloaded and verified", entry.name);
            }
        }
    }

    println!("\nSetup complete.");
    Ok(())
}

fn create_directories(config: &Config) -> anyhow::Result<()> {
    let mut all_dirs: Vec<String> = vec![
        config.daemon.model_dir.clone(),
        config.snapshots.dir.clone(),
    ];

    // Also ensure parent dirs for db and socket
    for path_str in [&config.storage.db_path, &config.daemon.socket_path] {
        if let Some(parent) = Path::new(path_str.as_str()).parent() {
            all_dirs.push(parent.to_string_lossy().to_string());
        }
    }

    for dir in &all_dirs {
        if !dir.is_empty() {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create directory: {dir}"))?;
            tracing::debug!("ensured directory: {dir}");
        }
    }

    // Config directory
    if let Some(parent) = Path::new(&howdy_core::paths::config_path()).parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory: {}", parent.display()))?;
    }

    println!("  Directories created.");
    Ok(())
}

fn create_default_config() -> anyhow::Result<()> {
    let config_path = howdy_core::paths::config_path();
    if config_path.exists() {
        return Ok(());
    }

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).context("failed to create config directory")?;
    }

    let default_config = r#"[device]
path = "/dev/video0"
"#;
    fs::write(&config_path, default_config)
        .with_context(|| format!("failed to write default config to {}", config_path.display()))?;
    println!("  Created default config at {}", config_path.display());
    Ok(())
}

enum ModelStatus {
    Present,
    Missing,
    BadChecksum,
}

fn check_model(path: &Path, expected_sha256: &str) -> anyhow::Result<ModelStatus> {
    if !path.exists() {
        return Ok(ModelStatus::Missing);
    }

    // If no checksum configured, accept any existing file
    if expected_sha256.is_empty() {
        return Ok(ModelStatus::Present);
    }

    let data = fs::read(path).context("failed to read model file")?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let hex = format!("{:x}", hasher.finalize());

    if hex == expected_sha256 {
        Ok(ModelStatus::Present)
    } else {
        Ok(ModelStatus::BadChecksum)
    }
}

fn download_model(entry: &ModelEntry, dest: &Path) -> anyhow::Result<()> {
    let url = format!("{MODEL_BASE_URL}/{}", entry.filename);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .context("failed to create HTTP client")?;

    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to download {}", entry.name))?;

    if !response.status().is_success() {
        bail!(
            "download failed for {}: HTTP {}",
            entry.name,
            response.status()
        );
    }

    let total_size = response
        .content_length()
        .unwrap_or(entry.size_mb * 1024 * 1024);

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::with_template(
            "    {spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .expect("valid template")
        .progress_chars("#>-"),
    );

    let bytes = response.bytes().context("failed to read response body")?;
    pb.set_position(bytes.len() as u64);
    pb.finish_and_clear();

    // Write atomically: write to temp file first, then rename
    let tmp_path = dest.with_extension("tmp");
    let mut file = fs::File::create(&tmp_path)
        .with_context(|| format!("failed to create {}", tmp_path.display()))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp_path, dest)
        .with_context(|| format!("failed to rename temp file to {}", dest.display()))?;

    Ok(())
}

fn verify_after_download(path: &Path, expected_sha256: &str, name: &str) -> anyhow::Result<()> {
    if expected_sha256.is_empty() {
        return Ok(());
    }

    let data = fs::read(path).context("failed to read downloaded model")?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let hex = format!("{:x}", hasher.finalize());

    if hex != expected_sha256 {
        // Remove the bad file
        fs::remove_file(path).ok();
        bail!("SHA256 verification failed for {name}: expected {expected_sha256}, got {hex}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manifest() {
        let manifest: ModelManifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert_eq!(manifest.models.len(), 4);

        let required: Vec<_> = manifest.models.iter().filter(|m| !m.optional).collect();
        assert_eq!(required.len(), 2);
        assert_eq!(required[0].name, "scrfd_2.5g");
        assert_eq!(required[1].name, "arcface_r50");
    }

    #[test]
    fn check_model_missing_file() {
        let status = check_model(Path::new("/nonexistent/model.onnx"), "abc123").unwrap();
        assert!(matches!(status, ModelStatus::Missing));
    }

    #[test]
    fn check_model_empty_sha256_accepts_any() {
        let dir = std::env::temp_dir().join("howdy_cli_test_setup");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.bin");
        std::fs::write(&path, b"test data").unwrap();

        let status = check_model(&path, "").unwrap();
        assert!(matches!(status, ModelStatus::Present));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_model_correct_sha256() {
        let dir = std::env::temp_dir().join("howdy_cli_test_sha");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.bin");
        std::fs::write(&path, b"hello world").unwrap();

        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let status = check_model(&path, expected).unwrap();
        assert!(matches!(status, ModelStatus::Present));

        let status = check_model(&path, "0000000000000000").unwrap();
        assert!(matches!(status, ModelStatus::BadChecksum));

        std::fs::remove_dir_all(&dir).ok();
    }
}
