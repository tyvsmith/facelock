use std::process::Command;

use anyhow::{Context, bail};

pub fn run(edit: bool) -> anyhow::Result<()> {
    let config_path = howdy_core::paths::config_path();

    if edit {
        open_in_editor(&config_path)?;
    } else {
        show_config(&config_path)?;
    }

    Ok(())
}

fn show_config(config_path: &std::path::Path) -> anyhow::Result<()> {
    println!("Config file: {}\n", config_path.display());

    if !config_path.exists() {
        println!("Config file does not exist.");
        println!("Run 'howdy setup' to create a default config.");
        return Ok(());
    }

    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;

    println!("{content}");

    // Also try to validate it
    match howdy_core::Config::load_from(config_path) {
        Ok(_) => println!("(config is valid)"),
        Err(e) => println!("Warning: config has errors: {e}"),
    }

    Ok(())
}

fn open_in_editor(config_path: &std::path::Path) -> anyhow::Result<()> {
    if !config_path.exists() {
        bail!(
            "Config file does not exist at {}. Run 'howdy setup' first.",
            config_path.display()
        );
    }

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
    match howdy_core::Config::load_from(config_path) {
        Ok(_) => println!("Config saved and validated successfully."),
        Err(e) => println!("Warning: config has errors after editing: {e}"),
    }

    Ok(())
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
