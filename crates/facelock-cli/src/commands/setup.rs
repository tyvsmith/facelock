use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};
use dialoguer::{Confirm, MultiSelect, Select, theme::ColorfulTheme};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use facelock_core::Config;
use facelock_core::fs_security::{
    create_truncate_file, ensure_mode, ensure_private_dir, write_file,
};

/// Embedded systemd unit file.
const SERVICE_UNIT: &str = include_str!("../../../../systemd/facelock-daemon.service");

/// Embedded D-Bus activation service file.
const DBUS_SERVICE: &str = include_str!("../../../../dbus/org.facelock.Daemon.service");

/// Embedded D-Bus policy configuration.
const DBUS_POLICY: &str = include_str!("../../../../dbus/org.facelock.Daemon.conf");

/// Embedded model manifest (same source as facelock-face).
const MANIFEST_TOML: &str = include_str!("../../../../models/manifest.toml");

/// Marker file written on successful setup completion.
pub const SETUP_COMPLETE_MARKER: &str = "/etc/facelock/.setup-complete";

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
    url: String,
    #[serde(default)]
    optional: bool,
}

impl ModelManifest {
    fn find(&self, filename: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.filename == filename)
    }
}

/// Check whether stdin is connected to an interactive terminal.
fn is_interactive() -> bool {
    std::io::stdin().is_terminal()
}

pub fn run(non_interactive: bool) -> anyhow::Result<()> {
    crate::ipc_client::require_root("sudo facelock setup")?;

    if non_interactive || !is_interactive() {
        return run_non_interactive();
    }

    run_wizard()
}

// ---------------------------------------------------------------------------
// Interactive wizard
// ---------------------------------------------------------------------------

fn run_wizard() -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    // -- Welcome --
    println!();
    println!("  Facelock v{}", env!("CARGO_PKG_VERSION"));
    println!("  Linux face authentication");
    println!();
    println!("  This wizard will walk you through initial setup:");
    println!("    - Camera detection");
    println!("    - Model quality and inference device");
    println!("    - Model downloads");
    println!("    - Embedding encryption (TPM or software)");
    println!("    - Face enrollment");
    println!("    - Daemon and PAM configuration");
    println!();

    // -- Load or create config --
    let mut config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not load config ({e}), using default paths");
            create_default_config()?;
            Config::load().context("failed to load config after creating default")?
        }
    };

    // -- Create directories (always needed) --
    create_directories(&config)?;

    // -- Step 1: Camera selection --
    println!("\n--- Step 1: Camera Selection ---\n");
    match wizard_camera_selection(&theme, &mut config) {
        Ok(()) => {}
        Err(e) => {
            println!("  Camera detection failed: {e}");
            println!("  You can configure the camera later in the config file.");
            println!(
                "  Continuing with current setting: {}",
                config.device.path.as_deref().unwrap_or("/dev/video0")
            );
        }
    }

    // -- Step 2: Model quality --
    println!("\n--- Step 2: Model Quality ---\n");
    match wizard_model_quality(&theme, &mut config) {
        Ok(()) => {}
        Err(e) => {
            println!("  Model quality selection failed: {e}");
            println!(
                "  Continuing with current setting: {}",
                config.recognition.detector_model
            );
        }
    }

    // -- Step 3: Execution provider --
    println!("\n--- Step 3: Inference Device ---\n");
    match wizard_execution_provider(&theme, &mut config) {
        Ok(()) => {}
        Err(e) => {
            println!("  Inference device selection failed: {e}");
            println!(
                "  Continuing with current setting: {}",
                config.recognition.execution_provider
            );
        }
    }

    // -- Step 4: Model download --
    println!("\n--- Step 4: Model Download ---\n");
    match wizard_model_download(&theme, &config) {
        Ok(()) => {}
        Err(e) => {
            println!("  Model download failed: {e}");
            println!("  You can retry later with: sudo facelock setup --non-interactive");
        }
    }

    // -- Step 5: Encryption setup --
    println!("\n--- Step 5: Embedding Encryption ---\n");
    match wizard_encryption_setup(&theme, &mut config) {
        Ok(()) => {}
        Err(e) => {
            println!("  Encryption setup failed: {e}");
            println!(
                "  You can configure encryption later with: sudo facelock encrypt --generate-key"
            );
        }
    }

    // -- Step 6: Face enrollment --
    println!("\n--- Step 6: Face Enrollment ---\n");
    let enrolled = match wizard_face_enroll(&theme) {
        Ok(did_enroll) => did_enroll,
        Err(e) => {
            println!("  Enrollment failed: {e}");
            println!("  You can enroll later with: facelock enroll");
            false
        }
    };

    // -- Step 7: Test recognition --
    if enrolled {
        println!("\n--- Step 7: Test Recognition ---\n");
        match wizard_test_recognition(&theme) {
            Ok(()) => {}
            Err(e) => {
                println!("  Test failed: {e}");
                println!("  You can test later with: facelock test");
            }
        }
    } else {
        println!("\n--- Step 7: Test Recognition (skipped, no face enrolled) ---\n");
    }

    // -- Step 8: Systemd setup --
    println!("\n--- Step 8: Daemon Configuration ---\n");
    let systemd_enabled = match wizard_systemd_setup(&theme) {
        Ok(enabled) => enabled,
        Err(e) => {
            println!("  Systemd setup failed: {e}");
            println!("  You can enable it later with: sudo facelock setup --systemd");
            false
        }
    };

    // -- Step 9: PAM configuration --
    println!("\n--- Step 9: PAM Configuration ---\n");
    let pam_services = match wizard_pam_setup(&theme) {
        Ok(services) => services,
        Err(e) => {
            println!("  PAM setup failed: {e}");
            println!("  You can configure PAM later with: sudo facelock setup --pam");
            Vec::new()
        }
    };

    // -- Summary --
    println!("\n--- Setup Complete ---\n");
    let encryption_label = match config.encryption.method {
        facelock_core::config::EncryptionMethod::Tpm => "AES-256-GCM (TPM-sealed key)",
        facelock_core::config::EncryptionMethod::Keyfile => "AES-256-GCM (keyfile)",
        facelock_core::config::EncryptionMethod::None => "none (NOT RECOMMENDED)",
    };
    let model_quality_label = match (
        config.recognition.detector_model.as_str(),
        config.recognition.embedder_model.as_str(),
    ) {
        ("det_10g.onnx", "glintr100.onnx") => "high accuracy (SCRFD 10G + ArcFace R100)",
        ("scrfd_2.5g_bnkps.onnx", "glintr100.onnx") => "balanced (SCRFD 2.5G + ArcFace R100)",
        _ => "standard (SCRFD 2.5G + ArcFace R50)",
    };
    println!(
        "  Camera:     {}",
        config.device.path.as_deref().unwrap_or("/dev/video0")
    );
    println!(
        "  Models:     {} ({})",
        config.daemon.model_dir, model_quality_label
    );
    println!(
        "  Inference:  {}",
        config.recognition.execution_provider.to_uppercase()
    );
    println!("  Database:   {}", config.storage.db_path);
    println!("  Encryption: {}", encryption_label);
    println!(
        "  Daemon:   {}",
        if systemd_enabled {
            "enabled (D-Bus activation)"
        } else {
            "not configured"
        }
    );
    if pam_services.is_empty() {
        println!("  PAM:      not configured");
    } else {
        println!("  PAM:      {}", pam_services.join(", "));
    }
    if enrolled {
        println!("  Face:     enrolled");
    } else {
        println!("  Face:     not enrolled (run `facelock enroll`)");
    }
    println!();

    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;
    secure_setup_paths(&config, Some(&manifest))?;
    write_setup_marker()?;
    Ok(())
}

