# System Contracts

Stable contracts. Do not change without updating this document.

## Binaries

| Binary | Crate | Purpose |
|--------|-------|---------|
| `facelock` | facelock-cli | Unified CLI (daemon, auth, enroll, test, setup, etc.) |
| `libpam_facelock.so` | pam-facelock | PAM authentication module |

## CLI Subcommands

| Command | Purpose |
|---------|---------|
| `facelock setup` | Download models, create directories |
| `facelock setup --systemd` | Install/enable systemd units |
| `facelock setup --pam` | Install PAM module to `/etc/pam.d/` |
| `facelock enroll` | Capture and store a face |
| `facelock test` | Test face recognition |
| `facelock list` | List enrolled face models |
| `facelock remove <id>` | Remove a specific model |
| `facelock clear` | Remove all models for a user |
| `facelock preview` | Live camera preview |
| `facelock devices` | List V4L2 cameras |
| `facelock status` | Check system status |
| `facelock config` | Show/edit configuration |
| `facelock daemon` | Run persistent daemon |
| `facelock auth --user X` | One-shot auth (PAM helper) |
| `facelock tpm status` | TPM status |
| `facelock bench` | Benchmarks |

## Operating Modes

| Mode | Config | PAM Behavior | CLI Behavior |
|------|--------|-------------|-------------|
| Daemon | `daemon.mode = "daemon"` (default) | IPC to daemon socket | Uses daemon if available, falls back to direct |
| Oneshot | `daemon.mode = "oneshot"` | Spawns `facelock auth` | Operates directly (no daemon) |

The CLI silently falls back to direct mode when the daemon socket doesn't exist, regardless of config mode.

### facelock auth Exit Codes

| Code | Meaning | PAM Code |
|------|---------|----------|
| 0 | Face matched | PAM_SUCCESS |
| 1 | No match / timeout / dark | PAM_AUTH_ERR |
| 2 | Error / no enrolled faces | PAM_IGNORE |

## Filesystem Paths

| Path | Owner | Mode | Purpose |
|------|-------|------|---------|
| `/etc/facelock/config.toml` | root:root | 644 | Configuration |
| `/var/lib/facelock/facelock.db` | root:facelock | 640 | Face embeddings |
| `/var/lib/facelock/models/` | root:root | 755 | ONNX models |
| `/var/log/facelock/snapshots/` | root:facelock | 750 | Auth snapshots |
| `/run/facelock/facelock.sock` | daemon | 660 | IPC socket (daemon mode) |
| `/usr/bin/facelock` | root:root | 755 | CLI binary |
| `/lib/security/pam_facelock.so` | root:root | 755 | PAM module |

All paths overridable via config. `FACELOCK_CONFIG` env var overrides config location.

## Config Schema

TOML format. All keys optional -- camera auto-detected, sensible defaults for everything. See [Configuration](configuration.md) for the full reference.

### Sections

| Section | Key fields |
|---------|-----------|
| `[device]` | `path` (Option), `max_height`, `rotation` |
| `[recognition]` | `threshold`, `timeout_secs`, `detector_model`, `embedder_model`, `threads`, `execution_provider` |
| `[daemon]` | `mode` (DaemonMode enum), `socket_path`, `model_dir`, `idle_timeout_secs` |
| `[storage]` | `db_path` |
| `[security]` | `require_ir`, `require_frame_variance`, `min_auth_frames`, `abort_if_ssh`, `abort_if_lid_closed`, rate_limit sub-section |
| `[notification]` | `mode` (off/terminal/desktop/both), `notify_prompt`, `notify_on_success`, `notify_on_failure` |
| `[snapshots]` | `mode` (off/all/failure/success), `dir` |
| `[tpm]` | `seal_database`, `pcr_binding`, `pcr_indices`, `tcti` |

### Camera Auto-Detection

When `device.path` is omitted:
1. Enumerate `/dev/video0` through `/dev/video63`
2. Filter to VIDEO_CAPTURE devices
3. Prefer IR cameras (name contains "ir"/"infrared", or supports GREY/Y16 format)
4. Fall back to first available device

## Database Schema

SQLite with WAL mode and foreign keys:

```sql
CREATE TABLE face_models (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user TEXT NOT NULL,
    label TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(user, label)
);

CREATE TABLE face_embeddings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_id INTEGER NOT NULL REFERENCES face_models(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL  -- 512 x f32 = 2048 bytes
);
```

## IPC Protocol

D-Bus system bus (`org.facelock.Daemon`). Only used in daemon mode.

The daemon registers on the system bus via D-Bus activation. The PAM module and CLI connect as D-Bus clients. Access is controlled by the D-Bus system bus policy (`dbus/org.facelock.Daemon.conf`).

### D-Bus Interface

- **Bus name**: `org.facelock.Daemon`
- **Object path**: `/org/facelock/Daemon`
- **Interface**: `org.facelock.Daemon`

### Methods
`Authenticate`, `Enroll`, `ListModels`, `RemoveModel`, `ClearModels`, `PreviewFrame`, `PreviewDetectFrame`, `ListDevices`, `ReleaseCamera`, `Ping`, `Shutdown`

### Response types
`AuthResult`, `Enrolled`, `Models`, `Removed`, `Frame`, `DetectFrame`, `Devices`, `Ok`, `Error`

## PAM Semantics

| Outcome | PAM Code |
|---------|----------|
| Face matched | `PAM_SUCCESS` (0) |
| No match | `PAM_AUTH_ERR` (7) |
| Daemon unavailable / error | `PAM_IGNORE` (25) |
| Timeout | `PAM_AUTH_ERR` (7) |

PAM module never blocks indefinitely. All operations have timeouts.

### Syslog Format

```
pam_facelock(<service>): <result> for user <username>
```

## Anti-Spoofing

| Defense | Config | Default |
|---------|--------|---------|
| IR camera enforcement | `security.require_ir` | **true** |
| Frame variance check | `security.require_frame_variance` | **true** |
| Minimum auth frames | `security.min_auth_frames` | 3 |
| Variance threshold | `FRAME_VARIANCE_THRESHOLD` | 0.998 |

These defaults must not be weakened without security review.

## Models

| Model | File | Size | Default |
|-------|------|------|---------|
| SCRFD 2.5G | `scrfd_2.5g_bnkps.onnx` | ~3MB | Yes |
| ArcFace R50 | `w600k_r50.onnx` | ~166MB | Yes |
| SCRFD 10G | `det_10g.onnx` | ~17MB | Optional |
| ArcFace R100 | `glintr100.onnx` | ~249MB | Optional |

Configurable via `recognition.detector_model` and `recognition.embedder_model`.
