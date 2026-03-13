use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use facelock_core::config::Config;

pub fn run(tail: bool, lines: usize) -> Result<()> {
    let config = Config::load()?;

    if !config.audit.enabled {
        println!("Audit logging is not enabled.");
        println!("Enable it in config:");
        println!("  [audit]");
        println!("  enabled = true");
        return Ok(());
    }

    let path = Path::new(&config.audit.path);
    if !path.exists() {
        println!("No audit log found at {}", path.display());
        return Ok(());
    }

    if tail {
        // Follow mode: print last N lines, then watch for new lines
        print_last_n_lines(path, lines)?;

        println!("--- watching for new entries (Ctrl+C to stop) ---");
        // Simple tail -f: re-read periodically
        let mut last_size = std::fs::metadata(path)?.len();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let current_size = std::fs::metadata(path)
                .map(|m| m.len())
                .unwrap_or(last_size);
            if current_size > last_size {
                let file = std::fs::File::open(path)?;
                let reader = BufReader::new(file);
                let mut pos = 0u64;
                for line in reader.lines() {
                    let line = line?;
                    pos += line.len() as u64 + 1;
                    if pos > last_size {
                        println!("{line}");
                    }
                }
                last_size = current_size;
            }
        }
    } else {
        print_last_n_lines(path, lines)?;
    }

    Ok(())
}

fn print_last_n_lines(path: &Path, n: usize) -> Result<()> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;

    let start = if all_lines.len() > n {
        all_lines.len() - n
    } else {
        0
    };

    for line in &all_lines[start..] {
        println!("{line}");
    }

    Ok(())
}