fn write_setup_marker() -> anyhow::Result<()> {
    let path = std::path::Path::new(SETUP_COMPLETE_MARKER);
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent, 0o755)?;
    }
    write_file(path, b"", 0o644)?;
    Ok(())
}

fn wizard_camera_selection(theme: &ColorfulTheme, config: &mut Config) -> anyhow::Result<()> {
    let devices = facelock_camera::list_devices().map_err(|e| anyhow::anyhow!("{e}"))?;

    if devices.is_empty() {
        println!("  No video devices found.");
        println!("  Check that your camera is connected and the v4l2 module is loaded.");
        return Ok(());
    }

    let ir_devices: Vec<_> = devices
        .iter()
        .filter(|d| facelock_camera::is_ir_camera(d))
        .collect();

    // If exactly one IR camera, auto-select it
    if ir_devices.len() == 1 {
        let dev = ir_devices[0];
        println!("  Auto-selected IR camera: {} ({})", dev.path, dev.name);
        config.device.path = Some(dev.path.clone());
        return Ok(());
    }

    // Build display list
    let display_items: Vec<String> = devices
        .iter()
        .map(|d| {
            let ir_tag = if facelock_camera::is_ir_camera(d) {
                " [IR]"
            } else {
                ""
            };
            format!("{}{} - {}", d.path, ir_tag, d.name)
        })
        .collect();

    // Find the currently configured device index for default selection
    let default_idx = devices
        .iter()
        .position(|d| config.device.path.as_ref().is_some_and(|p| d.path == *p))
        .or_else(|| {
            // Default to first IR camera if available
            devices.iter().position(facelock_camera::is_ir_camera)
        })
        .unwrap_or(0);

    let selection = Select::with_theme(theme)
        .with_prompt("Select camera device")
        .items(&display_items)
        .default(default_idx)
        .interact()?;

    let selected = &devices[selection];
    config.device.path = Some(selected.path.clone());
    println!("  Selected: {} ({})", selected.path, selected.name);

    Ok(())
}

fn wizard_model_quality(theme: &ColorfulTheme, config: &mut Config) -> anyhow::Result<()> {
    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;
    let current_detector = &config.recognition.detector_model;
    let current_embedder = &config.recognition.embedder_model;

    let default_idx = match (current_detector.as_str(), current_embedder.as_str()) {
        ("det_10g.onnx", "glintr100.onnx") => 2,
        ("scrfd_2.5g_bnkps.onnx", "glintr100.onnx") => 1,
        _ => 0,
    };

    let options = [
        "Standard (recommended) — SCRFD 2.5G + ArcFace R50 (~170MB, fast)",
        "Balanced — SCRFD 2.5G + ArcFace R100 (~252MB, ~15-30ms slower)",
        "High accuracy — SCRFD 10G + ArcFace R100 (~266MB, ~40-50ms slower)",
    ];

    let selection = Select::with_theme(theme)
        .with_prompt("Select model quality")
        .items(&options)
        .default(default_idx)
        .interact()?;

    match selection {
        0 => {
            config.recognition.detector_model = "scrfd_2.5g_bnkps.onnx".to_string();
            config.recognition.detector_sha256 = manifest
                .find("scrfd_2.5g_bnkps.onnx")
                .map(|m| m.sha256.clone());
            config.recognition.embedder_model = "w600k_r50.onnx".to_string();
            config.recognition.embedder_sha256 =
                manifest.find("w600k_r50.onnx").map(|m| m.sha256.clone());
            println!("  Selected standard models (fast, good accuracy).");
        }
        1 => {
            config.recognition.detector_model = "scrfd_2.5g_bnkps.onnx".to_string();
            config.recognition.detector_sha256 = manifest
                .find("scrfd_2.5g_bnkps.onnx")
                .map(|m| m.sha256.clone());
            config.recognition.embedder_model = "glintr100.onnx".to_string();
            config.recognition.embedder_sha256 =
                manifest.find("glintr100.onnx").map(|m| m.sha256.clone());
            println!("  Selected balanced models (fast detection, high-accuracy embedding).");
        }
        _ => {
            config.recognition.detector_model = "det_10g.onnx".to_string();
            config.recognition.detector_sha256 =
                manifest.find("det_10g.onnx").map(|m| m.sha256.clone());
            config.recognition.embedder_model = "glintr100.onnx".to_string();
            config.recognition.embedder_sha256 =
                manifest.find("glintr100.onnx").map(|m| m.sha256.clone());
            println!("  Selected high-accuracy models (larger, ~40-50ms slower).");
        }
    }

    update_config_models(config)?;
    Ok(())
}

fn wizard_execution_provider(theme: &ColorfulTheme, config: &mut Config) -> anyhow::Result<()> {
    let current = config.recognition.execution_provider.as_str();

    let default_idx = match current {
        "cuda" => 1,
        _ => 0,
    };

    let options = [
        "CPU (recommended — works everywhere)",
        "CUDA (NVIDIA GPU — requires onnxruntime-opt-cuda package)",
    ];

    let selection = Select::with_theme(theme)
        .with_prompt("Select inference device")
        .items(&options)
        .default(default_idx)
        .interact()?;

    let provider = match selection {
        1 => "cuda",
        _ => "cpu",
    };

    config.recognition.execution_provider = provider.to_string();
    println!("  Selected: {}", provider);

    if provider == "cuda" {
        let has_nvidia_driver = Path::new("/dev/nvidiactl").exists();
        let has_cuda_ort = ["/usr/lib/libonnxruntime.so", "/usr/lib64/libonnxruntime.so"]
            .iter()
            .any(|p| Path::new(p).exists());

        if !has_nvidia_driver {
            println!("  \u{26a0} NVIDIA driver not detected. Install the NVIDIA driver package");
            println!("    before starting the daemon.");
        }
        if !has_cuda_ort {
            println!(
                "  \u{26a0} CUDA-enabled ONNX Runtime not found. Install onnxruntime-opt-cuda"
            );
            println!("    before starting the daemon, or inference will fall back to CPU.");
        }
    }

    update_config_provider(config)?;
    Ok(())
}

