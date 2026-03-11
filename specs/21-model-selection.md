# Spec 21: Model Selection & Downloads

## Scope

Make model selection configurable. Fill in missing SHA256 hashes for optional models. Let `facelock setup` download whichever models are configured.

## Changes

### Config (`crates/facelock-core/src/config.rs`)

Add to `RecognitionConfig`:
```rust
#[serde(default = "default_detector_model")]
pub detector_model: String,  // default: "scrfd_2.5g_bnkps.onnx"

#[serde(default = "default_embedder_model")]
pub embedder_model: String,  // default: "w600k_r50.onnx"
```

### Face Engine (`crates/facelock-face/src/lib.rs`)

Replace hardcoded filenames in `FaceEngine::load()`:
```rust
let detector_path = model_dir.join(&config.detector_model);
let embedder_path = model_dir.join(&config.embedder_model);
```

### Model Manifest (`models/manifest.toml`)

Fill in SHA256 hashes for optional models from visomaster releases:
- `scrfd_10g_bnkps.onnx`
- `w600k_r100.onnx`

Add download URLs for all 4 models.

### CLI Setup (`crates/facelock-cli/src/commands/setup.rs`)

Update `facelock setup` to download whichever models are referenced by the current config's `detector_model` and `embedder_model`.

### Config template (`config/facelock.toml`)

Add commented-out model selection:
```toml
[recognition]
# detector_model = "scrfd_2.5g_bnkps.onnx"   # or "scrfd_10g_bnkps.onnx" for higher accuracy
# embedder_model = "w600k_r50.onnx"           # or "w600k_r100.onnx" for 99.77% LFW
```

## Acceptance

- Config selects model filenames
- `FaceEngine::load()` uses config, not hardcoded names
- All 4 models have SHA256 in manifest
- `facelock setup` downloads configured models
- Default config works unchanged
