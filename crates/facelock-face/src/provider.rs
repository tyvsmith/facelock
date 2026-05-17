use ort::session::builder::SessionBuilder;

/// Paths to search for a system-installed libonnxruntime.so.
/// A GPU-enabled system ORT (e.g. onnxruntime-opt-cuda) is preferred
/// so that GPU execution providers work. The bundled CPU-only ORT
/// is used as a last resort.
const SYSTEM_ORT_PATHS: &[&str] = &[
    "/usr/lib/libonnxruntime.so",
    "/usr/lib64/libonnxruntime.so",
    "/usr/local/lib/libonnxruntime.so",
];

/// Bundled CPU-only ORT fallback, shipped with the facelock package.
/// Debian/Ubuntu use /usr/lib/, Fedora/RHEL use /usr/lib64/ on x86_64.
const BUNDLED_ORT_PATHS: &[&str] = &[
    "/usr/lib/facelock/libonnxruntime.so",
    "/usr/lib64/facelock/libonnxruntime.so",
];

/// Load the ONNX Runtime shared library.
///
/// Search order:
/// 1. `ORT_DYLIB_PATH` env var (explicit override)
/// 2. System paths (may have GPU support)
/// 3. Bundled CPU-only fallback
fn load_ort() -> std::result::Result<(), String> {
    use std::sync::Once;

    static INIT: Once = Once::new();
    let mut init_err: Option<String> = None;

    INIT.call_once(|| {
        // Check ORT_DYLIB_PATH env var first
        if let Ok(path) = std::env::var("ORT_DYLIB_PATH") {
            if std::path::Path::new(&path).exists() {
                if let Err(e) = ort::init_from(std::path::Path::new(&path)) {
                    init_err = Some(format!(
                        "Failed to load ORT from ORT_DYLIB_PATH={path}: {e}"
                    ));
                }
                return;
            }
        }

        // Search system paths (may have GPU support)
        for path_str in SYSTEM_ORT_PATHS {
            let path = std::path::Path::new(path_str);
            if path.exists() {
                match ort::init_from(path) {
                    Ok(_) => {
                        tracing::info!("Loaded system ONNX Runtime from {path_str}");
                        return;
                    }
                    Err(e) => {
                        tracing::warn!("Found {path_str} but failed to load: {e}");
                    }
                }
            }
        }

        // Fall back to bundled CPU-only ORT
        for bundled_path in BUNDLED_ORT_PATHS {
            let bundled = std::path::Path::new(bundled_path);
            if bundled.exists() {
                match ort::init_from(bundled) {
                    Ok(_) => {
                        tracing::info!("Loaded bundled CPU ONNX Runtime from {bundled_path}");
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Found bundled ORT at {bundled_path} but failed to load: {e}"
                        );
                    }
                }
            }
        }

        init_err = Some(
            "Could not find libonnxruntime.so. \
             Ensure the facelock package is installed correctly, \
             or set ORT_DYLIB_PATH to point to a compatible libonnxruntime.so"
                .into(),
        );
    });

    if let Some(e) = init_err {
        Err(e)
    } else {
        Ok(())
    }
}

/// Ensure the ONNX Runtime shared library is loaded before any session builders
/// are created. Some ORT builds can deadlock if session construction re-enters
/// runtime initialization.
pub(crate) fn ensure_runtime_loaded() -> std::result::Result<(), String> {
    load_ort()
}

/// Register an execution provider on the session builder based on config.
///
/// All providers load the ONNX Runtime shared library at runtime.
/// GPU providers (cuda, rocm, openvino) require a system ORT built
/// with the corresponding support — install the appropriate package
/// (e.g. `onnxruntime-opt-cuda`) and it will be picked up automatically.
pub(crate) fn register_execution_provider(
    builder: SessionBuilder,
    provider: &str,
) -> std::result::Result<SessionBuilder, String> {
    load_ort()?;
    tracing::info!("Using execution provider: {provider}");
    match provider {
        "cpu" => Ok(builder),

        "cuda" => builder
            .with_execution_providers([ort::ep::CUDA::default().build()])
            .map_err(|e| format!("CUDA execution provider: {e}")),

        "rocm" => builder
            .with_execution_providers([ort::ep::ROCm::default().build()])
            .map_err(|e| format!("ROCm execution provider: {e}")),

        "openvino" => builder
            .with_execution_providers([ort::ep::OpenVINO::default().build()])
            .map_err(|e| format!("OpenVINO execution provider: {e}")),

        other => Err(format!(
            "Unknown execution provider '{other}'. Valid values: cpu, cuda, rocm, openvino"
        )),
    }
}