fn update_config_provider(config: &Config) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if !config_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;

    let provider = &config.recognition.execution_provider;

    if content.contains("[recognition]") {
        let mut new_content = String::new();
        let mut in_recognition = false;
        let mut provider_written = false;

        for line in content.lines() {
            if line.trim() == "[recognition]" {
                in_recognition = true;
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }
            if in_recognition && line.trim_start().starts_with("execution_provider") {
                new_content.push_str(&format!("execution_provider = \"{provider}\"\n"));
                provider_written = true;
                continue;
            }
            if in_recognition && line.starts_with('[') {
                if !provider_written {
                    new_content.push_str(&format!("execution_provider = \"{provider}\"\n"));
                }
                in_recognition = false;
            }
            new_content.push_str(line);
            new_content.push('\n');
        }
        if in_recognition && !provider_written {
            new_content.push_str(&format!("execution_provider = \"{provider}\"\n"));
        }
        write_file(&config_path, new_content.as_bytes(), 0o644)?;
    } else {
        let mut content = content;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!(
            "\n[recognition]\nexecution_provider = \"{provider}\"\n",
        ));
        write_file(&config_path, content.as_bytes(), 0o644)?;
    }

    Ok(())
}

fn wizard_model_download(theme: &ColorfulTheme, config: &Config) -> anyhow::Result<()> {
    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;

    let model_dir = Path::new(&config.daemon.model_dir);
    let configured_detector = &config.recognition.detector_model;
    let configured_embedder = &config.recognition.embedder_model;

    let needed: Vec<&ModelEntry> = manifest
        .models
        .iter()
        .filter(|m| {
            !m.optional || m.filename == *configured_detector || m.filename == *configured_embedder
        })
        .collect();

    // Check which models actually need downloading
    let mut to_download: Vec<&ModelEntry> = Vec::new();
    let mut already_present: Vec<&ModelEntry> = Vec::new();

    for entry in &needed {
        let model_path = model_dir.join(&entry.filename);
        match check_model(&model_path, &entry.sha256)? {
            ModelStatus::Present => already_present.push(entry),
            ModelStatus::Missing | ModelStatus::BadChecksum => to_download.push(entry),
        }
    }

    for entry in &already_present {
        println!("  [ok] {} ({})", entry.name, entry.purpose);
    }

    if to_download.is_empty() {
        println!("  All models are already present and verified.");
        return Ok(());
    }

    let total_mb: u64 = to_download.iter().map(|e| e.size_mb).sum();
    println!("  Models to download:");
    for entry in &to_download {
        println!(
            "    - {} (~{}MB) - {}",
            entry.name, entry.size_mb, entry.purpose
        );
    }
    println!("  Total download size: ~{}MB", total_mb);

    let proceed = Confirm::with_theme(theme)
        .with_prompt("Download required models?")
        .default(true)
        .interact()?;

    if !proceed {
        println!("  Skipping model download.");
        return Ok(());
    }

    for entry in &to_download {
        let model_path = model_dir.join(&entry.filename);
        println!("  Downloading {}...", entry.name);
        download_model(entry, &model_path)?;
        verify_after_download(&model_path, &entry.sha256, &entry.name)?;
        println!("  [ok] {} downloaded and verified", entry.name);
    }

    Ok(())
}

fn wizard_encryption_setup(theme: &ColorfulTheme, config: &mut Config) -> anyhow::Result<()> {
    use facelock_core::config::EncryptionMethod;

    println!("  Setting up AES-256-GCM encryption for face embeddings.");

    // Detect TPM availability
    let tpm_available = detect_tpm(config);

    if tpm_available {
        let options = [
            "TPM-protected key (recommended) — AES key sealed by TPM hardware",
            "Software keyfile — AES key stored as plaintext file",
        ];
        let selection = Select::with_theme(theme)
            .with_prompt("Select encryption key protection")
            .items(&options)
            .default(0)
            .interact()?;

        if selection == 0 {
            // TPM-sealed key
            let sealed_path = Path::new(&config.encryption.sealed_key_path);
            if sealed_path.exists() {
                println!(
                    "  TPM-sealed key already exists at {}.",
                    sealed_path.display()
                );
            } else {
                println!("  Generating and sealing AES key with TPM...");
                let pcr = if config.tpm.pcr_binding {
                    Some(config.tpm.pcr_indices.as_slice())
                } else {
                    None
                };
                #[cfg(feature = "tpm")]
                {
                    let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                        .context("failed to initialize TPM")?;
                    facelock_tpm::generate_and_seal_key(&mut tpm, sealed_path, pcr)
                        .context("failed to generate and seal key")?;
                    println!(
                        "  TPM-sealed key written to {} (permissions: 0600).",
                        sealed_path.display()
                    );
                }
                #[cfg(not(feature = "tpm"))]
                {
                    let _ = pcr;
                    anyhow::bail!("TPM support not compiled in (missing 'tpm' feature)");
                }
            }
            config.encryption.method = EncryptionMethod::Tpm;
            update_config_encryption(config, "tpm")?;
            println!("  Encryption enabled (TPM-sealed key).");
            return Ok(());
        }
    }

    // Software keyfile path
    let key_path = Path::new(&config.encryption.key_path);
    if key_path.exists() {
        println!("  Encryption key already exists at {}.", key_path.display());
    } else {
        println!("  Generating encryption key...");
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to generate encryption key")?;
        println!(
            "  Key written to {} (permissions: 0600).",
            key_path.display()
        );
    }

    config.encryption.method = EncryptionMethod::Keyfile;
    update_config_encryption(config, "keyfile")?;
    println!("  Encryption enabled.");

    Ok(())
}

/// Detect if TPM is available and functional.
fn detect_tpm(config: &Config) -> bool {
    // Check for TPM device
    let device_path = config
        .tpm
        .tcti
        .strip_prefix("device:")
        .unwrap_or(&config.tpm.tcti);
    if !Path::new(device_path).exists() {
        return false;
    }

    #[cfg(feature = "tpm")]
    {
        match facelock_tpm::TpmSealer::new(&config.tpm.tcti) {
            Ok(_) => {
                println!("  TPM 2.0 detected and functional.");
                true
            }
            Err(e) => {
                tracing::debug!("TPM detected but not functional: {e}");
                false
            }
        }
    }

    #[cfg(not(feature = "tpm"))]
    {
        false
    }
}

