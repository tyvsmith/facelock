# Configuration Reference

Facelock reads its configuration from `/etc/facelock/config.toml`.
`FACELOCK_CONFIG` overrides it only when the effective user is non-root. Every
effective-UID-0 process ignores the environment; use an explicit `--config`
where supported, or the default path.

All settings are optional. Facelock auto-detects the camera and uses sensible defaults. The annotated config file at `config/facelock.toml` in the repository serves as the canonical example.

## [device]

Camera settings.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `path` | string (optional) | Auto-detect | Camera device path (e.g., `/dev/video2`). When omitted, Facelock auto-detects the best available camera, preferring IR over RGB. |
| `max_height` | u32 | `480` | Maximum frame height in pixels. Frames taller than this are downscaled to improve processing speed. |
| `rotation` | u16 | `0` | Rotate captured frames. Values: `0`, `90`, `180`, `270`. Useful for cameras mounted sideways. |
| `warmup_frames` | u32 | `2` | Frames to discard immediately after opening the camera to let exposure and gain stabilize. Device quirks may override this. |
| `dark_threshold` | f32 | `0.6` | Fraction of pixels that must be darker than `dark_pixel_value` before the frame is treated as unusably dark. |
| `dark_pixel_value` | u8 | `10` | Pixel brightness cutoff used by the dark-frame check. |
| `ir_emitter` | bool | `false` | Attempt to enable a controllable IR emitter when the camera opens. Only needed for hardware that does not auto-enable its IR LED. |
| `camera_release_secs` | u32 | `3` | Daemon only. Seconds to keep the camera streaming after a **failed** authentication so an immediate retry skips the reopen cost. Cancellation and errors release the camera at once, and so does a success unless `camera_release_after_success_secs` is set. `0` disables the hold entirely (it used to be silently substituted with 5). |
| `camera_release_after_success_secs` | u32 | `0` | Daemon only. Seconds to keep the camera streaming after a **successful** authentication too. `0` (the default) releases it immediately — the interaction is over, and on IR hardware the emitter LED goes out with it. Set it only where privileged actions repeat with no authentication caching in front of them (`sudo` with a zero `timestamp_timeout`, a polkit action without `auth_admin_keep`), so each one is a fresh authentication that would otherwise pay a camera reopen. Failures still use `camera_release_secs`; cancellations and errors always release at once. |

## [recognition]

Face detection and embedding parameters.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `threshold` | f32 | `0.80` | Cosine similarity threshold for accepting a face match. Must be between 0.0 and 1.0. Higher values are stricter. See the range guide below. |
| `timeout_secs` | u32 | `5` | Maximum seconds to attempt recognition before giving up. Must be > 0. |
| `no_face_timeout_secs` | u32 | `2` | Seconds to keep scanning when **no face at all** has been detected. Once a face is seen, `timeout_secs` takes over — "seen, not matched yet" is the case worth waiting out. An empty-chair attempt ends early and charges no rate-limit budget. Clamped to `timeout_secs` (never an error); `0` disables the early exit. |
| `detection_confidence` | f32 | `0.5` | Minimum confidence for the face detector to report a detection. Lower values detect more faces but increase false positives. |
| `nms_threshold` | f32 | `0.4` | Non-maximum suppression threshold for overlapping detections. |
| `detector_model` | string | `"scrfd_2.5g_bnkps.onnx"` | ONNX detector model filename. Must exist in `daemon.model_dir`. Bundled models are verified against the manifest; custom models require `detector_sha256`. |
| `detector_sha256` | string (optional) | unset | Required digest for a custom detector; bundled models use the manifest digest. |
| `embedder_model` | string | `"w600k_r50.onnx"` | ONNX embedder model filename. Must exist in `daemon.model_dir`. Bundled models are verified against the manifest; custom models require `embedder_sha256`. |
| `embedder_sha256` | string (optional) | unset | Required digest for a custom embedder; bundled models use the manifest digest. |
| `execution_provider` | string | `"cpu"` | ONNX Runtime execution provider. Values: `"cpu"`, `"cuda"`, `"rocm"`, `"openvino"`. GPU providers require a GPU-enabled ONNX Runtime package installed on the system. |
| `threads` | u32 | `4` | Number of CPU threads for ONNX inference. |

### Threshold range guide (ArcFace cosine similarity)

| Range | Description |
|-------|-------------|
| 0.30 -- 0.50 | Very loose -- high false accept rate, not recommended |
| 0.50 -- 0.65 | Loose -- convenient but may accept similar-looking people |
| 0.65 -- 0.80 | Balanced -- good for most setups, low false accept rate |
| 0.80 -- 0.90 | Strict -- rarely accepts wrong person, may reject on bad angles |
| 0.90+ | Very strict -- may require near-ideal lighting and pose |

Run `sudo facelock test` to see your similarity scores, then set the threshold below your typical match score with some margin. Exit zero alone is not a match verdict; inspect the output.

### Model tiers

| Tier | Detector | Embedder | Total size | Notes |
|------|----------|----------|------------|-------|
| Standard | `scrfd_2.5g_bnkps.onnx` (3MB) | `w600k_r50.onnx` (166MB) | ~170MB | Fast, good accuracy (default) |
| Balanced | `scrfd_2.5g_bnkps.onnx` (3MB) | `glintr100.onnx` (249MB) | ~252MB | ~15-30ms slower, better recognition |
| High accuracy | `det_10g.onnx` (17MB) | `glintr100.onnx` (249MB) | ~266MB | ~40-50ms slower, best accuracy |

Run `sudo facelock setup` to select a model tier interactively and download the required models.
If you point `detector_model` or `embedder_model` at a custom file, you must also set the matching SHA256 so the daemon can verify it at load time.

