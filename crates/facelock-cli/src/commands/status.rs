use std::path::Path;

use facelock_core::Config;
use facelock_core::config::EncryptionMethod;
use facelock_core::ipc::{DaemonRequest, DaemonResponse};

use crate::ipc_client;

pub fn run() -> anyhow::Result<()> {
    println!("facelock system status\n");

    // 1. Config
    check_config();

    // 2. Daemon
    let config = check_daemon();

    // 3. Camera
    check_camera(&config);

    // 4. Models
    check_models(&config);

    // 5. Inference
    check_inference(&config);

    // 6. Encryption
    check_encryption(&config);

    // 7. Enrolled faces
    check_enrolled(&config);

    // 8. Security
    check_security(&config);

    // 9. Notifications
    check_notifications(&config);

    // 10. PAM
    check_pam();

    Ok(())
}

fn check_config() {
    let config_path = facelock_core::paths::config_path();
    print_status_item("Config file", &config_path.display().to_string());

    if !config_path.exists() {
        print_result(false, "not found");
        return;
    }

    match Config::load_from(&config_path) {
        Ok(config) => {
            print_result(true, "valid");
            print_detail(
                "device.path",
                config.device.path.as_deref().unwrap_or("(auto-detect)"),
            );
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

    print_status_item("Daemon", "org.facelock.Daemon (D-Bus system bus)");

    if config.daemon.mode == facelock_core::config::DaemonMode::Oneshot {
        print_result(true, "oneshot mode (no daemon)");
        return Some(config);
    }

    // Try to ping — this may trigger D-Bus activation if the daemon
    // isn't running yet but activation is configured.
    let request = DaemonRequest::Ping;
    match ipc_client::send_request(&request) {
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

    let device_path = config.device.path.as_deref().unwrap_or("(auto-detect)");
    print_status_item("Camera device", device_path);

    match &config.device.path {
        Some(p) if Path::new(p).exists() => {
            print_result(true, "device exists");
        }
        Some(_) => {
            print_result(false, "device not found");
        }
        None => {
            print_result(true, "auto-detect enabled");
        }
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

    // Check for the configured model files (not just defaults)
    let models = [
        ("detector", config.recognition.detector_model.as_str()),
        ("embedder", config.recognition.embedder_model.as_str()),
    ];

    let mut all_present = true;
    for (purpose, filename) in &models {
        let path = Path::new(model_dir).join(filename);
        if path.exists() {
            print_detail(&format!("{purpose} ({filename})"), "present");
        } else {
            print_detail(&format!("{purpose} ({filename})"), "MISSING");
            all_present = false;
        }
    }

    if all_present {
        print_result(true, "all configured models present");
    } else {
        print_result(false, "some models missing (run 'facelock setup')");
    }
}

fn check_inference(config: &Option<Config>) {
    let Some(config) = config else {
        return;
    };

    let provider = &config.recognition.execution_provider;
    let label = match provider.as_str() {
        "cpu" => "CPU",
        "cuda" => "CUDA (NVIDIA GPU)",
        "rocm" => "ROCm (AMD GPU)",
        "openvino" => "OpenVINO (Intel)",
        other => other,
    };
    print_status_item("Execution provider", label);

    // Check that the ORT library is loadable
    let ort_paths = [
        "/usr/lib/libonnxruntime.so",
        "/usr/lib64/libonnxruntime.so",
        "/usr/lib/facelock/libonnxruntime.so",
    ];
    if let Some(path) = ort_paths.iter().find(|p| Path::new(p).exists()) {
        print_result(true, &format!("ONNX Runtime found at {path}"));
    } else {
        print_result(false, "ONNX Runtime not found (install onnxruntime)");
    }
}

fn check_encryption(config: &Option<Config>) {
    let Some(config) = config else {
        return;
    };

    let method_str = match config.encryption.method {
        EncryptionMethod::Tpm => "AES-256-GCM (TPM-sealed key)",
        EncryptionMethod::Keyfile => "AES-256-GCM (keyfile)",
        EncryptionMethod::None => "none",
    };
    print_status_item("Encryption", method_str);

    match config.encryption.method {
        EncryptionMethod::Tpm => {
            let sealed_exists = Path::new(&config.encryption.sealed_key_path).exists();
            let device_path = config
                .tpm
                .tcti
                .strip_prefix("device:")
                .unwrap_or(&config.tpm.tcti);
            let device_exists = Path::new(device_path).exists();
            if sealed_exists && device_exists {
                print_result(
                    true,
                    &format!("sealed key: {}", config.encryption.sealed_key_path),
                );
            } else if !sealed_exists {
                print_result(
                    false,
                    &format!("sealed key missing: {}", config.encryption.sealed_key_path),
                );
            } else {
                print_result(false, &format!("TPM device missing: {}", device_path));
            }
        }
        EncryptionMethod::Keyfile => {
            let key_exists = Path::new(&config.encryption.key_path).exists();
            if key_exists {
                print_result(true, &format!("key file: {}", config.encryption.key_path));
            } else {
                print_result(
                    false,
                    &format!("key file missing: {}", config.encryption.key_path),
                );
            }
        }
        EncryptionMethod::None => {
            print_result(
                false,
                "embeddings stored as plaintext (run 'facelock setup' to enable encryption)",
            );
        }
    }

    // Show DB encryption stats if readable
    if let Ok(store) = facelock_store::FaceStore::open_readonly(Path::new(&config.storage.db_path))
    {
        if let Ok((sealed, unsealed)) = store.count_sealed() {
            if sealed + unsealed > 0 {
                print_detail("encrypted", &sealed.to_string());
                print_detail("plaintext", &unsealed.to_string());
            }
        }
    }
}

fn check_enrolled(config: &Option<Config>) {
    let Some(config) = config else {
        return;
    };

    let user = std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown".into());

    print_status_item("Enrolled faces", &user);

    match facelock_store::FaceStore::open_readonly(Path::new(&config.storage.db_path)) {
        Ok(store) => match store.list_models(&user) {
            Ok(models) if models.is_empty() => {
                print_result(false, "no faces enrolled (run 'facelock enroll')");
            }
            Ok(models) => {
                print_result(true, &format!("{} model(s)", models.len()));
                for m in &models {
                    print_detail(&format!("#{}", m.id), &m.label);
                }
            }
            Err(e) => {
                print_result(false, &format!("error reading models: {e}"));
            }
        },
        Err(_) => {
            print_detail("database", "not accessible (may need root)");
        }
    }
}

fn check_security(config: &Option<Config>) {
    let Some(config) = config else {
        return;
    };

    print_status_item("Security", "");
    if config.security.disabled {
        print_result(false, "ALL SECURITY CHECKS DISABLED");
        return;
    }
    print_detail("require_ir", if config.security.require_ir { "yes" } else { "no" });
    print_detail(
        "liveness (frame variance)",
        if config.security.require_frame_variance { "yes" } else { "no" },
    );
    print_detail(
        "liveness (landmark movement)",
        if config.security.require_landmark_liveness { "yes" } else { "no" },
    );
    print_detail(
        "min_auth_frames",
        &config.security.min_auth_frames.to_string(),
    );
}

fn check_notifications(config: &Option<Config>) {
    let Some(config) = config else {
        return;
    };

    let mode_str = match config.notification.mode {
        facelock_core::config::NotificationMode::Off => "off",
        facelock_core::config::NotificationMode::Terminal => "terminal",
        facelock_core::config::NotificationMode::Desktop => "desktop",
        facelock_core::config::NotificationMode::Both => "terminal + desktop",
    };
    print_status_item("Notifications", mode_str);
    if config.notification.mode != facelock_core::config::NotificationMode::Off {
        print_detail("prompt", if config.notification.notify_prompt { "yes" } else { "no" });
        print_detail("on success", if config.notification.notify_on_success { "yes" } else { "no" });
        print_detail("on failure", if config.notification.notify_on_failure { "yes" } else { "no" });
    }
}

fn check_pam() {
    let pam_path = "/lib/security/pam_facelock.so";
    print_status_item("PAM module", pam_path);

    if Path::new(pam_path).exists() {
        print_result(true, "installed");
    } else {
        // Also check /usr/lib path
        let alt_path = "/usr/lib/security/pam_facelock.so";
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
            if content.contains("pam_facelock") {
                print_detail("sudo PAM", "configured");
            } else {
                print_detail("sudo PAM", "not configured for facelock");
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
