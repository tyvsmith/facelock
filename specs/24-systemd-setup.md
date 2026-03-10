# Spec 24: systemd Setup Command

## Scope

Add `visage setup --systemd` to install and enable systemd units.

## Command Behavior

```
visage setup --systemd [--disable]
```

### Install (default)
1. Check for root (exit with error if not)
2. Copy `visage-daemon.service` and `visage-daemon.socket` to `/usr/lib/systemd/system/`
3. Run `systemctl daemon-reload`
4. Run `systemctl enable --now visage-daemon.socket`
5. Print confirmation

### Disable (`--disable`)
1. Check for root
2. Run `systemctl disable --now visage-daemon.socket visage-daemon`
3. Print confirmation

## Implementation

### Embedded unit files

Embed the `.service` and `.socket` files at compile time:
```rust
const SERVICE_UNIT: &str = include_str!("../../../systemd/visage-daemon.service");
const SOCKET_UNIT: &str = include_str!("../../../systemd/visage-daemon.socket");
```

### `crates/visage-cli/src/commands/setup.rs`

Add `--systemd` flag to the existing `setup` command. The flag is orthogonal to model download — `visage setup` downloads models, `visage setup --systemd` installs units, `visage setup --systemd` after models are downloaded is fine.

### Error handling

- Not root → clear error message
- systemd not present → clear error message ("systemd not found, use manual daemon management")
- Units already installed → overwrite (idempotent)

## Acceptance

- `sudo visage setup --systemd` installs and enables socket activation
- `sudo visage setup --systemd --disable` stops and disables
- Idempotent (safe to run multiple times)
- Works on Arch, Fedora, Ubuntu (standard systemd paths)
- Non-systemd systems get a clear error