## [daemon]

Controls how the PAM module reaches the face engine.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `mode` | string | `"daemon"` | `"daemon"` connects to a persistent daemon via D-Bus system bus (models stay loaded; only a cold attempt pays a camera reopen -- measure it with `sudo facelock bench camera-reopen`). `"oneshot"` spawns `facelock auth` per PAM call (slower: model load on every call, no background process). |
| `model_dir` | string | `"/var/lib/facelock/models"` | Directory containing ONNX model files. |
| `idle_timeout_secs` | u64 | `0` | Shut down the daemon after this many idle seconds. `0` means never. Useful with D-Bus activation. |

## [storage]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `db_path` | string | `"/var/lib/facelock/facelock.db"` | SQLite database for face embeddings. File permissions should be 600, owned by `root:root`. |

## [security]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `disabled` | bool | `false` | Disable face authentication entirely. PAM returns IGNORE, falling through to the next auth method. |
| `abort_if_ssh` | bool | `true` | Refuse face auth when connected via SSH (no camera available). |
| `abort_if_lid_closed` | bool | `true` | Refuse face auth when the laptop lid is closed (camera blocked). |
| `require_ir` | bool | `true` | Require an IR camera for authentication. RGB cameras are trivially spoofed with a printed photo. Only set to `false` for development/testing. |
| `require_frame_variance` | bool | `true` | Require multiple frames with different embeddings before accepting. Defends against static photo attacks. |
| `frame_variance_max_similarity` | f32 | `0.985` | Maximum similarity between consecutive matched frames in the variance window. Passive anti-photo check only; it does not stop video replay. |
| `ir_texture_min_stddev` | f32 | `10.0` | Minimum raw-grayscale standard deviation for the IR texture check. |
| `require_landmark_liveness` | bool | `false` | Require landmark movement between frames to pass liveness check. Detects static images by tracking facial landmark positions across frames. Experimental; off by default. |
| `landmark_displacement_px` | f32 | `1.5` | Minimum pixel displacement for a landmark to count as "moving" between frames. Only used when `require_landmark_liveness` is true. |
| `landmark_min_moving` | u32 | `3` | Number of facial landmarks (out of 5) that must show movement to pass the liveness check. Only used when `require_landmark_liveness` is true. |
| `suppress_unknown` | bool | `false` | Suppress warnings for unknown users (users with no enrolled face). |
| `min_auth_frames` | u32 | `3` | Minimum number of matching frames required before accepting. Only applies when `require_frame_variance` is true. |
| `bind_templates_to_device` | bool | `true` | Skip templates enrolled on a camera that does not match the live camera at the configured granularity. Advisory, not device attestation. |
| `device_match_granularity` | string | `"model"` | `"model"` compares VID:PID; `"unit"` also requires a stable serial. |
| `bind_legacy_templates` | bool | `true` | Permit older templates without a device identity, with a re-enrollment warning. |
| `bind_device_aad` | bool | `false` | Opt-in cryptographic camera binding for encrypted templates; requires re-enrollment and a usable device identity. |
| `allow_plaintext` | bool | `false` | Permit `encryption.method = "none"`; without it, plaintext enrollment is refused. |

### [security.rate_limit]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `max_attempts` | u32 | `5` | Maximum face-detected authentication failures per user per window; successful and no-face attempts do not consume this budget. |
| `window_secs` | u64 | `60` | Rate limit window in seconds. |

### [security.pam_policy]

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `allowed_services` | list of strings | `[]` | If non-empty, only these PAM services may use facelock. |
| `denied_services` | list of strings | `[]` | PAM services that must always skip facelock, even if otherwise allowed. |

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
| `method` | string | `"keyfile"` | `"keyfile"` -- AES-256-GCM with a root-only key file. `"tpm"` -- AES-256-GCM with a TPM-sealed key. `"none"` requires `security.allow_plaintext = true`. |
| `key_path` | string | `"/etc/facelock/encryption.key"` | Path to AES-256-GCM key file for `keyfile` method. |
| `sealed_key_path` | string | `"/etc/facelock/encryption.key.sealed"` | Path to TPM-sealed AES key for `tpm` method. |

With `method = "tpm"`, the 32-byte AES key is sealed by the TPM at rest. At daemon startup, the key is unsealed and held in memory. Embeddings use the same AES-256-GCM format as `keyfile` — no re-encryption needed when migrating between methods. The root-gated migration commands are `sudo facelock tpm seal-key` (keyfile → tpm) and `sudo facelock tpm unseal-key` (tpm → keyfile). `seal-key` requires the plaintext key to exist and refuses to overwrite a sealed key; `unseal-key` requires the sealed key and refuses to overwrite a plaintext key. Each command updates `encryption.method` only after writing the destination key.

## [polkit]

| Key | Default | Description |
|-----|---------|-------------|
| `face_eligible_actions` | `["org.freedesktop.login1.lock-sessions"]` | Action IDs the optional agent may handle; it declines all others. |

## [pam]

| Key | Default | Description |
|-----|---------|-------------|
| `config_dirs` | `["/etc/pam.d", "/usr/lib/pam.d"]` | Lookup order. Only the first directory is writable; later entries are vendor roots. |

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
| `seal_database` | bool | `false` | Seal the SQLite database file with the TPM key in addition to the encryption key. |
| `pcr_binding` | bool | `false` | Bind sealed key to boot state (PCR values). |
| `pcr_indices` | list of u32 | `[0, 1, 2, 3, 7]` | PCR registers to verify on unseal. |
| `tcti` | string | `"device:/dev/tpmrm0"` | TPM Communication Interface. |
