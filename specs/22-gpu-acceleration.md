# Spec 22: GPU/Acceleration Config

## Scope

Make ORT execution provider and thread count configurable. Feature-gate GPU providers.

## Changes

### Config (`crates/facelock-core/src/config.rs`)

Add to `RecognitionConfig`:
```rust
#[serde(default = "default_execution_provider")]
pub execution_provider: String,  // "cpu" (default), "cuda", "tensorrt", "coreml"

#[serde(default = "default_threads")]
pub threads: u32,  // default: 4
```

### Face Engine (`crates/facelock-face/src/detector.rs`, `embedder.rs`)

Pass `execution_provider` and `threads` to ORT session builder:
```rust
let mut builder = Session::builder()?
    .with_optimization_level(GraphOptimizationLevel::Level3)?
    .with_intra_threads(config.threads as usize)?;

match config.execution_provider.as_str() {
    "cuda" => { builder = builder.with_execution_providers([CUDAExecutionProvider::default().build()])?; }
    "tensorrt" => { builder = builder.with_execution_providers([TensorRTExecutionProvider::default().build()])?; }
    _ => {} // CPU is the default
}
```

### Cargo features (`crates/facelock-face/Cargo.toml`)

```toml
[features]
default = []
cuda = ["ort/cuda"]
tensorrt = ["ort/tensorrt"]
```

Propagate through `facelock-daemon` and `facelock-cli` Cargo.toml feature flags.

### Config template

```toml
[recognition]
# execution_provider = "cpu"  # "cpu", "cuda", "tensorrt"
# threads = 4
```

## Acceptance

- Default build is CPU-only (no new deps)
- `--features cuda` enables CUDA provider
- Config controls provider and thread count
- Invalid provider name logs warning, falls back to CPU
- Existing tests unaffected