/// Update only the encryption method in the config file (no key_path changes).
/// Used by `facelock tpm seal-key` / `unseal-key` for migration.
#[cfg_attr(not(feature = "tpm"), allow(dead_code))]
pub fn update_config_encryption_method(method: &str) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    update_config_encryption(&config, method)
}

/// Update the config file on disk with the chosen encryption method.
fn update_config_encryption(config: &Config, method: &str) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if !config_path.exists() {
        return Ok(()); // Config will be created later
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;

    // Check if [encryption] section already exists
    if content.contains("[encryption]") {
        // Update existing section
        let mut new_content = String::new();
        let mut in_encryption = false;
        let mut method_written = false;
        for line in content.lines() {
            if line.trim() == "[encryption]" {
                in_encryption = true;
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }
            if in_encryption && line.trim_start().starts_with("method") {
                new_content.push_str(&format!("method = \"{method}\"\n"));
                method_written = true;
                continue;
            }
            if in_encryption && line.starts_with('[') {
                if !method_written {
                    new_content.push_str(&format!("method = \"{method}\"\n"));
                }
                in_encryption = false;
            }
            new_content.push_str(line);
            new_content.push('\n');
        }
        if in_encryption && !method_written {
            new_content.push_str(&format!("method = \"{method}\"\n"));
        }
        write_file(&config_path, new_content.as_bytes(), 0o644)?;
    } else {
        // Append new section
        let mut content = content;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!(
            "\n[encryption]\nmethod = \"{method}\"\nkey_path = \"{}\"\n",
            config.encryption.key_path
        ));
        write_file(&config_path, content.as_bytes(), 0o644)?;
    }

    Ok(())
}

fn update_config_models(config: &Config) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if !config_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let manifest: ModelManifest =
        toml::from_str(MANIFEST_TOML).context("failed to parse model manifest")?;

    let detector = &config.recognition.detector_model;
    let embedder = &config.recognition.embedder_model;
    let detector_sha = resolve_configured_model_sha256(
        &manifest,
        detector,
        config.recognition.detector_sha256.as_deref(),
    )?;
    let embedder_sha = resolve_configured_model_sha256(
        &manifest,
        embedder,
        config.recognition.embedder_sha256.as_deref(),
    )?;

    if content.contains("[recognition]") {
        let mut new_content = String::new();
        let mut in_recognition = false;
        let mut detector_written = false;
        let mut embedder_written = false;
        let mut detector_sha_written = false;
        let mut embedder_sha_written = false;

        for line in content.lines() {
            if line.trim() == "[recognition]" {
                in_recognition = true;
                new_content.push_str(line);
                new_content.push('\n');
                continue;
            }
            if in_recognition && line.trim_start().starts_with("detector_model") {
                new_content.push_str(&format!("detector_model = \"{detector}\"\n"));
                detector_written = true;
                continue;
            }
            if in_recognition && line.trim_start().starts_with("detector_sha256") {
                new_content.push_str(&format!("detector_sha256 = \"{detector_sha}\"\n"));
                detector_sha_written = true;
                continue;
            }
            if in_recognition && line.trim_start().starts_with("embedder_model") {
                new_content.push_str(&format!("embedder_model = \"{embedder}\"\n"));
                embedder_written = true;
                continue;
            }
            if in_recognition && line.trim_start().starts_with("embedder_sha256") {
                new_content.push_str(&format!("embedder_sha256 = \"{embedder_sha}\"\n"));
                embedder_sha_written = true;
                continue;
            }
            if in_recognition && line.starts_with('[') {
                if !detector_written {
                    new_content.push_str(&format!("detector_model = \"{detector}\"\n"));
                }
                if !detector_sha_written {
                    new_content.push_str(&format!("detector_sha256 = \"{detector_sha}\"\n"));
                }
                if !embedder_written {
                    new_content.push_str(&format!("embedder_model = \"{embedder}\"\n"));
                }
                if !embedder_sha_written {
                    new_content.push_str(&format!("embedder_sha256 = \"{embedder_sha}\"\n"));
                }
                in_recognition = false;
            }
            new_content.push_str(line);
            new_content.push('\n');
        }
        if in_recognition {
            if !detector_written {
                new_content.push_str(&format!("detector_model = \"{detector}\"\n"));
            }
            if !detector_sha_written {
                new_content.push_str(&format!("detector_sha256 = \"{detector_sha}\"\n"));
            }
            if !embedder_written {
                new_content.push_str(&format!("embedder_model = \"{embedder}\"\n"));
            }
            if !embedder_sha_written {
                new_content.push_str(&format!("embedder_sha256 = \"{embedder_sha}\"\n"));
            }
        }
        write_file(&config_path, new_content.as_bytes(), 0o644)?;
    } else {
        let mut content = content;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!(
            "\n[recognition]\ndetector_model = \"{detector}\"\ndetector_sha256 = \"{detector_sha}\"\nembedder_model = \"{embedder}\"\nembedder_sha256 = \"{embedder_sha}\"\n",
        ));
        write_file(&config_path, content.as_bytes(), 0o644)?;
    }

    Ok(())
}

fn resolve_configured_model_sha256(
    manifest: &ModelManifest,
    filename: &str,
    configured_sha256: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(entry) = manifest.find(filename) {
        if let Some(explicit) = configured_sha256
            && explicit != entry.sha256
        {
            anyhow::bail!("configured SHA256 for {filename} does not match bundled manifest");
        }
        return Ok(entry.sha256.clone());
    }

    if let Some(explicit) = configured_sha256 {
        if explicit.is_empty() {
            anyhow::bail!("custom model {filename} requires a non-empty SHA256");
        }
        return Ok(explicit.to_string());
    }

    anyhow::bail!("custom model {filename} requires an explicit SHA256 in config")
}

fn wizard_face_enroll(theme: &ColorfulTheme) -> anyhow::Result<bool> {
    let proceed = Confirm::with_theme(theme)
        .with_prompt("Would you like to enroll a face now?")
        .default(true)
        .interact()?;

    if !proceed {
        println!("  Skipping face enrollment.");
        return Ok(false);
    }

    super::enroll::run(None, None, true)?;
    Ok(true)
}

fn wizard_test_recognition(theme: &ColorfulTheme) -> anyhow::Result<()> {
    let proceed = Confirm::with_theme(theme)
        .with_prompt("Would you like to test recognition?")
        .default(true)
        .interact()?;

    if !proceed {
        println!("  Skipping recognition test.");
        return Ok(());
    }

    super::test_cmd::run(None)?;
    Ok(())
}

