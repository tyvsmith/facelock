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
2. IR detection looks for keywords like "ir" or "infrared" in the device name and checks for grayscale formats (GREY, Y16). Some cameras do not advertise IR in their name.
3. If you are certain your camera is IR, check its V4L2 capabilities:
   ```bash
   v4l2-ctl -d /dev/video2 --list-formats-ext
   ```
4. As a workaround, you can set `security.require_ir = false` -- but understand this weakens spoofing resistance. Only do this for testing.

## Auth too slow

### First-start latency (~700ms -- 2s)

The first authentication after boot (or after the daemon starts) is slow because ONNX models must be loaded into memory. This is normal. Subsequent auths in daemon mode take ~200ms.

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
   cp /etc/pam.d/sudo.facelock-backup /etc/pam.d/sudo
   ```
   Or remove the Facelock line from `/etc/pam.d/sudo`:
   ```bash
   sed -i '/pam_facelock/d' /etc/pam.d/sudo
   ```
4. Reboot normally.

### If you still have a root shell open

```bash
# From your root shell:
cp /etc/pam.d/sudo.facelock-backup /etc/pam.d/sudo
```

### Prevention

- Always test in containers first (`just test-pam`).
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

### "Permission denied" when running facelock commands

Ensure your user is in the `facelock` group:
```bash
groups  # check current groups
sudo usermod -aG facelock $USER
# Log out and back in for group changes to take effect
```

### Database permission errors

The SQLite database requires specific permissions:
```bash
sudo chown root:facelock /var/lib/facelock/facelock.db
sudo chmod 640 /var/lib/facelock/facelock.db
# The directory needs group write for SQLite WAL files:
sudo chmod 770 /var/lib/facelock
```

### PAM module cannot reach daemon

The daemon is accessed via D-Bus system bus (`org.facelock.Daemon`). Verify:
```bash
busctl status org.facelock.Daemon
systemctl status facelock-daemon.service
```

## Debugging with RUST_LOG

Facelock uses the `tracing` crate with `RUST_LOG` env-filter syntax.

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
