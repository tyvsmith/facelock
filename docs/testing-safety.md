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

### Synthetic camera

The same two scripts run against a synthetic camera, with no real device
passed through and nobody in frame:

```bash
just test-arch-loopback
```

The camera is a v4l2loopback node fed by `ffmpeg` with a procedurally
rendered face sequence (`test/loopback/NOTICE.md`: drawn from arithmetic,
nobody's face). While it is fed, the node enumerates `GREY` only, so
facelock classifies it as IR by format evidence — the residual
[Security](security.md) describes — and the run keeps `require_ir` and
`require_frame_variance` at their product defaults, because the sequence
drifts frame to frame the way a person does. A second node fed `YUYV` is the
non-IR camera the `require_ir` refusal assertions need; without it they
report `SKIP`.

The tier needs two idle loopback nodes the calling user can write. Loading
the module needs root; the recipe does not do it and exits 2 with this when
the nodes are missing:

```bash
sudo modprobe v4l2loopback devices=2 video_nr=20,21 \
    card_label=facelock-synth-ir,facelock-synth-rgb exclusive_caps=1
sudo chmod a+rw /dev/video20 /dev/video21
```

If v4l2loopback is already loaded for something else, add nodes without
unloading it (`v4l2loopback-utils`, module 0.13 or later):

```bash
sudo v4l2loopback-ctl add -x 1 -n facelock-synth-ir /dev/video20
sudo v4l2loopback-ctl add -x 1 -n facelock-synth-rgb /dev/video21
```

`FACELOCK_LOOPBACK_IR` and `FACELOCK_LOOPBACK_RGB` pick other nodes
(`FACELOCK_LOOPBACK_RGB=none` runs without the twin). The script refuses a
node that has a parent device in sysfs — a real camera — or that another
process is already feeding, so it cannot open the host's webcam by mistake.
Only the loopback nodes are passed into the container.

It records the commit it passed at to `.loopback-tier-verified`, which
`just release-preflight` requires alongside the real-camera record. It is
cheaper evidence, not the same evidence: it proves capture, IR
classification, the liveness gates, enrollment, the daemon and one-shot
paths and PAM end to end on a device the product treats as an IR sensor, and
it cannot prove that a real sensor's frames match a real face.

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
