use anyhow::Context;
use chrono::{Local, TimeZone};

use facelock_core::Config;
use facelock_core::ipc::{DaemonRequest, DaemonResponse};
use facelock_core::types::FaceModelInfo;

use crate::ipc_client;

pub fn run(user: Option<String>, json: bool) -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    let user = ipc_client::resolve_user(user.as_deref());

    let models = fetch_models(&config, &user)?;

    if json {
        print_json(&models);
    } else {
        print_table(&user, &models);
    }

    Ok(())
}

fn fetch_models(config: &Config, user: &str) -> anyhow::Result<Vec<FaceModelInfo>> {
    // Try D-Bus first (works without root)
    if !ipc_client::should_use_direct(config) {
        let request = DaemonRequest::ListModels {
            user: user.to_string(),
        };
        let response = ipc_client::send_request(&request)?;
        return match response {
            DaemonResponse::Models(models) => Ok(models),
            other => anyhow::bail!("unexpected response from daemon: {other:?}"),
        };
    }

    // Direct mode: needs read access to DB (typically root or facelock group)
    match crate::direct::open_store(config) {
        Ok(store) => store.list_models(user).map_err(|e| anyhow::anyhow!("{e}")),
        Err(_) => {
            // DB not accessible — prompt for root
            ipc_client::require_root("sudo facelock list")?;
            let store = crate::direct::open_store(config)?;
            store.list_models(user).map_err(|e| anyhow::anyhow!("{e}"))
        }
    }
}

fn print_table(user: &str, models: &[FaceModelInfo]) {
    if models.is_empty() {
        println!("No face models enrolled for user '{user}'.");
        return;
    }

    println!("Face models for user '{user}':\n");
    println!("  {:<6} {:<20} Created", "ID", "Label");
    println!("  {}", "-".repeat(50));

    for model in models {
        let created = format_timestamp(model.created_at);
        println!("  {:<6} {:<20} {}", model.id, model.label, created);
    }

    println!("\n  Total: {} model(s)", models.len());
}

fn print_json(models: &[FaceModelInfo]) {
    println!("[");
    for (i, model) in models.iter().enumerate() {
        let comma = if i + 1 < models.len() { "," } else { "" };
        println!(
            "  {{\"id\": {}, \"label\": \"{}\", \"user\": \"{}\", \"created_at\": {}}}{}",
            model.id, model.label, model.user, model.created_at, comma
        );
    }
    println!("]");
}

fn format_timestamp(unix_ts: u64) -> String {
    match Local.timestamp_opt(unix_ts as i64, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        _ => unix_ts.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_valid() {
        let formatted = format_timestamp(1700000000);
        assert!(formatted.contains("2023"), "expected 2023 in {formatted}");
    }

    #[test]
    fn format_timestamp_zero() {
        let formatted = format_timestamp(0);
        assert!(
            formatted.contains("1970") || formatted.contains("1969"),
            "expected 1970 or 1969 (timezone-dependent) in {formatted}"
        );
    }
}
