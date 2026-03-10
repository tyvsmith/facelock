# Spec 02: Camera Capture

**Phase**: 2 (Components) | **Crate**: visage-camera | **Depends on**: 01 | **Parallel with**: 03, 04

## Goal

V4L2 camera capture with format conversion, preprocessing, and IR normalization. Produces `Frame` structs usable by the face engine.

## Dependencies

- `visage-core` (for `Frame`, `VisageError`)
- `v4l` 0.14+ (V4L2 bindings)
- `image` (JPEG decoding, resize)

## Modules

### `device.rs` -- Device Discovery

```rust
pub struct DeviceInfo {
    pub path: String,        // e.g. "/dev/video2"
    pub name: String,        // e.g. "Infrared Camera"
    pub driver: String,
    pub capabilities: Vec<String>,
    pub formats: Vec<FormatInfo>,
}

pub struct FormatInfo {
    pub fourcc: String,
    pub description: String,
    pub sizes: Vec<(u32, u32)>,  // (width, height) pairs
}

/// List all V4L2 video capture devices
pub fn list_devices() -> Result<Vec<DeviceInfo>>;

/// Validate a specific device path
pub fn validate_device(path: &str) -> Result<DeviceInfo>;
```

### `capture.rs` -- Camera Capture

```rust
pub struct Camera {
    stream: v4l::io::mmap::Stream,
    width: u32,
    height: u32,
    format: FourCC,
    dark_threshold: f32,
    rotation: u16,
}

impl Camera {
    /// Open camera with config settings
    pub fn open(config: &DeviceConfig) -> Result<Self>;

    /// Capture a single frame (blocking)
    pub fn capture(&mut self) -> Result<Frame>;

    /// Check if frame is too dark (% pixels < 10 exceeds threshold)
    pub fn is_dark(frame: &Frame) -> bool;
}
```

**Open sequence**:
1. Open V4L2 device at `config.path`
2. Query capabilities, verify VIDEO_CAPTURE
3. Select format: prefer GREY > YUYV > MJPG > any available
4. Set resolution: use `config.max_height`, maintain aspect ratio, cap at 640x480
5. Create MMAP stream (4 buffers)

**Per-frame sequence**:
1. Dequeue buffer from MMAP stream
2. Convert to RGB based on format:
   - GREY: replicate channel 3x for RGB
   - YUYV: manual YUV->RGB conversion per 2-pixel pair
   - MJPG: decode with `image::codecs::jpeg::JpegDecoder`
3. Downscale if height > max_height (Triangle filter)
4. Apply rotation if configured (0/90/180/270)
5. Convert to grayscale: `0.299*R + 0.587*G + 0.114*B`
6. Apply CLAHE to grayscale channel
7. Check darkness threshold
8. Return `Frame { rgb, gray, width, height }`

### `preprocess.rs` -- Image Preprocessing

```rust
/// CLAHE: Contrast Limited Adaptive Histogram Equalization
/// Improves IR camera image quality
pub fn clahe(gray: &[u8], width: u32, height: u32) -> Vec<u8>;

/// YUV (YUYV) to RGB conversion
pub fn yuyv_to_rgb(data: &[u8], width: u32, height: u32) -> Vec<u8>;

/// Compute grayscale from RGB
pub fn rgb_to_gray(rgb: &[u8], width: u32, height: u32) -> Vec<u8>;
```

**CLAHE algorithm**:
1. Divide image into 8x8 tile grid
2. For each tile: compute histogram (256 bins)
3. Clip histogram at limit (2.0 * average), redistribute clipped counts
4. Build CDF per tile
5. For each pixel: bilinear interpolate between surrounding tile CDFs

### IR Camera Detection

The camera module must expose whether a device is likely an IR camera, used by the daemon for anti-spoofing enforcement (`security.require_ir`):

```rust
/// Heuristic: is this likely an IR camera?
/// Checks: native GREY/Y16 format support AND/OR device name contains "ir"/"infrared"
pub fn is_ir_camera(device: &DeviceInfo) -> bool {
    let name_lower = device.name.to_lowercase();
    let has_ir_name = name_lower.contains("ir") || name_lower.contains("infrared");
    let has_ir_format = device.formats.iter().any(|f| {
        matches!(f.fourcc.as_str(), "GREY" | "Y16 ")
    });
    has_ir_name || has_ir_format
}
```

This is a heuristic. The `visage devices` command should display the IR detection result for each camera so users can verify.

### IR Texture Validation

For anti-spoofing, compute texture variance within a face bounding box region:

```rust
/// Check that face region has sufficient IR texture (real skin vs flat photo/screen)
/// Returns true if texture is consistent with a real face
pub fn check_ir_texture(gray: &[u8], bbox: &BoundingBox, width: u32) -> bool {
    let face_pixels = extract_bbox_region(gray, bbox, width);
    let mean = face_pixels.iter().map(|&p| p as f32).sum::<f32>() / face_pixels.len() as f32;
    let variance = face_pixels.iter()
        .map(|&p| (p as f32 - mean).powi(2))
        .sum::<f32>() / face_pixels.len() as f32;
    let std_dev = variance.sqrt();
    // Real IR faces: std_dev > ~15. Flat surfaces (photos/screens): std_dev < 5
    std_dev > 10.0
}
```

## Tests

- `yuyv_to_rgb`: known input/output pairs
- `rgb_to_gray`: known luminance values
- `clahe`: synthetic uniform image should remain ~uniform, dark image should brighten
- `is_dark`: all-black frame = dark, all-white = not dark, threshold boundary
- `is_ir_camera`: device with GREY format = true, device with only MJPG = false
- `check_ir_texture`: uniform image (simulating photo) = false, varied image = true
- `list_devices`: returns without crashing (may be empty)
- `Camera::open` + `capture`: **#[ignore]** -- requires real camera

## Acceptance Criteria

1. `list_devices()` returns available cameras without crashing
2. Frame capture works with GREY, YUYV, and MJPG formats
3. CLAHE produces visually better frames for IR input
4. Graceful error on missing/inaccessible device
5. Dark frame detection works correctly
6. All unit tests pass

## Verification

```bash
cargo test -p visage-camera
cargo clippy -p visage-camera
```