fn wizard_systemd_setup(theme: &ColorfulTheme) -> anyhow::Result<bool> {
    if !Path::new("/run/systemd/system").exists() {
        println!("  systemd not detected. Skipping daemon configuration.");
        println!("  Facelock will use oneshot mode for authentication.");
        return Ok(false);
    }

    let proceed = Confirm::with_theme(theme)
        .with_prompt("Enable daemon mode with D-Bus activation?")
        .default(true)
        .interact()?;

    if !proceed {
        println!("  Skipping systemd setup. Facelock will use oneshot mode.");
        return Ok(false);
    }

    run_systemd(false)?;
    Ok(true)
}

fn wizard_pam_setup(theme: &ColorfulTheme) -> anyhow::Result<Vec<String>> {
    if !Path::new(PAM_MODULE_PATH).exists() {
        println!("  PAM module not found at {PAM_MODULE_PATH}.");
        println!("  Install it first, then run: sudo facelock setup --pam");
        return Ok(Vec::new());
    }

    let available_services = ["sudo", "polkit-1", "hyprlock"];
    let service_descriptions = [
        "sudo     - authenticate sudo commands with face recognition",
        "polkit-1 - authenticate graphical privilege prompts with face recognition",
        "hyprlock - authenticate lock screen with face recognition",
    ];

    // Filter to services that actually exist on the system
    let mut valid_services: Vec<&str> = Vec::new();
    let mut valid_descriptions: Vec<&str> = Vec::new();
    for (svc, desc) in available_services.iter().zip(service_descriptions.iter()) {
        let pam_path = format!("/etc/pam.d/{svc}");
        if Path::new(&pam_path).exists() {
            valid_services.push(svc);
            valid_descriptions.push(desc);
        }
    }

    if valid_services.is_empty() {
        println!("  No supported PAM service files found in /etc/pam.d/.");
        return Ok(Vec::new());
    }

    let selections = MultiSelect::with_theme(theme)
        .with_prompt("Select PAM services to configure (space to toggle, enter to confirm)")
        .items(&valid_descriptions)
        .interact()?;

    if selections.is_empty() {
        println!("  No PAM services selected.");
        return Ok(Vec::new());
    }

    let mut configured = Vec::new();
    for idx in selections {
        let service = valid_services[idx];
        println!("  Configuring PAM for {service}...");
        match pam_install(service, true) {
            Ok(()) => configured.push(service.to_string()),
            Err(e) => {
                println!("  Failed to configure {service}: {e}");
            }
        }
    }

    Ok(configured)
}

// ---------------------------------------------------------------------------
// Non-interactive setup (original behavior)
// ---------------------------------------------------------------------------

fn run_non_interactive() -> anyhow::Result<()> {
    println!("facelock setup: preparing system...\n");

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

    // 3. Check and download needed models:
    //    - All non-optional models (defaults)
    //    - Any model whose filename matches the current config (user switched to optional models)
    let configured_detector = &config.recognition.detector_model;
    let configured_embedder = &config.recognition.embedder_model;

    let needed: Vec<&ModelEntry> = manifest
        .models
        .iter()
        .filter(|m| {
            !m.optional || m.filename == *configured_detector || m.filename == *configured_embedder
        })
        .collect();

    println!("Checking {} model(s)...\n", needed.len());

    for entry in &needed {
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

    // 4. Auto-configure encryption
    setup_encryption_auto(&config)?;

    secure_setup_paths(&config, Some(&manifest))?;
    write_setup_marker()?;
    println!("\nSetup complete. Run `facelock enroll` to register your face.");
    Ok(())
}

/// Auto-configure encryption in non-interactive mode.
/// Prefers TPM-sealed key if TPM is available, falls back to keyfile.
fn setup_encryption_auto(config: &Config) -> anyhow::Result<()> {
    use facelock_core::config::EncryptionMethod;

    // Skip if already configured
    if config.encryption.method != EncryptionMethod::None {
        println!(
            "  Encryption already configured ({:?}).",
            config.encryption.method
        );
        return Ok(());
    }

    // Try TPM first
    if detect_tpm(config) {
        #[cfg(feature = "tpm")]
        {
            let sealed_path = Path::new(&config.encryption.sealed_key_path);
            if !sealed_path.exists() {
                let pcr = if config.tpm.pcr_binding {
                    Some(config.tpm.pcr_indices.as_slice())
                } else {
                    None
                };
                let mut tpm = facelock_tpm::TpmSealer::new(&config.tpm.tcti)
                    .context("failed to initialize TPM")?;
                facelock_tpm::generate_and_seal_key(&mut tpm, sealed_path, pcr)
                    .context("failed to generate and seal key")?;
                println!(
                    "  [ok] Generated TPM-sealed encryption key at {}",
                    sealed_path.display()
                );
            }
            let mut config = config.clone();
            config.encryption.method = EncryptionMethod::Tpm;
            update_config_encryption(&config, "tpm")?;
            println!("  [ok] AES-256-GCM encryption enabled (TPM-sealed key).");
            return Ok(());
        }
    }

    // Fall back to keyfile
    let key_path = Path::new(&config.encryption.key_path);
    if !key_path.exists() {
        facelock_tpm::SoftwareSealer::generate_key_file(key_path)
            .context("failed to generate encryption key")?;
        println!("  [ok] Generated encryption key at {}", key_path.display());
    }

    let mut config = config.clone();
    config.encryption.method = EncryptionMethod::Keyfile;
    update_config_encryption(&config, "keyfile")?;
    println!("  [ok] AES-256-GCM encryption enabled.");
    Ok(())
}

#[cfg(unix)]
fn chown_path(path: &Path, uid: u32, gid: u32) -> anyhow::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains embedded NUL: {}", path.display()))?;
    let result = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
    if result != 0 {
        anyhow::bail!(
            "failed to chown {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn chown_path(_path: &Path, _uid: u32, _gid: u32) -> anyhow::Result<()> {
    Ok(())
}

fn facelock_group_gid() -> anyhow::Result<u32> {
    nix::unistd::Group::from_name("facelock")
        .context("failed to look up facelock group")?
        .map(|group| group.gid.as_raw())
        .context(
            "facelock group is missing; install package assets or create the facelock system group",
        )
}

fn secure_existing_path(path: &Path, mode: u32, gid: u32) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    ensure_mode(path, mode)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;

    if nix::unistd::Uid::current().is_root() {
        chown_path(path, 0, gid)?;
    }

    Ok(())
}

fn secure_dir_if_exists(path: &Path, mode: u32, gid: u32) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if !path.is_dir() {
        bail!(
            "expected directory but found non-directory path: {}",
            path.display()
        );
    }

    ensure_private_dir(path, mode)
        .with_context(|| format!("failed to secure directory {}", path.display()))?;
    if nix::unistd::Uid::current().is_root() {
        chown_path(path, 0, gid)?;
    }

    Ok(())
}

