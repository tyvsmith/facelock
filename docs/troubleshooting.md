# Troubleshooting

## Camera not detected

**Symptom**: `facelock devices` shows no cameras, or `facelock enroll` fails with "no camera found".

**Steps**:

1. Check that your camera is recognized by the kernel:
   ```bash
   ls /dev/video*
   v4l2-ctl --list-devices
   ```
2. Verify your user has access to the video device:
   ```bash
   groups  # should include "video"
   sudo usermod -aG video $USER  # add yourself, then log out and back in
   ```
3. If `/dev/video*` exists but Facelock skips it, set the device explicitly:
   ```toml
   [device]
   path = "/dev/video0"
   ```
4. Some cameras expose multiple `/dev/video*` nodes (capture + metadata). Try each one.

## "No IR camera" error when IR camera is available

**Symptom**: `security.require_ir = true` (the default) rejects your camera even though it supports IR.

**Steps**:

1. Check what Facelock detects:
   ```bash
   facelock devices
   ```
2. IR detection is derived from the pixel formats the camera reports, **not** its name. A node classifies as IR only when it enumerates *exclusively* IR-typical mono formats (GREY, Y8, Y10, Y12, Y16) with no color format (YUYV/MJPG) mixed in, or when a hardware quirk matches it. The device name is never used to classify a camera as IR (it is only a tiebreak during auto-detection), so a genuine IR camera whose single node also advertises a color format will **not** auto-classify.
3. Inspect the formats your camera actually reports:
   ```bash
   v4l2-ctl -d /dev/video2 --list-formats-ext
   ```
   If your IR sensor is a separate `/dev/video*` node that reports only a mono format (e.g. GREY or Y16), point `device.path` at that node (or let auto-detect find it). If IR and color share one node (a mixed format set), it will not classify by evidence alone — use a quirk (next step).
4. For a genuine IR camera that format evidence alone does not catch, add a quirk in `/etc/facelock/quirks.d/` keyed by **USB vendor:product ID** (find yours with `lsusb`), which is the authoritative override:
   ```toml
   [[quirk]]
   vendor_id = "046d"
   product_id = "085e"
   force_ir = true
   format_preference = "GREY"   # the IR node's native format, for multi-node cameras
   notes = "My IR camera"
   ```
   Prefer a USB-ID quirk over a `name_pattern` one: a name-only `force_ir` is trusted only when corroborated by the device's own mono-format evidence or a real USB identity, so it will not, by itself, force a color-only device to IR.
5. As a last resort you can set `security.require_ir = false` -- but understand this weakens spoofing resistance. Only do this for testing.

## Enrollment captures 0 frames

**Symptom**: `facelock enroll` ends with "only captured 0 frames, need at least 3".

**Steps**:

1. **Lighting**: frames with mean brightness below ~20 are rejected as too
   dark. IR cameras usually self-illuminate, but RGB cameras need a lit room.
   Add light and retry.
2. **Wrong device / format**: if the configured camera is a raw sensor node
   (e.g. Intel IPU6/IPU7 Bayer nodes, formats like `SGRBG10`), facelock now
   refuses to open it with an error listing the advertised formats. Point
   `device.path` at a processed camera instead — see "Intel IPU6/IPU7 MIPI
   cameras" in `docs/compatibility.md`.
3. **Verify frames are usable**: run `facelock preview --json` (or
   `facelock preview`) and check that you see a live image with your face
   detected.
4. Run with `RUST_LOG=facelock_daemon=debug` to see per-frame rejection
   reasons (dark frame, no face detected, low quality).

## Auth too slow

### First-start latency (~700ms -- 2s)

The first authentication after boot (or after the daemon starts) is slow because ONNX models must be loaded into memory. This is normal. Subsequent auths in daemon mode never reload the models; a cold one additionally pays a camera reopen, a warm one does not. What that reopen costs is a property of your camera and driver — measure it with `sudo facelock bench camera-reopen` (it prints the open / STREAMON / warmup split) rather than comparing against someone else's figure.

### Consistently slow (~700ms+ every time)

You may be running in oneshot mode. Check your config:

```toml
[daemon]
mode = "daemon"  # default; uses persistent daemon
```

With daemon mode, enable D-Bus activation:
```bash
sudo facelock setup --systemd
```

### Inference slow on CPU

Try reducing frame resolution or switching to the smaller model set:
```toml
[device]
max_height = 320

[recognition]
detector_model = "scrfd_2.5g_bnkps.onnx"
embedder_model = "w600k_r50.onnx"
threads = 4  # increase if you have more cores
```

## PAM lockout recovery

**A broken PAM module can lock you out of your system.** Always keep a root shell open when testing PAM changes.

### If you are locked out

1. Boot into single-user/recovery mode (GRUB: edit the boot entry, add `single` or `init=/bin/bash` to the kernel line).
2. Remount the filesystem read-write:
   ```bash
   mount -o remount,rw /
   ```
3. Restore the PAM backup:
   ```bash
   cp /var/lib/facelock/pam-backups/sudo.TIMESTAMP /etc/pam.d/sudo
   ```
   Or remove the Facelock line from `/etc/pam.d/sudo`:
   ```bash
   sed -i '/pam_facelock/d' /etc/pam.d/sudo
   ```
4. Reboot normally.

### If you still have a root shell open

```bash
# From your root shell:
cp /var/lib/facelock/pam-backups/sudo.TIMESTAMP /etc/pam.d/sudo
```

### Prevention

