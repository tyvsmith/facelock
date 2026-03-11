# TODO: Integration Tests for PAM/Daemon/Notification Flow

Container tests that would have caught the issues from the Specs 28-30 investigation.

## Socket Activation Fallback

The daemon crashed under systemd (`DevicePolicy=closed` blocked `/dev/null`). The PAM module connected to the systemd-managed socket, but send/recv failed when the daemon crashed. Previously returned `PAM_IGNORE` instead of falling back to oneshot.

**Tests needed:**
- Start socket listener (fake), accept connection, immediately close → verify PAM falls back to oneshot (syslog: `daemon_send_failed...falling back to oneshot`)
- Start socket listener, accept, read request, close without responding → verify PAM falls back to oneshot (syslog: `daemon_recv_failed...falling back to oneshot`)
- No socket file at all → verify PAM falls back to oneshot (syslog: `success (oneshot)`)

## Daemon-Mode PAM Without Camera

Test the full PAM → daemon IPC → response flow without needing hardware.

**Tests needed:**
- Start daemon, enroll with mock/recorded embeddings, then `pamtester` authenticates via daemon
- Verify syslog shows `success (similarity=...)` not `success (oneshot)`
- Verify daemon stays alive after auth (no crash)

## Syslog Path Assertions

The PAM module logs which auth path was taken. These logs were critical for debugging.

**Tests needed:**
- No daemon, no enrolled faces → syslog contains `no_enrolled_faces`
- Daemon running, enrolled face, auth succeeds → syslog contains `success (similarity=`
- Oneshot mode, enrolled face, auth succeeds → syslog contains `success (oneshot)`
- Daemon crash during auth → syslog contains `falling back to oneshot`

## Desktop Notification Smoke Test

Containers lack D-Bus/Mako, so we can't verify actual desktop popups. But we can verify the notification attempt.

**Tests needed:**
- Install a fake `/usr/bin/notify-send` that writes args to a file
- Trigger PAM auth success → verify fake notify-send was called with expected args
- Verify `send_desktop_notification` fork+setuid drops to correct UID (check `/proc/self/status` in fake binary)

## Systemd Service Hardening

The daemon service unit was stripped to zero restrictions after `DevicePolicy=closed` caused silent crashes. Restrictions should be re-added one at a time.

**Tests needed (use `systemd-run --pipe` with each restriction):**
- `ProtectSystem=strict` + `ReadWritePaths` — should work
- `ProtectHome=yes` — should work
- `PrivateTmp=yes` — should work
- `DevicePolicy=closed` + `DeviceAllow=/dev/video* rw` + `DeviceAllow=char-misc rw` — test if ONNX needs `/dev/null`, `/dev/urandom`
- `ProtectKernelTunables=yes` — known to break ONNX
- `ProtectProc=invisible` — may hide stderr
- `NoNewPrivileges=yes` — blocks notification subprocess privilege drop