fn secure_setup_paths(config: &Config, manifest: Option<&ModelManifest>) -> anyhow::Result<()> {
    let facelock_gid = facelock_group_gid()?;
    let config_path = facelock_core::paths::config_path();
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/facelock"));
    let db_path = Path::new(&config.storage.db_path);
    let audit_path = Path::new(&config.audit.path);
    let key_path = Path::new(&config.encryption.key_path);
    let sealed_key_path = Path::new(&config.encryption.sealed_key_path);

    secure_dir_if_exists(config_dir, 0o755, 0)?;
    secure_dir_if_exists(Path::new(&config.daemon.model_dir), 0o755, 0)?;
    secure_dir_if_exists(Path::new(&config.snapshots.dir), 0o750, facelock_gid)?;

    if let Some(parent) = db_path.parent() {
        secure_dir_if_exists(parent, 0o750, facelock_gid)?;
    }
    if let Some(parent) = audit_path.parent() {
        secure_dir_if_exists(parent, 0o750, facelock_gid)?;
    }
    if let Some(parent) = key_path.parent() {
        secure_dir_if_exists(parent, 0o755, 0)?;
    }
    if let Some(parent) = sealed_key_path.parent() {
        secure_dir_if_exists(parent, 0o755, 0)?;
    }

    secure_existing_path(&config_path, 0o644, 0)?;
    secure_existing_path(db_path, 0o640, facelock_gid)?;
    secure_existing_path(audit_path, 0o640, facelock_gid)?;
    secure_existing_path(key_path, 0o600, 0)?;
    secure_existing_path(sealed_key_path, 0o600, 0)?;
    secure_existing_path(Path::new(SETUP_COMPLETE_MARKER), 0o644, 0)?;

    if let Some(manifest) = manifest {
        for entry in &manifest.models {
            let model_path = Path::new(&config.daemon.model_dir).join(&entry.filename);
            secure_existing_path(&model_path, 0o644, 0)?;
        }
    }

    Ok(())
}

fn create_directories(config: &Config) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    let mut dirs: Vec<(&Path, u32)> = vec![
        (Path::new(&config.daemon.model_dir), 0o755),
        (Path::new(&config.snapshots.dir), 0o750),
    ];

    for (path, mode) in [
        (&config.storage.db_path, 0o750),
        (&config.audit.path, 0o750),
        (&config.encryption.key_path, 0o755),
        (&config.encryption.sealed_key_path, 0o755),
    ] {
        if let Some(parent) = Path::new(path.as_str()).parent() {
            dirs.push((parent, mode));
        }
    }

    if let Some(parent) = config_path.parent() {
        dirs.push((parent, 0o755));
    }

    for (dir, mode) in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }

        ensure_private_dir(dir, mode)
            .with_context(|| format!("failed to create directory {}", dir.display()))?;
        tracing::debug!("ensured directory: {}", dir.display());
    }

    println!("  Directories created.");
    Ok(())
}

fn create_default_config() -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();
    if config_path.exists() {
        return Ok(());
    }

    if let Some(parent) = config_path.parent() {
        ensure_private_dir(parent, 0o755).context("failed to create config directory")?;
    }

    let default_config = r#"[device]
path = "/dev/video0"
"#;
    write_file(&config_path, default_config.as_bytes(), 0o644).with_context(|| {
        format!(
            "failed to write default config to {}",
            config_path.display()
        )
    })?;
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
    if entry.url.is_empty() {
        bail!("no download URL configured for {}", entry.name);
    }
    let url = entry.url.as_str();

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .context("failed to create HTTP client")?;

    let mut response = client
        .get(url)
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

    // Write atomically: write to temp file first, then rename
    let tmp_path = dest.with_extension("tmp");
    let mut file = create_truncate_file(&tmp_path, 0o644)
        .with_context(|| format!("failed to create {}", tmp_path.display()))?;

    let mut downloaded: u64 = 0;
    let mut buffer = vec![0u8; 8192];
    loop {
        use std::io::Read as _;
        let n = response
            .read(&mut buffer)
            .context("failed to read response")?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])
            .context("failed to write to temp file")?;
        downloaded += n as u64;
        pb.set_position(downloaded);
    }
    pb.finish_and_clear();
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp_path, dest)
        .with_context(|| format!("failed to rename temp file to {}", dest.display()))?;
    ensure_mode(dest, 0o644).with_context(|| format!("failed to secure {}", dest.display()))?;

    Ok(())
}

// --- PAM installation ---

const PAM_LINE: &str = "auth      sufficient pam_facelock.so";
const PAM_MODULE_PATH: &str = "/lib/security/pam_facelock.so";
const SENSITIVE_SERVICES: &[&str] = &["system-auth", "login", "sshd"];

/// Check if a PAM config line references pam_facelock, regardless of spacing.
fn is_facelock_pam_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.starts_with('#') && trimmed.contains("pam_facelock.so")
}

pub fn run_pam(service: &str, remove: bool, yes: bool) -> anyhow::Result<()> {
    // 1. Check root
    if !nix::unistd::Uid::current().is_root() {
        bail!("PAM configuration requires root. Run with sudo.");
    }

    if remove {
        pam_remove(service)
    } else {
        pam_install(service, yes)
    }
}

fn pam_install(service: &str, yes: bool) -> anyhow::Result<()> {
    // 2. Check PAM module exists
    if !Path::new(PAM_MODULE_PATH).exists() {
        bail!(
            "PAM module not found at {PAM_MODULE_PATH}.\n\
             Install it first: cargo build --release -p pam-facelock && \
             sudo cp target/release/libpam_facelock.so {PAM_MODULE_PATH}"
        );
    }

    // 3. Refuse sensitive services without --yes
    if SENSITIVE_SERVICES.contains(&service) && !yes {
        bail!(
            "Refusing to modify '{service}' without --yes flag.\n\
             This is a sensitive PAM service. Use: facelock setup --pam --service {service} --yes"
        );
    }

    let pam_path = format!("/etc/pam.d/{service}");
    let pam_file = Path::new(&pam_path);

    if !pam_file.exists() {
        bail!("PAM service file not found: {pam_path}");
    }

    // Read existing content
    let content =
        fs::read_to_string(pam_file).with_context(|| format!("failed to read {pam_path}"))?;

    // Check idempotency — match on the module name, not exact spacing
    if content.lines().any(is_facelock_pam_line) {
        println!("PAM line already present in {pam_path}. Nothing to do.");
        return Ok(());
    }

    // 4. Create backup (always, before any modification)
    let backup_path = format!("{pam_path}.facelock-backup");
    fs::copy(pam_file, &backup_path)
        .with_context(|| format!("failed to back up {pam_path} to {backup_path}"))?;
    println!("Backed up {pam_path} -> {backup_path}");

    // 5. Prepend PAM line before first auth line
    let mut new_lines: Vec<String> = Vec::new();
    let mut inserted = false;

    for line in content.lines() {
        if !inserted && line.trim_start().starts_with("auth") {
            new_lines.push(PAM_LINE.to_string());
            inserted = true;
        }
        new_lines.push(line.to_string());
    }

    if !inserted {
        // No auth line found; append at the top
        new_lines.insert(0, PAM_LINE.to_string());
    }

    // Preserve trailing newline
    let mut output = new_lines.join("\n");
    if content.ends_with('\n') {
        output.push('\n');
    }

    fs::write(pam_file, &output).with_context(|| format!("failed to write {pam_path}"))?;

    println!("Installed facelock PAM line into {pam_path}");
    println!("\nTo rollback:");
    println!("  sudo cp {backup_path} {pam_path}");
    println!("  # or: sudo facelock setup --pam --remove --service {service}");

    Ok(())
}

