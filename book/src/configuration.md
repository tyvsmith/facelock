# Configuration Reference

Facelock reads its configuration from `/etc/facelock/config.toml`. Override the path with the `FACELOCK_CONFIG` environment variable.

All settings are optional. Facelock auto-detects the camera and uses sensible defaults. The annotated config file at `config/facelock.toml` in the repository serves as the canonical example.

## [device]

Camera settings.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `path` | string (optional) | Auto-detect | Camera device path (e.g., `/dev/video2`). When omitted, Facelock auto-detects the best available camera, preferring IR over RGB. |
| `max_height` | u32 | `480` | Maximum frame height in pixels. Frames taller than this are downscaled to improve processing speed. |
| `rotation` | u16 | `0` | Rotate captured frames. Values: `0`, `90`, `180`, `270`. Useful for cameras mounted sideways. |

## [recognition]

Face detection and embedding parameters.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `threshold` | f32 | `0.80` | Cosine similarity threshold for accepting a face match. Must be between 0.0 and 1.0. Higher values are stricter. See the range guide below. |
| `timeout_secs` | u32 | `5` | Maximum seconds to attempt recognition before giving up. Must be > 0. |
| `detection_confidence` | f32 | `0.5` | Minimum confidence for the face detector to report a detection. Lower values detect more faces but increase false positives. |
| `nms_threshold` | f32 | `0.4` | Non-maximum suppression threshold for overlapping detections. |
| `detector_model` | string | `"scrfd_2.5g_bnkps.onnx"` | ONNX detector model filename. Must exist in `daemon.model_dir`. |
| `embedder_model` | string | `"w600k_r50.onnx"` | ONNX embedder model filename. Must exist in `daemon.model_dir`. |
| `execution_provider` | string | `"cpu"` | ONNX Runtime execution provider. Values: `"cpu"`, `"cuda"`, `"tensorrt"`. GPU providers require building with `--features cuda` or `--features tensorrt`. |
| `threads` | u32 | `4` | Number of CPU threads for ONNX inference. |

### Threshold range guide (ArcFace cosine similarity)

| Range | Description |
|-------|-------------|
| 0.30 -- 0.50 | Very loose -- high false accept rate, not recommended |
| 0.50 -- 0.65 | Loose -- convenient but may accept similar-looking people |
| 0.65 -- 0.80 | Balanced -- good for most setups, low false accept rate |
| 0.80 -- 0.90 | Strict -- rarely accepts wrong person, may reject on bad angles |
| 0.90+ | Very strict -- may require near-ideal lighting and pose |

Run `facelock test` to see your similarity scores, then set the threshold below your typical match score with some margin.

### Model options

| Preset | Detector | Embedder | Total size | Notes |
|--------|----------|----------|------------|-------|
| Default | `scrfd_2.5g_bnkps.onnx` (3MB) | `w600k_r50.onnx` (166MB) | ~170MB | Good accuracy, fast |
| High-accuracy | `det_10g.onnx` (17MB) | `glintr100.onnx` (249MB) | ~266MB | ~200ms slower |

Run `facelock setup` after changing models to download them.

## [daemon]

Controls how the PAM module reaches the face engine.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `mode` | string | `"daemon"` | `"daemon"` connects to a persistent daemon via D-Bus system bus (fast, ~200ms). `"oneshot"` spawns `facelock auth` per PAM call (slower, ~700ms, no background process). |
| `model_dir` | string | `"/var/lib/facelock/models"` | Directory containing ONNX model files. |
| `idle_timeout_secs` | u64 | `0` | Shut down the daemon after this many idle seconds. `0` means never. Useful with D-Bus activation. |

## [storage]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `db_path` | string | `"/var/lib/facelock/facelock.db"` | SQLite database for face embeddings. File permissions should be 640, owned by `root:facelock`. |

## [security]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `disabled` | bool | `false` | Disable face authentication entirely. PAM returns IGNORE, falling through to the next auth method. |
| `abort_if_ssh` | bool | `true` | Refuse face auth when connected via SSH (no camera available). |
| `abort_if_lid_closed` | bool | `true` | Refuse face auth when the laptop lid is closed (camera blocked). |
| `require_ir` | bool | `true` | Require an IR camera for authentication. RGB cameras are trivially spoofed with a printed photo. Only set to `false` for development/testing. |
| `require_frame_variance` | bool | `true` | Require multiple frames with different embeddings before accepting. Defends against static photo attacks. |
| `require_landmark_liveness` | bool | `true` | Require landmark movement between frames to pass liveness check. Detects static images by tracking facial landmark positions across frames. |
| `suppress_unknown` | bool | `false` | Suppress warnings for unknown users (users with no enrolled face). |
| `min_auth_frames` | u32 | `3` | Minimum number of matching frames required before accepting. Only applies when `require_frame_variance` is true. |

### [security.rate_limit]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `max_attempts` | u32 | `5` | Maximum auth attempts per user per window. |
| `window_secs` | u64 | `60` | Rate limit window in seconds. |

## [notification]

Controls how authentication feedback is delivered.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `mode` | string | `"terminal"` | Notification mode. `"off"` -- no notifications. `"terminal"` -- PAM text prompts only. `"desktop"` -- desktop popups only (via D-Bus/notify-send). `"both"` -- terminal and desktop. |
| `notify_prompt` | bool | `true` | Show prompt when scanning starts ("Identifying face..."). |
| `notify_on_success` | bool | `true` | Notify on successful face match. |
| `notify_on_failure` | bool | `false` | Notify on failed face match. |

## [snapshots]

Save camera snapshots on auth attempts for debugging or auditing.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `mode` | string | `"off"` | `"off"` -- never save. `"all"` -- every attempt. `"failure"` -- failed auth only. `"success"` -- successful auth only. |
| `dir` | string | `"/var/log/facelock/snapshots"` | Directory for snapshot JPEG images. |

## [encryption]

Controls how face embeddings are encrypted at rest.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `method` | string | `"none"` | `"none"` -- no encryption. `"keyfile"` -- AES-256-GCM with a plaintext key file. `"tpm"` -- AES-256-GCM with a TPM-sealed key (recommended if TPM available). |
| `key_path` | string | `"/etc/facelock/encryption.key"` | Path to AES-256-GCM key file for `keyfile` method. |
| `sealed_key_path` | string | `"/etc/facelock/encryption.key.sealed"` | Path to TPM-sealed AES key for `tpm` method. |

## [audit]

Structured audit logging of authentication events.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Enable structured audit logging to JSONL file. |
| `path` | string | `"/var/log/facelock/audit.jsonl"` | Path to the audit log file. |
| `rotate_size_mb` | u32 | `10` | Rotate the log file when it exceeds this size (in MB). |

## [tpm]

TPM 2.0 settings for sealing the AES encryption key. These settings apply when `encryption.method = "tpm"`.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `pcr_binding` | bool | `false` | Bind sealed key to boot state (PCR values). |
| `pcr_indices` | list of u32 | `[0, 1, 2, 3, 7]` | PCR registers to verify on unseal. |
| `tcti` | string | `"device:/dev/tpmrm0"` | TPM Communication Interface. |
