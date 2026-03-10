use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use visage_core::Config;

/// Embedded systemd unit files.
const SERVICE_UNIT: &str = include_str!("../../../../systemd/visage-daemon.service");
const SOCKET_UNIT: &str = include_str!("../../../../systemd/visage-daemon.socket");

/// Embedded model manifest (same source as visage-face).
const MANIFEST_TOML: &str = include_str!("../../../../models/manifest.toml");

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

pub fn run() -> anyhow::Result<()> {
    println!("visage setup: preparing system...\n");

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

    println!("\nSetup complete. Run `visage enroll` to register your face.");
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
    if let Some(parent) = Path::new(&visage_core::paths::config_path()).parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory: {}", parent.display()))?;
    }

    println!("  Directories created.");
    Ok(())
}

fn create_default_config() -> anyhow::Result<()> {
    let config_path = visage_core::paths::config_path();
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
    if entry.url.is_empty() {
        bail!("no download URL configured for {}", entry.name);
    }
    let url = entry.url.as_str();

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .context("failed to create HTTP client")?;

    let response = client
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

// --- PAM installation ---

const PAM_LINE: &str = "auth  sufficient  pam_visage.so";
const PAM_MODULE_PATH: &str = "/lib/security/pam_visage.so";
const SENSITIVE_SERVICES: &[&str] = &["system-auth", "login", "sshd"];

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
             Install it first: cargo build --release -p pam-visage && \
             sudo cp target/release/libpam_visage.so {PAM_MODULE_PATH}"
        );
    }

    // 3. Refuse sensitive services without --yes
    if SENSITIVE_SERVICES.contains(&service) && !yes {
        bail!(
            "Refusing to modify '{service}' without --yes flag.\n\
             This is a sensitive PAM service. Use: visage setup --pam --service {service} --yes"
        );
    }

    let pam_path = format!("/etc/pam.d/{service}");
    let pam_file = Path::new(&pam_path);

    if !pam_file.exists() {
        bail!("PAM service file not found: {pam_path}");
    }

    // Read existing content
    let content = fs::read_to_string(pam_file)
        .with_context(|| format!("failed to read {pam_path}"))?;

    // Check idempotency
    if content.lines().any(|line| line.trim() == PAM_LINE) {
        println!("PAM line already present in {pam_path}. Nothing to do.");
        return Ok(());
    }

    // 4. Create backup (always, before any modification)
    let backup_path = format!("{pam_path}.visage-backup");
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

    fs::write(pam_file, &output)
        .with_context(|| format!("failed to write {pam_path}"))?;

    println!("Installed visage PAM line into {pam_path}");
    println!("\nTo rollback:");
    println!("  sudo cp {backup_path} {pam_path}");
    println!("  # or: sudo visage setup --pam --remove --service {service}");

    Ok(())
}

fn pam_remove(service: &str) -> anyhow::Result<()> {
    let pam_path = format!("/etc/pam.d/{service}");
    let pam_file = Path::new(&pam_path);

    if !pam_file.exists() {
        bail!("PAM service file not found: {pam_path}");
    }

    let content = fs::read_to_string(pam_file)
        .with_context(|| format!("failed to read {pam_path}"))?;

    let original_count = content.lines().count();
    let new_lines: Vec<&str> = content
        .lines()
        .filter(|line| line.trim() != PAM_LINE)
        .collect();

    if new_lines.len() == original_count {
        println!("No visage PAM line found in {pam_path}. Nothing to remove.");
    } else {
        let mut output = new_lines.join("\n");
        if content.ends_with('\n') {
            output.push('\n');
        }

        fs::write(pam_file, &output)
            .with_context(|| format!("failed to write {pam_path}"))?;
        println!("Removed visage PAM line from {pam_path}");
    }

    // Offer backup restore
    let backup_path = format!("{pam_path}.visage-backup");
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
const SERVICE_FILENAME: &str = "visage-daemon.service";
const SOCKET_FILENAME: &str = "visage-daemon.socket";

fn check_systemd() -> anyhow::Result<()> {
    if !Path::new("/run/systemd/system").exists() {
        bail!("systemd not found — use manual daemon management or oneshot mode");
    }
    Ok(())
}

fn check_root() -> anyhow::Result<()> {
    let uid = unsafe { libc::getuid() };
    if uid != 0 {
        bail!("this command must be run as root (try: sudo visage setup --systemd)");
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

pub fn run_systemd(disable: bool) -> anyhow::Result<()> {
    check_root()?;
    check_systemd()?;

    if disable {
        println!("Disabling visage-daemon systemd units...");
        run_cmd("systemctl", &["disable", "--now", SOCKET_FILENAME, "visage-daemon"])?;
        println!("visage-daemon socket and service disabled and stopped.");
    } else {
        println!("Installing visage-daemon systemd units...");

        let unit_dir = Path::new(SYSTEMD_UNIT_DIR);
        fs::create_dir_all(unit_dir)
            .with_context(|| format!("failed to create {SYSTEMD_UNIT_DIR}"))?;

        let service_path = unit_dir.join(SERVICE_FILENAME);
        fs::write(&service_path, SERVICE_UNIT)
            .with_context(|| format!("failed to write {}", service_path.display()))?;
        println!("  Wrote {}", service_path.display());

        let socket_path = unit_dir.join(SOCKET_FILENAME);
        fs::write(&socket_path, SOCKET_UNIT)
            .with_context(|| format!("failed to write {}", socket_path.display()))?;
        println!("  Wrote {}", socket_path.display());

        run_cmd("systemctl", &["daemon-reload"])?;
        println!("  systemctl daemon-reload done.");

        run_cmd("systemctl", &["enable", "--now", SOCKET_FILENAME])?;
        println!("  systemctl enable --now {SOCKET_FILENAME} done.");

        println!("\nvisage-daemon socket activation is now enabled.");
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
        let dir = std::env::temp_dir().join("visage_cli_test_setup");
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
        let content = format!(
            "#%PAM-1.0\n{PAM_LINE}\nauth    include   system-login\n"
        );
        let already_present = content.lines().any(|line| line.trim() == PAM_LINE);
        assert!(already_present);
    }

    #[test]
    fn pam_remove_filters_line() {
        let content = format!(
            "#%PAM-1.0\n{PAM_LINE}\nauth    include   system-login\naccount include   system-login\n"
        );
        let new_lines: Vec<&str> = content
            .lines()
            .filter(|line| line.trim() != PAM_LINE)
            .collect();
        assert_eq!(new_lines.len(), 3);
        assert!(!new_lines.iter().any(|l| l.trim() == PAM_LINE));
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
        let dir = std::env::temp_dir().join("visage_cli_test_sha");
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
