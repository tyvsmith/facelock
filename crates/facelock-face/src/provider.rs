use ort::session::builder::SessionBuilder;

/// Paths to search for the system-installed libonnxruntime.so.
/// The onnxruntime-opt-cuda package on Arch installs here.
#[cfg(any(feature = "cuda", feature = "tensorrt"))]
const SYSTEM_ORT_PATHS: &[&str] = &[
    "/usr/lib/libonnxruntime.so",
    "/usr/lib64/libonnxruntime.so",
    "/usr/local/lib/libonnxruntime.so",
];

/// Load the system-installed ONNX Runtime shared library.
///
/// With `load-dynamic`, ORT is not statically linked. We must load
/// `libonnxruntime.so` from the system before creating any sessions.
/// The system ORT (e.g. `onnxruntime-opt-cuda`) is built against the
/// locally installed CUDA version, so GPU EPs work correctly.
#[cfg(any(feature = "cuda", feature = "tensorrt"))]
fn load_system_ort() -> std::result::Result<(), String> {
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

        // Search standard system paths
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

        init_err = Some(
            "Could not find system libonnxruntime.so. \
             Install with: sudo pacman -S onnxruntime-opt-cuda"
                .into(),
        );
    });

    if let Some(e) = init_err {
        Err(e)
    } else {
        Ok(())
    }
}

/// Register an execution provider on the session builder based on the provider name.
///
/// For `"cuda"` and `"tensorrt"`, loads the system ONNX Runtime (which has GPU
/// support built in) and registers the corresponding execution provider.
/// Falls back to CPU with a warning if GPU is unavailable.
pub(crate) fn register_execution_provider(
    builder: SessionBuilder,
    provider: &str,
) -> std::result::Result<SessionBuilder, String> {
    match provider {
        "cpu" => Ok(builder),

        #[cfg(feature = "cuda")]
        "cuda" => {
            load_system_ort()?;
            builder
                .with_execution_providers([ort::ep::CUDA::default().build()])
                .map_err(|e| format!("CUDA execution provider: {e}"))
        }

        #[cfg(not(feature = "cuda"))]
        "cuda" => Err(
            "CUDA execution provider requested but facelock was not built with --features cuda. \
             Rebuild with: just build-cuda"
                .into(),
        ),

        #[cfg(feature = "tensorrt")]
        "tensorrt" => {
            load_system_ort()?;
            builder
                .with_execution_providers([
                    ort::ep::TensorRT::default().build(),
                    ort::ep::CUDA::default().build(), // fallback
                ])
                .map_err(|e| format!("TensorRT execution provider: {e}"))
        }

        #[cfg(not(feature = "tensorrt"))]
        "tensorrt" => Err(
            "TensorRT execution provider requested but facelock was not built with --features tensorrt. \
             Rebuild with: cargo build --workspace --features tensorrt"
                .into(),
        ),

        other => Err(format!(
            "Unknown execution provider '{other}'. Valid values: cpu, cuda, tensorrt"
        )),
    }
}
