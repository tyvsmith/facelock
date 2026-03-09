# Spec 07: CLI Tool

**Phase**: 4 (Interfaces) | **Crate**: howdy-cli | **Depends on**: 01 (IPC protocol) | **Parallel with**: 06

## Goal

User-facing CLI for face management, diagnostics, and model setup. Communicates with the daemon via Unix socket IPC.

## Dependencies

- `howdy-core` (config, types, IPC protocol)
- `clap` (derive macros)
- `indicatif` (progress bars for download)
- `reqwest` (blocking, for model download)
- `notify-rust` (D-Bus notifications)
- `anyhow` (error handling)
- `tracing`, `tracing-subscriber`

## CLI Structure

```
howdy <command>

Commands:
  setup           Download models and create directories
  enroll          Capture and store a face
  remove          Remove a face model
  clear           Remove all face models for a user
  list            List stored face models
  test            Test face recognition
  preview         Live camera preview with detection overlay
  config          Show or edit configuration
  status          Check system status
  devices         List available camera devices
```

## Commands

### `howdy setup`

First-time setup. Requires root or appropriate permissions.

1. Create directories: model_dir, db parent, snapshot dir, socket parent
2. Parse model manifest (embedded via `include_str!`)
3. Check each model: exists + SHA256 match
4. Download missing models with progress bar (reqwest + indicatif)
5. Verify SHA256 after download
6. Report status

### `howdy enroll [--user USER] [--label LABEL]`

1. Resolve user: `--user` flag > `SUDO_USER` env > `DOAS_USER` > current user
2. Generate label from `YYYY-MM-DD-N` if not provided
3. Connect to daemon
4. Send `Enroll { user, label }`
5. Display progress ("Look at the camera. Slowly turn your head left and right.")
6. Receive response: show model ID and snapshot count
7. Warn if user has > 5 models

### `howdy remove <ID> [--user USER] [-y|--yes]`

1. Resolve user
2. Confirm unless `--yes`
3. Connect to daemon
4. Send `RemoveModel { user, model_id }`
5. Print confirmation

### `howdy clear [--user USER] [-y|--yes]`

1. Resolve user
2. Confirm ("Remove ALL face models for user 'ty'? [y/N]")
3. Connect to daemon
4. Send `ClearModels { user }`
5. Print count removed

### `howdy list [--user USER] [--json]`

1. Resolve user
2. Connect to daemon
3. Send `ListModels { user }`
4. Display as table (ID, Label, Created) or JSON

### `howdy test [--user USER]`

1. Resolve user
2. Connect to daemon
3. Send `Authenticate { user }`
4. Display result:
   - Match: "Matched model #1 (similarity: 0.87) in 0.23s"
   - No match: "No match (best: 0.31) after 5.0s"

### `howdy preview`

Delegate to spec 08 (Wayland preview window). If `--text-only`, print detection results to stdout.

### `howdy config [--edit]`

1. If `--edit`: open config file in `$EDITOR` (fallback: nano, vi)
2. Otherwise: print config file path and contents

### `howdy status`

Check system health without requiring daemon:

1. Config: parseable? device path set?
2. Daemon: socket exists? Ping responds?
3. Camera: device exists? VIDEO_CAPTURE capable?
4. Models: model files exist? checksums match?
5. PAM: `/lib/security/pam_howdy.so` exists? `/etc/pam.d/sudo` contains howdy?
6. Display status table with pass/fail indicators

### `howdy devices`

1. Enumerate V4L2 devices
2. Display table: path, name, driver, formats, resolutions

## User Resolution

```rust
fn resolve_user(flag: Option<&str>) -> String {
    flag.map(String::from)
        .or_else(|| std::env::var("SUDO_USER").ok())
        .or_else(|| std::env::var("DOAS_USER").ok())
        .unwrap_or_else(|| {
            nix::unistd::User::from_uid(nix::unistd::getuid())
                .ok().flatten()
                .map(|u| u.name)
                .unwrap_or_else(|| "unknown".into())
        })
}
```

## Tests

- All subcommands parse with `--help`
- `status` works without daemon running
- `devices` works without camera
- User resolution: flag > SUDO_USER > DOAS_USER > current
- Config display: shows path and contents
- Model manifest parsing

## Acceptance Criteria

1. All commands parse correctly
2. `setup` downloads and verifies models
3. `enroll` captures and stores
4. `list` displays models
5. `test` performs face match
6. `status` provides useful diagnostics
7. Helpful error messages for common failures (daemon not running, no models, etc.)

## Verification

```bash
cargo build -p howdy-cli
cargo test -p howdy-cli
cargo run --bin howdy -- --help
```
