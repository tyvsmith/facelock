# Spec 24: systemd Setup Command

## Scope

Add `facelock setup --systemd` to install and enable systemd units.

## Command Behavior

```
facelock setup --systemd [--disable]
```

### Install (default)
1. Check for root (exit with error if not)
2. Copy `facelock-daemon.service` and `facelock-daemon.socket` to `/usr/lib/systemd/system/`
3. Run `systemctl daemon-reload`
4. Run `systemctl enable --now facelock-daemon.socket`
5. Print confirmation

### Disable (`--disable`)
1. Check for root
2. Run `systemctl disable --now facelock-daemon.socket facelock-daemon`
3. Print confirmation

## Implementation

### Embedded unit files

Embed the `.service` and `.socket` files at compile time:
```rust
const SERVICE_UNIT: &str = include_str!("../../../systemd/facelock-daemon.service");
const SOCKET_UNIT: &str = include_str!("../../../systemd/facelock-daemon.socket");
```

### `crates/facelock-cli/src/commands/setup.rs`

Add `--systemd` flag to the existing `setup` command. The flag is orthogonal to model download — `facelock setup` downloads models, `facelock setup --systemd` installs units, `facelock setup --systemd` after models are downloaded is fine.

### Error handling

- Not root → clear error message
- systemd not present → clear error message ("systemd not found, use manual daemon management")
- Units already installed → overwrite (idempotent)

## Acceptance

- `sudo facelock setup --systemd` installs and enables socket activation
- `sudo facelock setup --systemd --disable` stops and disables
- Idempotent (safe to run multiple times)
- Works on Arch, Fedora, Ubuntu (standard systemd paths)
- Non-systemd systems get a clear error
