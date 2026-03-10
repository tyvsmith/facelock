use std::path::Path;

use anyhow::Result;
use visage_core::config::Config;

use crate::commands::TpmCommand;

pub fn run(command: TpmCommand) -> Result<()> {
    match command {
        TpmCommand::Status => status(),
    }
}

fn status() -> Result<()> {
    let config = Config::load()?;

    // Extract device path from TCTI string (e.g., "device:/dev/tpmrm0" -> "/dev/tpmrm0")
    let device_path = config
        .tpm
        .tcti
        .strip_prefix("device:")
        .unwrap_or(&config.tpm.tcti);

    let device_exists = Path::new(device_path).exists();

    println!("TPM Status");
    println!("──────────");
    println!(
        "  TPM device ({}): {}",
        device_path,
        if device_exists { "found" } else { "not found" }
    );
    println!(
        "  seal_database: {}",
        if config.tpm.seal_database {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  pcr_binding:   {}",
        if config.tpm.pcr_binding {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!();
    println!("  Implementation: stubs only (not yet functional)");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_device_path_from_tcti() {
        let tcti = "device:/dev/tpmrm0";
        let path = tcti.strip_prefix("device:").unwrap_or(tcti);
        assert_eq!(path, "/dev/tpmrm0");
    }

    #[test]
    fn extract_device_path_without_prefix() {
        let tcti = "/dev/tpmrm0";
        let path = tcti.strip_prefix("device:").unwrap_or(tcti);
        assert_eq!(path, "/dev/tpmrm0");
    }
}
