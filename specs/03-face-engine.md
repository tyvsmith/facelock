# Spec 03: Face Engine (ONNX Inference)

**Phase**: 2 (Components) | **Crate**: howdy-face | **Depends on**: 01 | **Parallel with**: 02, 04

## Goal

Face detection (SCRFD), alignment (5-point landmarks), and embedding extraction (ArcFace) via ONNX Runtime. This is the ML core of the system.

## Dependencies

- `howdy-core` (for `Detection`, `BoundingBox`, `Point2D`, `FaceEmbedding`)
- `ort` 2.0+ (ONNX Runtime, with `download-binaries` feature)
- `ndarray` (tensor manipulation)
- `image` (resizing, format conversion)

## Modules

### `detector.rs` -- SCRFD Face Detection

```rust
pub struct FaceDetector {
    session: ort::Session,
    input_width: u32,    // 640
    input_height: u32,   // 640
    confidence_threshold: f32,
    nms_threshold: f32,
}

impl FaceDetector {
    pub fn load(model_path: &Path, confidence: f32, nms: f32) -> Result<Self>;
    pub fn detect(&self, frame: &Frame) -> Result<Vec<Detection>>;
}
```

**SCRFD preprocessing**:
1. Letterbox: resize to 640x640 preserving aspect ratio, pad with zeros
2. Normalize: `(pixel - 127.5) / 128.0`
3. Transpose to NCHW: `[1, 3, 640, 640]` float32

**SCRFD postprocessing**:
1. Model outputs per-stride (8, 16, 32): scores, bbox deltas, landmark deltas
2. Generate anchor grids for each stride
3. Decode bbox deltas: `center_x = anchor_x + delta_x * stride`, etc.
4. Decode landmark deltas similarly
5. Filter by confidence threshold
6. Apply NMS (Non-Maximum Suppression) with IoU threshold
7. Map coordinates back to original image space (undo letterbox)

**NMS algorithm**:
1. Sort detections by confidence (descending)
2. For each detection: compute IoU with all kept detections
3. Suppress if IoU > threshold with any kept detection

**IoU computation**:
```
intersection = max(0, min(x2_a, x2_b) - max(x1_a, x1_b)) * max(0, min(y2_a, y2_b) - max(y1_a, y1_b))
union = area_a + area_b - intersection
iou = intersection / union
```

### `align.rs` -- Face Alignment

```rust
/// Standard 112x112 reference landmarks (InsightFace canonical positions)
const REFERENCE_LANDMARKS: [[f32; 2]; 5] = [
    [38.2946, 51.6963],   // Left eye
    [73.5318, 51.5014],   // Right eye
    [56.0252, 71.7366],   // Nose tip
    [41.5493, 92.3655],   // Left mouth corner
    [70.7299, 92.2041],   // Right mouth corner
];

pub struct AlignedFace {
    pub rgb: Vec<u8>,  // 112 * 112 * 3
    pub width: u32,    // 112
    pub height: u32,   // 112
}

/// Compute similarity transform from detected landmarks to canonical landmarks
pub fn compute_affine_matrix(src: &[Point2D; 5]) -> [[f32; 3]; 2];

/// Apply affine warp to produce 112x112 aligned face
pub fn align_face(frame: &Frame, landmarks: &[Point2D; 5]) -> Result<AlignedFace>;
```

**Alignment algorithm** (Umeyama's method):
1. Compute centroids of source (detected) and destination (canonical) landmarks
2. Center both point sets
3. Compute covariance matrix: `H = src_centered^T * dst_centered / N`
4. SVD of 2x2 H matrix (analytical, no library needed)
5. Compute rotation: `R = V * D * U^T` (D ensures proper rotation, not reflection)
6. Compute scale from `trace(S * D) / variance(src_centered)`
7. Compute translation: `t = dst_mean - scale * R * src_mean`
8. Build 2x3 affine matrix
9. Warp source image via **inverse** transform with bilinear interpolation
10. Black padding for out-of-bounds pixels

### `embedder.rs` -- ArcFace Embedding

```rust
pub struct FaceEmbedder {
    session: ort::Session,
}

impl FaceEmbedder {
    pub fn load(model_path: &Path) -> Result<Self>;
    pub fn embed(&self, aligned: &AlignedFace) -> Result<FaceEmbedding>;
}
```

**ArcFace preprocessing**:
1. Take 112x112 aligned face RGB
2. Normalize: `(pixel - 127.5) / 127.5` (maps to [-1, 1])
3. Transpose to NCHW: `[1, 3, 112, 112]` float32

**ArcFace postprocessing**:
1. Extract output `[1, 512]` float32
2. L2 normalize: `v_i = v_i / sqrt(sum(v_j^2))`
3. Return `FaceEmbedding` ([f32; 512])

### `models.rs` -- Model Management

```rust
pub struct ModelManifest {
    pub models: HashMap<String, ModelEntry>,
}

pub struct ModelEntry {
    pub name: String,
    pub filename: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub category: String,  // "detection" or "recognition"
    pub default: bool,
}

/// Embedded manifest (compiled into binary)
pub fn load_manifest() -> ModelManifest;

/// Check if a model file exists and has correct checksum
pub fn verify_model(path: &Path, expected_sha256: &str) -> Result<bool>;
```

Model manifest embedded via `include_str!("../../models/manifest.toml")`.

### Full Pipeline

```rust
pub struct FaceEngine {
    detector: FaceDetector,
    embedder: FaceEmbedder,
}

impl FaceEngine {
    /// Load models with SHA256 integrity verification.
    /// Verifies each model file against the embedded manifest checksums
    /// BEFORE loading into ONNX Runtime. This prevents tampered models
    /// from being used for authentication.
    pub fn load(config: &RecognitionConfig, model_dir: &Path) -> Result<Self> {
        let manifest = load_manifest();
        for model in manifest.default_models() {
            let path = model_dir.join(&model.filename);
            if !verify_model(&path, &model.sha256)? {
                return Err(HowdyError::Detection(format!(
                    "Model integrity check failed for {}. Expected SHA256: {}. \
                     Re-run `howdy setup` to re-download.",
                    model.filename, model.sha256
                )));
            }
        }
        // ... load verified models into ONNX sessions
    }

    /// Run full pipeline: detect -> align -> embed
    pub fn process(&self, frame: &Frame) -> Result<Vec<(Detection, FaceEmbedding)>>;
}
```

**ONNX Session settings**: optimization level 3, 4 intra-op threads.

## Tests

- NMS: known overlapping boxes, verify correct suppression
- IoU: known box pairs with expected values
- Letterbox: verify scale/padding math with various aspect ratios
- Anchor generation: verify grid sizes for each stride
- Alignment: identity transform, known rotation/scale, bilinear interpolation
- L2 normalization: output norm = 1.0, zero vector handling
- ArcFace preprocessing: pixel value range [-1, 1]
- Full pipeline on test image: **#[ignore]** -- requires ONNX models
- Same-person similarity > 0.5, different-person < 0.3: **#[ignore]**

## Acceptance Criteria

1. FaceDetector loads ONNX model successfully
2. Detection finds faces with correct bounding boxes and landmarks
3. Alignment produces 112x112 normalized face images
4. FaceEmbedder produces 512-D L2-normalized vectors
5. Full pipeline: detect -> align -> embed works end-to-end
6. Model manifest parses correctly
7. All unit tests pass

## Verification

```bash
cargo test -p howdy-face
cargo clippy -p howdy-face
```
