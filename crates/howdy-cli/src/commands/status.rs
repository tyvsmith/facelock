use std::path::Path;

use howdy_core::ipc::{DaemonRequest, DaemonResponse};
use howdy_core::Config;

use crate::ipc_client;

pub fn run() -> anyhow::Result<()> {
    println!("howdy system status\n");

    // 1. Config
    check_config();

    // 2. Daemon
    let config = check_daemon();

    // 3. Camera
    check_camera(&config);

    // 4. Models
    check_models(&config);

    // 5. PAM
    check_pam();

    Ok(())
}

fn check_config() {
    let config_path = howdy_core::paths::config_path();
    print_status_item("Config file", &config_path.display().to_string());

    if !config_path.exists() {
        print_result(false, "not found");
        return;
    }

    match Config::load_from(&config_path) {
        Ok(config) => {
            print_result(true, "valid");
            print_detail("device.path", &config.device.path);
        }
        Err(e) => {
            print_result(false, &format!("invalid: {e}"));
        }
    }
}

fn check_daemon() -> Option<Config> {
    let config = match Config::load() {
        Ok(c) => c,
        Err(_) => {
            print_status_item("Daemon", "");
            print_result(false, "config not loaded, cannot check daemon");
            return None;
        }
    };

    let socket_path = &config.daemon.socket_path;
    print_status_item("Daemon socket", socket_path);

    if !Path::new(socket_path).exists() {
        print_result(false, "socket not found");
        return Some(config);
    }

    // Try to ping the daemon
    let request = DaemonRequest::Ping;
    match ipc_client::send_request(socket_path, &request) {
        Ok(DaemonResponse::Ok) => {
            print_result(true, "responding");
        }
        Ok(_) => {
            print_result(true, "connected (unexpected response)");
        }
        Err(e) => {
            print_result(false, &format!("not responding: {e}"));
        }
    }

    Some(config)
}

fn check_camera(config: &Option<Config>) {
    let Some(config) = config else {
        print_status_item("Camera", "");
        print_result(false, "config not available");
        return;
    };

    let device_path = &config.device.path;
    print_status_item("Camera device", device_path);

    if Path::new(device_path).exists() {
        print_result(true, "device exists");
    } else {
        print_result(false, "device not found");
    }
}

fn check_models(config: &Option<Config>) {
    let Some(config) = config else {
        print_status_item("Models", "");
        print_result(false, "config not available");
        return;
    };

    let model_dir = &config.daemon.model_dir;
    print_status_item("Model directory", model_dir);

    if !Path::new(model_dir).exists() {
        print_result(false, "directory not found");
        return;
    }

    // Check for required model files
    let required_files = ["scrfd_2.5g_bnkps.onnx", "w600k_r50.onnx"];
    let mut all_present = true;

    for filename in &required_files {
        let path = Path::new(model_dir).join(filename);
        if path.exists() {
            print_detail(filename, "present");
        } else {
            print_detail(filename, "MISSING");
            all_present = false;
        }
    }

    if all_present {
        print_result(true, "all required models present");
    } else {
        print_result(false, "some models missing (run 'howdy setup')");
    }
}

fn check_pam() {
    let pam_path = "/lib/security/pam_howdy.so";
    print_status_item("PAM module", pam_path);

    if Path::new(pam_path).exists() {
        print_result(true, "installed");
    } else {
        // Also check /usr/lib path
        let alt_path = "/usr/lib/security/pam_howdy.so";
        if Path::new(alt_path).exists() {
            print_result(true, &format!("installed at {alt_path}"));
        } else {
            print_result(false, "not installed");
        }
    }

    // Check if sudo is configured
    let sudo_pam = "/etc/pam.d/sudo";
    if Path::new(sudo_pam).exists() {
        if let Ok(content) = std::fs::read_to_string(sudo_pam) {
            if content.contains("pam_howdy") {
                print_detail("sudo PAM", "configured");
            } else {
                print_detail("sudo PAM", "not configured for howdy");
            }
        }
    }
}

fn print_status_item(label: &str, value: &str) {
    if value.is_empty() {
        println!("  {label}:");
    } else {
        println!("  {label}: {value}");
    }
}

fn print_result(ok: bool, message: &str) {
    let indicator = if ok { "[ok]" } else { "[!!]" };
    println!("    {indicator} {message}");
}

fn print_detail(key: &str, value: &str) {
    println!("    - {key}: {value}");
}