fn pam_remove(service: &str) -> anyhow::Result<()> {
    let pam_path = format!("/etc/pam.d/{service}");
    let pam_file = Path::new(&pam_path);

    if !pam_file.exists() {
        bail!("PAM service file not found: {pam_path}");
    }

    let content =
        fs::read_to_string(pam_file).with_context(|| format!("failed to read {pam_path}"))?;

    let original_count = content.lines().count();
    let new_lines: Vec<&str> = content
        .lines()
        .filter(|line| !is_facelock_pam_line(line))
        .collect();

    if new_lines.len() == original_count {
        println!("No facelock PAM line found in {pam_path}. Nothing to remove.");
    } else {
        let mut output = new_lines.join("\n");
        if content.ends_with('\n') {
            output.push('\n');
        }

        fs::write(pam_file, &output).with_context(|| format!("failed to write {pam_path}"))?;
        println!("Removed facelock PAM line from {pam_path}");
    }

    // Offer backup restore
    let backup_path = format!("{pam_path}.facelock-backup");
    if Path::new(&backup_path).exists() {
        println!("Backup exists at {backup_path}");
        println!("To restore: sudo cp {backup_path} {pam_path}");
    }

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

// ---------------------------------------------------------------------------
// systemd unit installation
// ---------------------------------------------------------------------------

const SYSTEMD_UNIT_DIR: &str = "/usr/lib/systemd/system";
const SERVICE_FILENAME: &str = "facelock-daemon.service";
const DBUS_SYSTEM_SERVICES_DIR: &str = "/usr/share/dbus-1/system-services";
const DBUS_SYSTEM_CONF_DIR: &str = "/usr/share/dbus-1/system.d";
const DBUS_SERVICE_FILENAME: &str = "org.facelock.Daemon.service";
const DBUS_POLICY_FILENAME: &str = "org.facelock.Daemon.conf";
const LEGACY_SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/facelock-daemon.service";
const LEGACY_DBUS_SYSTEM_SERVICE_PATH: &str =
    "/etc/dbus-1/system-services/org.facelock.Daemon.service";
const LEGACY_DBUS_SYSTEM_CONF_PATH: &str = "/etc/dbus-1/system.d/org.facelock.Daemon.conf";

fn check_systemd() -> anyhow::Result<()> {
    if !Path::new("/run/systemd/system").exists() {
        bail!("systemd not found — use manual daemon management or oneshot mode");
    }
    Ok(())
}

fn check_root() -> anyhow::Result<()> {
    let uid = unsafe { libc::getuid() };
    if uid != 0 {
        bail!("this command must be run as root (try: sudo facelock setup --systemd)");
    }
    Ok(())
}

fn run_cmd(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{program} {} failed: {stderr}", args.join(" "));
    }
    Ok(())
}

fn refresh_legacy_copy_if_present(path: &Path, contents: &str, marker: &str) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let existing = fs::read_to_string(path)
        .with_context(|| format!("failed to read existing legacy file {}", path.display()))?;
    if !existing.contains(marker) {
        return Ok(());
    }

    write_file(path, contents.as_bytes(), 0o644)
        .with_context(|| format!("failed to refresh {}", path.display()))?;
    println!("  Refreshed legacy {}", path.display());
    Ok(())
}

