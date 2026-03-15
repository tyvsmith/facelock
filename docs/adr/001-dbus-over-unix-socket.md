# ADR 001: D-Bus over Unix Socket for IPC

## Status

Accepted

## Date

2026-03-11

## Context

Facelock originally used a custom Unix socket with bincode serialization for IPC
between the PAM module and the daemon. While functional, this approach accumulated
technical debt:

- The PAM module contained approximately 200 lines of inline bincode
  serialization/deserialization logic.
- Desktop notifications required a fork+exec hack to invoke `notify-send`, since
  the daemon had no standard way to broadcast events.
- Polkit and logind integration both require D-Bus; any interaction with the
  desktop session bus was impossible without it.
- No mechanism for broadcast events (e.g., enrollment complete, auth status
  changes) to multiple listeners.

Analysis of planned features showed that 3 of 5 upcoming capabilities required
D-Bus connectivity regardless. Maintaining two parallel IPC protocols would
compound complexity.

## Decision

Replace the custom Unix socket + bincode protocol with D-Bus system bus
communication under the well-known name `org.facelock.Daemon`.

## Alternatives Considered

### Keep Unix socket + add D-Bus facade

Run both protocols simultaneously: Unix socket for PAM (backward compatibility)
and D-Bus for desktop integration. Rejected because maintaining two IPC
mechanisms doubles the protocol surface area indefinitely, and the PAM module
would still carry the bincode dependency.

### gRPC

Use gRPC with Unix domain socket transport. Rejected as overkill for a
single-machine daemon. gRPC is not a Linux desktop standard, would add protobuf
compilation to the build, and provides no integration benefits with polkit,
logind, or session management.

## Consequences

- **PAM module gains zbus dependency** (~5-10 MB binary size increase). This is
  acceptable given the elimination of hand-rolled serialization.
- **Runtime dependency on dbus-daemon.** All supported Linux desktops ship
  dbus-daemon; headless servers may need explicit installation.
- **fprintd-compatible patterns.** The D-Bus interface follows conventions
  established by fprintd, making Facelock a natural complement in multi-factor
  biometric stacks.
- **Debugging via busctl.** Administrators can inspect, call, and monitor the
  daemon interface with standard tools (`busctl`, `gdbus`, `d-feet`).
- **D-Bus activation replaces socket activation.** The daemon can be started on
  first method call via a `.service` file in `/usr/share/dbus-1/system-services/`,
  eliminating the need for custom socket activation logic.

## References

- [fprintd D-Bus API](https://fprint.freedesktop.org/fprintd/fprintd-docs.html)
- Sovren Visage competitive analysis (internal)
- [D-Bus specification](https://dbus.freedesktop.org/doc/dbus-specification.html)
