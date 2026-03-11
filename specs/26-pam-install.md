# Spec 26: PAM Installation Command

## Scope

Add `facelock setup --pam` to safely install the PAM module into system PAM config.

## Command Behavior

```
facelock setup --pam [--service sudo] [--remove] [--yes]
```

### Install (default)
1. Check for root
2. Check `pam_facelock.so` exists at `/lib/security/pam_facelock.so`
3. Target service: `--service` flag (default: `sudo`)
4. Refuse to touch `system-auth`, `login`, or `sshd` without explicit `--yes`
5. Back up `/etc/pam.d/<service>` → `/etc/pam.d/<service>.facelock-backup`
6. Prepend `auth  sufficient  pam_facelock.so` as first auth line
7. Print confirmation and rollback instructions

### Remove (`--remove`)
1. Check for root
2. Remove the `auth sufficient pam_facelock.so` line from `/etc/pam.d/<service>`
3. If backup exists, offer to restore it

### Safety

- NEVER auto-install to `system-auth` or `login` — require explicit `--service system-auth --yes`
- Always create backup before modifying
- Check that the line isn't already present (idempotent)
- Print rollback instructions after every modification

## Implementation

### `crates/facelock-cli/src/commands/setup.rs`

Add `--pam` flag. Parse the target PAM file, find the auth stack, prepend the facelock line.

PAM file parsing: look for lines starting with `auth` and insert before the first one. Handle both `common-auth` includes and direct auth lines.

### Error handling

- Not root → error
- PAM module not installed → error with install instructions
- Service file doesn't exist → error
- Line already present → skip with message

## Acceptance

- `sudo facelock setup --pam` installs to `/etc/pam.d/sudo` with backup
- `sudo facelock setup --pam --remove` removes cleanly
- `sudo facelock setup --pam --service login --yes` works for other services
- Idempotent
- Never modifies without backup
- Container-tested