pub fn run_systemd(disable: bool) -> anyhow::Result<()> {
    check_root()?;
    check_systemd()?;

    if disable {
        println!("Disabling facelock-daemon systemd units...");
        run_cmd("systemctl", &["disable", "--now", "facelock-daemon"])?;
        println!("facelock-daemon service disabled and stopped.");
    } else {
        println!("Installing facelock-daemon systemd and D-Bus units...");

        // Install systemd service unit
        let unit_dir = Path::new(SYSTEMD_UNIT_DIR);
        fs::create_dir_all(unit_dir)
            .with_context(|| format!("failed to create {SYSTEMD_UNIT_DIR}"))?;

        let service_path = unit_dir.join(SERVICE_FILENAME);
        write_file(&service_path, SERVICE_UNIT.as_bytes(), 0o644)
            .with_context(|| format!("failed to write {}", service_path.display()))?;
        println!("  Wrote {}", service_path.display());
        refresh_legacy_copy_if_present(
            Path::new(LEGACY_SYSTEMD_UNIT_PATH),
            SERVICE_UNIT,
            "ExecStart=/usr/bin/facelock daemon",
        )?;

        // Install D-Bus policy file
        let conf_dir = Path::new(DBUS_SYSTEM_CONF_DIR);
        fs::create_dir_all(conf_dir)
            .with_context(|| format!("failed to create {DBUS_SYSTEM_CONF_DIR}"))?;

        let policy_path = conf_dir.join(DBUS_POLICY_FILENAME);
        write_file(&policy_path, DBUS_POLICY.as_bytes(), 0o644)
            .with_context(|| format!("failed to write {}", policy_path.display()))?;
        println!("  Wrote {}", policy_path.display());
        refresh_legacy_copy_if_present(
            Path::new(LEGACY_DBUS_SYSTEM_CONF_PATH),
            DBUS_POLICY,
            "org.facelock.Daemon",
        )?;

        // Install D-Bus activation service
        let svc_dir = Path::new(DBUS_SYSTEM_SERVICES_DIR);
        fs::create_dir_all(svc_dir)
            .with_context(|| format!("failed to create {DBUS_SYSTEM_SERVICES_DIR}"))?;

        let dbus_svc_path = svc_dir.join(DBUS_SERVICE_FILENAME);
        write_file(&dbus_svc_path, DBUS_SERVICE.as_bytes(), 0o644)
            .with_context(|| format!("failed to write {}", dbus_svc_path.display()))?;
        println!("  Wrote {}", dbus_svc_path.display());
        refresh_legacy_copy_if_present(
            Path::new(LEGACY_DBUS_SYSTEM_SERVICE_PATH),
            DBUS_SERVICE,
            "org.facelock.Daemon",
        )?;

        run_cmd("systemctl", &["daemon-reload"])?;
        println!("  systemctl daemon-reload done.");

        run_cmd("systemctl", &["enable", SERVICE_FILENAME])?;
        println!("  systemctl enable {SERVICE_FILENAME} done.");

        println!("\nfacelock-daemon D-Bus activation is now enabled.");
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
        let dir = std::env::temp_dir().join("facelock_cli_test_setup");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.bin");
        std::fs::write(&path, b"test data").unwrap();

        let status = check_model(&path, "").unwrap();
        assert!(matches!(status, ModelStatus::Present));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pam_insert_before_first_auth_line() {
        let original = "\
#%PAM-1.0
auth    include   system-local-login
auth    include   system-login
account include   system-login
";
        // Simulate the insertion logic
        let mut new_lines: Vec<String> = Vec::new();
        let mut inserted = false;
        for line in original.lines() {
            if !inserted && line.trim_start().starts_with("auth") {
                new_lines.push(PAM_LINE.to_string());
                inserted = true;
            }
            new_lines.push(line.to_string());
        }
        let mut output = new_lines.join("\n");
        if original.ends_with('\n') {
            output.push('\n');
        }

        assert!(inserted);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines[0], "#%PAM-1.0");
        assert_eq!(lines[1], PAM_LINE);
        assert_eq!(lines[2], "auth    include   system-local-login");
    }

    #[test]
    fn pam_idempotent_detection() {
        // Exact match
        let content = format!("#%PAM-1.0\n{PAM_LINE}\nauth    include   system-login\n");
        assert!(content.lines().any(|line| is_facelock_pam_line(line)));

        // Different spacing should still match
        let content2 =
            "#%PAM-1.0\nauth  sufficient  pam_facelock.so\nauth    include   system-login\n";
        assert!(content2.lines().any(|line| is_facelock_pam_line(line)));

        // Commented-out line should not match
        let content3 =
            "#%PAM-1.0\n#auth sufficient pam_facelock.so\nauth    include   system-login\n";
        assert!(!content3.lines().any(|line| is_facelock_pam_line(line)));
    }

    #[test]
    fn pam_remove_filters_line() {
        // Should remove regardless of spacing
        let content = "#%PAM-1.0\nauth  sufficient  pam_facelock.so\nauth    include   system-login\naccount include   system-login\n";
        let new_lines: Vec<&str> = content
            .lines()
            .filter(|line| !is_facelock_pam_line(line))
            .collect();
        assert_eq!(new_lines.len(), 3);
        assert!(!new_lines.iter().any(|l| is_facelock_pam_line(l)));
    }

    #[test]
    fn sensitive_services_detected() {
        assert!(SENSITIVE_SERVICES.contains(&"system-auth"));
        assert!(SENSITIVE_SERVICES.contains(&"login"));
        assert!(SENSITIVE_SERVICES.contains(&"sshd"));
        assert!(!SENSITIVE_SERVICES.contains(&"sudo"));
    }

    #[test]
    fn check_model_correct_sha256() {
        let dir = std::env::temp_dir().join("facelock_cli_test_sha");
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

    #[test]
    fn update_config_models_scenarios() {
        let dir = std::env::temp_dir().join("facelock_test_models_scenarios");
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");

        struct ProcessConfigOverrideGuard;

        impl Drop for ProcessConfigOverrideGuard {
            fn drop(&mut self) {
                facelock_core::paths::clear_process_config_override();
            }
        }

        facelock_core::paths::clear_process_config_override();
        facelock_core::paths::set_process_config_override(config_path.clone());
        let _override_guard = ProcessConfigOverrideGuard;

        // Scenario 1: appends [recognition] section when absent
        std::fs::write(&config_path, "[device]\npath = \"/dev/video0\"\n").unwrap();
        let mut config = Config::load_from(&config_path).unwrap();
        config.recognition.detector_model = "det_10g.onnx".to_string();
        config.recognition.embedder_model = "glintr100.onnx".to_string();
        update_config_models(&config).unwrap();

        let result = std::fs::read_to_string(&config_path).unwrap();
        assert!(result.contains("[recognition]"));
        assert!(result.contains("detector_model = \"det_10g.onnx\""));
        assert!(result.contains(
            "detector_sha256 = \"5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91\""
        ));
        assert!(result.contains("embedder_model = \"glintr100.onnx\""));
        assert!(result.contains(
            "embedder_sha256 = \"4ab1d6435d639628a6f3e5008dd4f929edf4c4124b1a7169e1048f9fef534cdf\""
        ));

        // Scenario 2: updates existing model fields, preserves other fields
        std::fs::write(
            &config_path,
            "[device]\npath = \"/dev/video0\"\n\n[recognition]\ndetector_model = \"scrfd_2.5g_bnkps.onnx\"\nembedder_model = \"w600k_r50.onnx\"\nthreshold = 0.80\n",
        )
        .unwrap();
        let mut config = Config::load_from(&config_path).unwrap();
        config.recognition.detector_model = "det_10g.onnx".to_string();
        config.recognition.embedder_model = "glintr100.onnx".to_string();
        update_config_models(&config).unwrap();

        let result = std::fs::read_to_string(&config_path).unwrap();
        assert!(result.contains("detector_model = \"det_10g.onnx\""));
        assert!(result.contains(
            "detector_sha256 = \"5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91\""
        ));
        assert!(result.contains("embedder_model = \"glintr100.onnx\""));
        assert!(result.contains(
            "embedder_sha256 = \"4ab1d6435d639628a6f3e5008dd4f929edf4c4124b1a7169e1048f9fef534cdf\""
        ));
        assert!(!result.contains("scrfd_2.5g_bnkps.onnx"));
        assert!(!result.contains("w600k_r50.onnx"));
        assert!(result.contains("threshold = 0.80"));

        // Scenario 3: adds model fields to existing [recognition] without them
        std::fs::write(
            &config_path,
            "[device]\npath = \"/dev/video0\"\n\n[recognition]\nthreshold = 0.75\n",
        )
        .unwrap();
        let mut config = Config::load_from(&config_path).unwrap();
        config.recognition.detector_model = "det_10g.onnx".to_string();
        config.recognition.embedder_model = "glintr100.onnx".to_string();
        update_config_models(&config).unwrap();

        let result = std::fs::read_to_string(&config_path).unwrap();
        assert!(result.contains("detector_model = \"det_10g.onnx\""));
        assert!(result.contains(
            "detector_sha256 = \"5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91\""
        ));
        assert!(result.contains("embedder_model = \"glintr100.onnx\""));
        assert!(result.contains(
            "embedder_sha256 = \"4ab1d6435d639628a6f3e5008dd4f929edf4c4124b1a7169e1048f9fef534cdf\""
        ));
        assert!(result.contains("threshold = 0.75"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
