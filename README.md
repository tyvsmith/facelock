# visage: Facial Authentication for Linux

A modern, performant rewrite of [visage](https://github.com/boltgolt/visage) in Rust. Provides Windows Hello-style facial authentication via Linux PAM.

## Why Rewrite?

The original visage suffers from:
- Python dependency conflicts and virtualenv fragility
- 2-3 second cold-start latency (spawns Python per auth)
- Complex dlib/OpenCV/GTK dependency chain
- Painful packaging across distributions

This rewrite eliminates all of that: zero Python, ~200ms auth via persistent daemon, single workspace, deterministic packaging.

## Architecture

```
                    +------------------+
                    |  /etc/pam.d/...  |
                    +--------+---------+
                             |
                    +--------v---------+
                    |  pam_visage.so    |   Thin IPC client (~200KB cdylib)
                    |  (connect to     |   No ONNX, no camera, no heavy deps
                    |   daemon socket) |   Returns PAM_IGNORE if daemon down
                    +--------+---------+
                             | Unix socket IPC
                    +--------v-------------------------------------------+
                    |  visage-daemon                                       |
                    |  (persistent, holds models + camera in memory)      |
                    |                                                     |
                    |  +------------+  +----------+  +---------+         |
                    |  | V4L2       |  | SCRFD    |  | ArcFace |         |
                    |  | Camera     |->| Detect   |->| Embed   |         |
                    |  +------------+  +----------+  +----+----+         |
                    |                                      |             |
                    |  +------------+              +-------v------+      |
                    |  | Config     |              | SQLite       |      |
                    |  | (TOML)     |              | FaceStore    |      |
                    |  +------------+              +--------------+      |
                    +----------------------------------------------------+
                             ^
                    +--------+---------+
                    |  visage-cli       |   User-facing CLI (enroll, test,
                    |  (IPC client)    |   preview, config, status)
                    +------------------+
```

### Why Daemon?

A persistent daemon keeps ONNX models loaded in memory, achieving ~200ms auth latency vs 2-3s cold start. This was the original visage's most frustrating UX problem.

- PAM module stays thin (~200KB) -- just an IPC client
- If daemon crashes or isn't running, PAM returns PAM_IGNORE (graceful fallback to password)
- Camera and GPU resources managed in one place
- Single-threaded: PAM requests are sequential, camera is shared, ONNX sessions aren't thread-safe

### Why Not Subprocess (OpenCode) or In-Process (Codex)?

- **Subprocess** (original visage pattern): 2-3s cold start per auth -- the #1 complaint about visage
- **In-process** (PAM loads ONNX directly): A segfault in ONNX Runtime would crash PAM, potentially locking out the user

The daemon provides both speed and crash isolation.

## Workspace Crates

| Crate | Type | Purpose |
|-------|------|---------|
| `visage-core` | lib | Config, types, error handling, IPC protocol |
| `visage-camera` | lib | V4L2 camera capture and frame preprocessing |
| `visage-face` | lib | ONNX inference pipeline (SCRFD + ArcFace) |
| `visage-store` | lib | SQLite face embedding storage |
| `visage-daemon` | bin | Persistent daemon owning camera + models |
| `pam-visage` | cdylib | Thin PAM module, IPC client to daemon |
| `visage-cli` | bin | User-facing CLI tool |
| `visage-bench` | bin | Benchmark and calibration tooling |

## Technology Choices

| Component | Technology | Rationale |
|-----------|-----------|-----------|
| Face detection | SCRFD via ONNX | Single-pass detection + landmarks, fast, accurate |
| Face recognition | ArcFace via ONNX | State-of-art accuracy (99.5-99.8% LFW), 512-D embeddings |
| ML runtime | `ort` crate (ONNX Runtime) | Production-proven, hardware acceleration support |
| Camera | `v4l` crate (V4L2) | Pure Rust, zero-copy MMAP, no OpenCV dependency |
| Preview | `smithay-client-toolkit` | Native Wayland layer-shell, works on Hyprland/Sway |
| Notifications | `notify-rust` | D-Bus notifications, works with mako/dunst/swaync |
| Config | `toml` + `serde` | Native Rust ecosystem, typed config |
| CLI | `clap` (derive) | Standard Rust CLI framework |
| Storage | `rusqlite` (bundled) | Embedded SQLite, migration support, concurrent access |
| IPC | Unix socket + bincode | Fast, typed, no D-Bus dependency |
| PAM | Raw FFI via `libc` | Minimal surface, no heavy deps in PAM module |

## Configuration

TOML config at `/etc/visage/config.toml`:

```toml
[device]
path = "/dev/video2"       # IR camera device
# format auto-detected (GREY, YUYV, MJPG)

[recognition]
threshold = 0.45           # Cosine similarity (0.0-1.0, higher = stricter)
timeout_secs = 5           # Max seconds to attempt recognition
max_height = 480           # Downscale frames for faster processing

[daemon]
socket_path = "/run/visage/visage.sock"
model_dir = "/var/lib/visage/models"

[storage]
db_path = "/var/lib/visage/visage.db"

[security]
abort_if_ssh = true
abort_if_lid_closed = true
disabled = false
require_ir = true              # Refuse auth on RGB-only cameras (anti-spoof)
require_frame_variance = true  # Reject static images (photo attack defense)
min_auth_frames = 3            # Minimum frames before accepting match

[notification]
enabled = true
on_success = true
on_failure = true

[snapshots]
save_failed = false
save_successful = false
dir = "/var/log/visage/snapshots"
```

Supports `VISAGE_CONFIG` env var override for rootless development.

## Face Recognition Pipeline

```
Camera Frame (RGB)
    |
    v
[SCRFD Detection] -----> Bounding boxes + 5-point landmarks
    |                     (confidence threshold + NMS)
    v
[Alignment] ------------> 112x112 aligned face crop
    |                     (affine transform from landmarks)
    v
[ArcFace Embedding] ----> 512-dim float32 vector (L2-normalized)
    |
    v
[Cosine Similarity] ----> Match score vs stored embeddings
    |                     (threshold from config)
    v
  MATCH / NO MATCH
```

## Models

Downloaded during `visage setup` from HuggingFace (InsightFace):
- **SCRFD** (`scrfd_2.5g_bnkps.onnx`): ~3MB face detection with keypoints
- **ArcFace** (`w600k_r50.onnx`): ~166MB face embedding network

Optional higher-accuracy models:
- **SCRFD 10G** (`scrfd_10g_bnkps.onnx`): ~16MB, higher accuracy
- **ArcFace R100** (`w600k_r100.onnx`): ~249MB, 99.77% LFW accuracy

Stored in `/var/lib/visage/models/` with SHA-256 integrity verification.

## Reading Order for Agents

1. `AGENTS.md` -- rules and conventions
2. `docs/contracts.md` -- stable cross-spec contracts
3. `docs/security.md` -- security model, threat mitigations, anti-spoofing
4. `docs/risk-register.md` -- known hard edges
5. `docs/delivery-roadmap.md` -- phases and dependencies
6. Your assigned `specs/XX-name.md`

## Spec Dependency Graph

```
Phase 1 (Foundation):   00-workspace -> 01-core-types
Phase 2 (Components):   02-camera, 03-face-engine, 04-face-store  (parallel, depend on 01)
Phase 3 (Integration):  05-daemon  (integrates Phase 2)
Phase 4 (Interfaces):   06-pam-module, 07-cli  (parallel, depend on 05)
Phase 5 (Polish):       08-preview, 09-notifications, 10-build-install  (parallel)
Phase 6 (Validation):   11-benchmarks, 12-integration-tests  (sequential)
```

## Definition of Done

- No Python runtime or pip dependency anywhere in the auth path
- Models installed locally and never downloaded on first use in production
- Camera preview, enrollment, and PAM auth all use the same daemon
- PAM module is a thin IPC client (no ONNX, no camera, no heavy deps)
- Auth latency < 450ms warm, < 900ms cold (daemon startup + first auth)
- Graceful PAM fallback when daemon is unavailable
- Container + VM validated PAM integration before any host PAM changes
- Benchmark evidence backing shipped default thresholds
- IR camera enforcement enabled by default (anti-spoofing)
- Frame variance check rejects static photo attacks
- Model integrity verified at load time (SHA256)
- All auth attempts logged to syslog/journald
- Socket access restricted to root + visage group with peer credential checks
- Database file permissions restrict biometric data access

## Source Plans

This aggregate plan synthesizes the best elements from three independent planning efforts:
- **OpenCode + Opus 4.6**: Detailed implementation specs, 4-tier testing, pragmatic design
- **Codex + ChatGPT 4**: Contract-first governance, risk register, benchmark tooling
- **Claude + Opus 4.6**: Daemon architecture, crate structure, dev safety
