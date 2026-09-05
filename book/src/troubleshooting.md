# Troubleshooting

## Camera selection

Start with the root-gated device report:

```bash
sudo facelock devices
v4l2-ctl --list-devices
```

Facelock identifies IR from an exclusively mono advertised format set or an
exact hardware quirk, never from “IR” in the device name. `GREY` is supported;
`Y16` additionally needs a hardware-verified `y16_bit_depth`. `Y8`, `Y10`,
`Y12`, raw Bayer, and unknown formats are not decode paths. See
[Compatibility](compatibility.md) before forcing a device.

Use preview to distinguish selection from detection problems:

```bash
sudo facelock preview
sudo env RUST_LOG=facelock_camera=trace facelock preview
```

## Recognition and performance

`facelock test` may return zero when no camera scan ran or after a completed
non-match. Read the printed result. A cold daemon attempt includes model load
and camera reopen; measure the hardware-specific reopen cost with:

```bash
sudo facelock bench camera-reopen
```

## PAM lockout recovery

Keep a separate root shell open for every host PAM test. From that shell,
prefer the validated removal path:

### If you still have a root shell open

```bash
facelock pam remove --service sudo
```

Current managed backups live beneath `/var/lib/facelock/pam-backups/` with
versioned names and adjacent JSON provenance. Do not restore the
newest-looking file without reviewing its provenance and the live target.
Current Facelock does not automatically create
`/etc/pam.d/sudo.facelock-backup`; that path exists only when an operator or an
older release made it.

### If you are locked out

With no root shell, boot recovery media, remount the root filesystem
read-write, and remove the exact `pam_facelock.so` rule or restore a separately
recorded and reviewed operator copy. Test `sudo` before any shared stack,
display manager, `login`, or `sshd` integration.

## Daemon and logs

```bash
systemctl status facelock-daemon.service
journalctl -u facelock-daemon.service -n 50 --no-pager
sudo facelock status --json
sudo facelock -vv daemon run
```

The package uses the D-Bus system bus and the package-owned policy under
`/usr/share/dbus-1/system.d/`. An administrator may also have local fragments;
D-Bus merges them. `sudo facelock setup --systemd` validates installed assets
and reports preserved local policy for review.

See the canonical
[Troubleshooting page on GitHub](https://github.com/tyvsmith/facelock/blob/main/docs/troubleshooting.md)
for model verification, IPU relay, permissions, and detailed recovery guidance.