- Always test in containers first (`just test-arch-pam`).
- Keep a root shell open during PAM testing.
- Start with `sudo` only -- do not add Facelock to `login` or `sddm` until `sudo` works reliably.
- Set `security.disabled = true` as an emergency kill switch (PAM returns IGNORE).

## systemd unit not starting

**Symptom**: `systemctl status facelock-daemon.service` shows failed or inactive.

**Steps**:

1. Check the journal:
   ```bash
   journalctl -u facelock-daemon.service -n 50 --no-pager
   ```
2. Verify the service unit is enabled and D-Bus activation is configured:
   ```bash
   systemctl status facelock-daemon.service
   systemctl enable --now facelock-daemon.service
   ```
3. Check that the binary exists:
   ```bash
   which facelock
   ls -la /usr/bin/facelock
   ```
4. Check model files exist:
   ```bash
   ls -la /var/lib/facelock/models/
   ```
5. Manual test run (should print errors to stderr):
   ```bash
   sudo /usr/bin/facelock daemon
   ```

### Known issue: ONNX runtime crashes under restrictive systemd sandboxing

The ONNX runtime requires access to `/dev/null`, `/dev/urandom`, and `/proc/sys`. If you have customized the systemd unit with `DevicePolicy=closed`, `ProtectKernelTunables=yes`, or `ProtectProc=invisible`, the daemon may crash before `main()` with no stderr output. Use the default unit file or add restrictions incrementally, testing each one.

## Model download failures

**Symptom**: `facelock setup` fails to download models.

**Steps**:

1. Check network connectivity.
2. Try downloading manually:
   ```bash
   curl -L -o /var/lib/facelock/models/scrfd_2.5g_bnkps.onnx \
     "https://github.com/visomaster/visomaster-assets/releases/download/v0.1.0/scrfd_2.5g_bnkps.onnx"
   curl -L -o /var/lib/facelock/models/w600k_r50.onnx \
     "https://github.com/visomaster/visomaster-assets/releases/download/v0.1.0/w600k_r50.onnx"
   ```
3. Verify SHA256 checksums match (Facelock checks these at model load time and rejects tampered files).
4. Ensure the model directory exists and has correct permissions:
   ```bash
   sudo mkdir -p /var/lib/facelock/models
   sudo chown root:root /var/lib/facelock/models
   sudo chmod 755 /var/lib/facelock/models
   ```

## Permission issues

### "Permission denied" / "AccessDenied" when running facelock commands

**Symptom**: `facelock preview`, `facelock test`, `facelock list` or another
command fails with a D-Bus `AccessDenied` error as a normal user (root works
fine).

Every management command is root-only; the CLI offers to re-run itself under
`sudo` on a terminal. Face unlock itself (hyprlock, swaylock, the polkit
agent, `facelock is-enrolled`) needs no group and no re-login: the bus admits
any local user's `Authenticate` for their own account (ADR 010). If a lock
screen still reports `AccessDenied` right after an upgrade, the bus has not
re-read the policy yet — `sudo facelock setup --systemd` rewrites it and asks
for a reload, or reboot.

The `facelock` group is no longer used (ADR 010). `sudo facelock setup`, `just
install-files` and the package scriptlets remove a leftover group; if it
lingers:
```bash
sudo groupdel facelock
```
Face unlock is turned off per user by removing that user's models
(`sudo facelock remove` / `sudo facelock clear`).

### Database permission errors

The SQLite database requires specific permissions:
```bash
sudo chown root:root /var/lib/facelock/facelock.db
sudo chmod 600 /var/lib/facelock/facelock.db
# The daemon (root) writes the -wal/-shm sidecars next to the database; the
# state directory and enrolled/ ship at 711 root:root (traversable by every
# local user, listable by none):
sudo chown root:root /var/lib/facelock /var/lib/facelock/enrolled
sudo chmod 711 /var/lib/facelock /var/lib/facelock/enrolled
```

### PAM module cannot reach daemon

The daemon is accessed via D-Bus system bus (`org.facelock.Daemon`). Verify:
```bash
busctl status org.facelock.Daemon
systemctl status facelock-daemon.service
```

## Turning up the log level

Commands print warnings and errors only. `-v` adds the informational lines,
`-vv` debug, `-vvv` trace:

```bash
facelock -v test
sudo facelock -vv daemon run
```

The flag is part of the command line, so it survives `sudo`, which strips
`RUST_LOG` from the environment.

## Debugging with RUST_LOG

Facelock uses the `tracing` crate with `RUST_LOG` env-filter syntax, which
picks a level per crate rather than one level for all of them. It outranks
`-v`.

```bash
# Verbose output for all facelock crates:
RUST_LOG=debug facelock test

# Trace a specific crate:
RUST_LOG=facelock_camera=trace facelock devices

# Multiple filters:
RUST_LOG=facelock_daemon=debug,facelock_face=trace facelock daemon
```

### sudo strips environment variables

`sudo` sanitizes the environment by default. Use `env` to preserve `RUST_LOG`:

```bash
sudo env RUST_LOG=debug facelock test
sudo env RUST_LOG=facelock_daemon=trace facelock daemon
```

### Useful log targets

| Target | What it shows |
|--------|---------------|
| `facelock_camera` | Camera detection, format negotiation, frame capture |
| `facelock_face` | Model loading, inference timing, similarity scores |
| `facelock_daemon` | IPC handling, rate limiting, auth flow |
| `facelock_store` | Database operations, embedding storage |
| `pam_facelock` | PAM module decisions (logged to syslog) |
