# ADR 007: Selective systemd Hardening with Documented Exclusions

## Status

Accepted

## Date

2026-03-14

## Context

The Facelock daemon runs as a systemd service under root (required for PAM
integration and camera access). systemd provides extensive sandboxing directives
that can restrict file access, system calls, device visibility, and memory
operations. Applying maximum hardening is desirable for a security-sensitive
service, but several directives conflict with Facelock's specific requirements:

- **Camera access.** The daemon must discover and open `/dev/video*` devices at
  runtime. Camera indices are not fixed and may change across reboots.
- **Desktop notifications.** The daemon sends authentication notifications to
  the user's session via `setpriv --reuid=$UID -- notify-send`, which requires
  access to `/run/user/$UID/bus` (the user's D-Bus session bus socket).
- **ONNX Runtime.** The `ort` crate (ONNX Runtime) uses JIT compilation to
  optimize model execution graphs at load time, requiring writable and
  executable memory pages.

Running `systemd-analyze security facelock-daemon.service` scores the current
configuration at approximately 5.4/10 (lower is more hardened). A fully
unrestricted service scores ~9.6.

## Decision

Apply selective systemd hardening with three specific exclusions, each justified
by a concrete functional requirement:

### InaccessiblePaths instead of ProtectHome

`ProtectHome=yes` mounts `/home`, `/root`, and `/run/user` as empty tmpfs
overlays. This blocks access to `/run/user/$UID/bus`, which the daemon needs to
deliver desktop notifications via the user's session D-Bus socket.

Instead, use `InaccessiblePaths` to block specific sensitive directories (e.g.,
`/home` contents) while preserving `/run/user` access. This provides home
directory protection without breaking notification delivery.

### No DevicePolicy

`DevicePolicy=closed` or `DevicePolicy=strict` uses cgroup device ACLs to
restrict device access. Under this policy, `/dev/video*` devices are hidden from
`stat()` calls, which breaks V4L2 camera auto-detection. The camera crate
enumerates `/dev/video*` by iterating and stat-ing device nodes; invisible
devices cannot be discovered.

Device access is instead controlled through the `SupplementaryGroups=video`
directive, which grants access to video devices via standard Unix group
permissions without cgroup-level hiding.

### No MemoryDenyWriteExecute

`MemoryDenyWriteExecute=yes` blocks `mmap()` calls with both `PROT_WRITE` and
`PROT_EXEC` flags. ONNX Runtime's execution provider performs JIT compilation of
model graphs to optimize inference performance. This requires allocating memory
that is first written (compiled code) then executed — exactly the pattern
`MemoryDenyWriteExecute` blocks.

Disabling JIT (if possible) would significantly degrade inference performance,
pushing authentication latency beyond the 2-second target.

### Hardening directives that ARE applied

The service unit does apply many other hardening directives:

- `NoNewPrivileges=yes`
- `ProtectSystem=strict` (read-only `/usr`, `/boot`, `/efi`)
- `ProtectKernelTunables=yes`
- `ProtectKernelModules=yes`
- `ProtectKernelLogs=yes`
- `ProtectControlGroups=yes`
- `ProtectClock=yes`
- `RestrictNamespaces=yes`
- `RestrictRealtime=yes`
- `RestrictSUIDSGID=yes`
- `PrivateTmp=yes`
- `ReadWritePaths=/var/lib/facelock`
- `SupplementaryGroups=video`
- `SystemCallFilter=@system-service`

## Alternatives Considered

### Full hardening with workarounds

Enable all hardening directives and work around each conflict:

- Pass explicit device paths via configuration instead of auto-detection.
- Send notifications through a separate unprivileged helper process.
- Disable ONNX JIT and accept slower inference.

Rejected because each workaround introduces fragility. Hardcoded device paths
break when cameras are reconnected. A notification helper adds a new binary, a
new service, and IPC between them. Disabling JIT may not even be possible with
the current `ort` version and would measurably degrade performance.

### No hardening

Run the service with default systemd settings and no sandboxing. Rejected as
unacceptable for a PAM authentication module that runs as root and handles
biometric data. Even partial hardening significantly reduces the attack surface.

## Consequences

- **Reasonable security posture.** Score of ~5.4 is meaningfully hardened for a
  daemon that requires camera hardware, GPU/JIT, and user session access. For
  comparison, many desktop services score 7-9.
- **Documented exclusions.** Each omitted directive has a clear, testable
  justification. If upstream changes (e.g., ONNX Runtime drops JIT, or V4L2
  gains a cgroup-aware enumeration API), the corresponding directive can be
  re-enabled.
- **Notification reliability.** Using `InaccessiblePaths` over `ProtectHome`
  preserves the ability to deliver desktop notifications without architectural
  changes to the notification system.
- **Camera reliability.** Auto-detection continues to work across reboots and
  hot-plug events without configuration changes.

## References

- `dist/facelock-daemon.service` — systemd unit file
- `crates/facelock-cli/src/notifications.rs` — Desktop notification delivery
- `crates/facelock-camera/` — V4L2 camera auto-detection
- [systemd.exec(5) — Sandboxing](https://www.freedesktop.org/software/systemd/man/systemd.exec.html#Sandboxing)
- [systemd-analyze security](https://www.freedesktop.org/software/systemd/man/systemd-analyze.html)
