use std::process::Command;

use anyhow::{Context, bail};
use clap::Subcommand;

/// Verbs on the config file.
///
/// `None` at the call site means [`ConfigCommand::Show`]: bare `facelock
/// config` still displays, which is the unprivileged read DEC-6 lists (ADR
/// 009).
#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Print the config file path and contents (what bare `facelock config` does)
    Show,
    /// Open the config file in $EDITOR
    Edit,
}

pub fn run(command: Option<ConfigCommand>) -> anyhow::Result<()> {
    let config_path = facelock_core::paths::config_path();

    match command.unwrap_or(ConfigCommand::Show) {
        ConfigCommand::Show => show_config(&config_path)?,
        ConfigCommand::Edit => {
            // DEC-6: display stays unprivileged (it reads a 0644 file), but
            // editing is root — must run before the editor ever opens (C6).
            crate::ipc_client::require_root("sudo facelock config edit")?;
            open_in_editor(&config_path)?;
        }
    }

    Ok(())
}

fn show_config(config_path: &std::path::Path) -> anyhow::Result<()> {
    println!("Config file: {}\n", config_path.display());

    if !config_path.exists() {
        println!("Config file does not exist.");
        println!("Run 'facelock setup' to create a default config.");
        return Ok(());
    }

    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;

    println!("{content}");

    // Also try to validate it
    match facelock_core::Config::load_from(config_path) {
        Ok(_) => println!("(config is valid)"),
        Err(e) => println!("Warning: config has errors: {e}"),
    }

    Ok(())
}

fn open_in_editor(config_path: &std::path::Path) -> anyhow::Result<()> {
    if !config_path.exists() {
        bail!(
            "Config file does not exist at {}. Run 'facelock setup' first.",
            config_path.display()
        );
    }

    // Snapshot config before editing to detect changes
    let old_config = facelock_core::Config::load_from(config_path).ok();

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| find_fallback_editor());

    let status = Command::new(&editor)
        .arg(config_path)
        .status()
        .with_context(|| format!("failed to launch editor '{editor}'"))?;

    if !status.success() {
        bail!("editor exited with status: {status}");
    }

    // Validate after editing
    let new_config = match facelock_core::Config::load_from(config_path) {
        Ok(c) => {
            println!("Config saved and validated successfully.");
            Some(c)
        }
        Err(e) => {
            println!("Warning: config has errors after editing: {e}");
            None
        }
    };

    // If both old and new configs are valid, check if daemon-relevant settings changed
    if let (Some(old), Some(new)) = (old_config, new_config)
        && needs_daemon_restart(&old, &new)
    {
        println!("Daemon-relevant settings changed. Restarting daemon...");
        crate::commands::daemon::restart_daemon();
    }

    Ok(())
}

/// Check if config changes require a daemon restart.
/// The daemon caches models, device config, and recognition settings at startup.
fn needs_daemon_restart(old: &facelock_core::Config, new: &facelock_core::Config) -> bool {
    // Model changes (require ONNX reload)
    old.recognition.detector_model != new.recognition.detector_model
        || old.recognition.embedder_model != new.recognition.embedder_model
        || old.recognition.execution_provider != new.recognition.execution_provider
        || old.recognition.threads != new.recognition.threads
        // Device changes
        || old.device.path != new.device.path
        || old.device.ir_emitter != new.device.ir_emitter
        || old.device.rotation != new.device.rotation
        // Recognition tuning
        || old.recognition.detection_confidence != new.recognition.detection_confidence
        || old.recognition.nms_threshold != new.recognition.nms_threshold
        || old.recognition.threshold != new.recognition.threshold
        || old.recognition.timeout_secs != new.recognition.timeout_secs
        // Security settings
        || old.security.require_ir != new.security.require_ir
        || old.security.min_auth_frames != new.security.min_auth_frames
        || old.security.require_frame_variance != new.security.require_frame_variance
        || old.security.require_landmark_liveness != new.security.require_landmark_liveness
        // Encryption changes
        || old.encryption.method != new.encryption.method
        || old.encryption.key_path != new.encryption.key_path
        // Database path change
        || old.storage.db_path != new.storage.db_path
        // Model directory change
        || old.daemon.model_dir != new.daemon.model_dir
}

fn find_fallback_editor() -> String {
    for editor in &["nano", "vi", "vim"] {
        if Command::new("which")
            .arg(editor)
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return (*editor).to_string();
        }
    }
    "nano".to_string()
}
