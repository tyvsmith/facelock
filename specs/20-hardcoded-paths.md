# Spec 20: Hardcoded Path Audit

## Scope

Make the PAM module's `visage-auth` binary path configurable instead of hardcoded.

## Changes

### `crates/pam-visage/src/lib.rs`

Replace `const VISAGE_AUTH_BIN: &str = "/usr/bin/visage-auth"` with a config field.

Add to `PamDaemonConfig`:
```rust
#[serde(default = "default_auth_bin")]
auth_bin: String,
```
Default: `/usr/bin/visage auth` (after Spec 23 merges binaries).

### Verify no other hardcoded paths

Confirm all production paths flow through `paths.rs` or config. The ACPI lid paths (`/proc/acpi/button/lid/...`) and `/proc/self/environ` are kernel interfaces, not configurable — leave as-is.

## Acceptance

- `VISAGE_AUTH_BIN` const removed
- PAM module reads `daemon.auth_bin` from config
- Default value works out of the box
- Existing tests pass
