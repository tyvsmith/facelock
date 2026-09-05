# Testing and Safety

PAM, package lifecycle, service activation, enrollment, and authentication can
change the machine or require real hardware. Do not exercise those paths on a
workstation merely to validate documentation or a patch.

## Safe local checks

These checks do not need root, a camera, installed models, or host PAM edits:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
just check
```

`just check` is broader than the first two commands: it includes formatting,
the RustSec audit, documentation/contracts checks, source-install lifecycle
tests, and package/release contract checks. See
[Developer Commands](developer-commands.md) for the generated inventory.

Ignored hardware tests need models and a camera and are not part of that safe
baseline:

```bash
just link-models
cargo test --workspace -- --ignored
```

## Container and guest tiers

The Arch PAM smoke container tests module loading and failure behavior without
editing host PAM:

```bash
just test-arch-pam
```

The camera container recipes pass real devices through and perform live
enrollment/authentication. Run them only when that hardware interaction is
intended:

```bash
just test-arch-integration
just test-arch-oneshot
```

They default to a 90-second live-step timeout. A longer timeout uses
`timeout(1)` syntax:

```bash
FACELOCK_LIVE_TIMEOUT=5m just test-arch-integration
```

Container coverage is not proof that a booted package, display manager, or
real login stack is safe. Use the evidence walkthrough in an explicitly marked
disposable guest for those cases; its runner refuses ordinary hosts and does
not provision a VM for you. See [Testing Walkthrough](testing-walkthrough.md).

## Development configuration

`dev/config.toml` uses checkout models, oneshot mode, and temporary database,
key, snapshot, and audit paths. It is not rootless: the management CLI keeps
its normal privilege gate. Root also ignores `FACELOCK_CONFIG`, so pass the
configuration explicitly:

```bash
just build
just link-models
sudo target/debug/facelock --config "$PWD/dev/config.toml" devices
sudo target/debug/facelock --config "$PWD/dev/config.toml" enroll --skip-setup-check
sudo target/debug/facelock --config "$PWD/dev/config.toml" test
```

Do not run `setup` for this flow. Setup owns installed-system state, including
the fixed `/etc/facelock/.setup-complete` marker, and may offer systemd and PAM
changes. The explicit non-default configuration routes supported management
commands through direct access; it does not make a manually started daemon use
that backend.

`facelock test` returning zero is not proof of a match or even a scan. It also
returns zero when no usable enrollment exists and after a completed non-match.
Read its output.

## Host PAM testing

Only test host PAM after the container and disposable-guest tiers are
satisfactory.

1. Open a separate root shell and keep it open.
2. Optionally create and label your own emergency copy before Facelock touches
   the service: `cp /etc/pam.d/sudo /root/sudo.pam.before-facelock` from that
   root shell.
3. Add only the `sudo` service with `facelock pam add --service sudo` from the
   root shell.
4. Test a correct password and a wrong password in a new terminal, then test
   face authentication.
5. If anything is wrong, run `facelock pam remove --service sudo` from the
   retained root shell.

Facelock-managed rollback files are versioned under
`/var/lib/facelock/pam-backups/` with adjacent JSON provenance. They are not
the old `/etc/pam.d/sudo.facelock-backup` path. Never select the newest-looking
backup and copy it blindly: review its provenance and target state, or let the
CLI perform the validated removal. An adjacent
`/etc/pam.d/sudo.facelock-backup` exists only if an operator or an older
release created it; current Facelock does not create that emergency copy.

Do not begin with `login`, `sshd`, a display manager, or shared stacks such as
`system-auth` and `common-auth`. The CLI requires `--allow-sensitive` for these
targets because one error can affect many authentication paths.

If the retained root shell is unavailable, boot a recovery environment,
remount the root filesystem read-write, and remove the exact
`pam_facelock.so` rule or restore a separately reviewed operator copy. See
[Troubleshooting](troubleshooting.md#pam-lockout-recovery).

## Logging

Use global `-v` flags for privileged commands because they survive sudo's
environment filtering:

```bash
sudo facelock -v test
sudo facelock -vv daemon run
```

For target-specific filters, pass the environment through a trusted `env`
invocation:

```bash
sudo env RUST_LOG=facelock_camera=trace facelock devices
```
