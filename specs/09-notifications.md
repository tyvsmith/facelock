# Spec 09: Desktop Notifications

**Phase**: 5 (Polish) | **Crate**: visage-cli | **Depends on**: 07 | **Parallel with**: 08, 10

## Goal

D-Bus desktop notifications for face recognition events. Notifications fire from CLI commands that perform auth (test, and indirectly from the daemon for PAM-triggered auth).

## Dependencies

- `notify-rust` (already in visage-cli)

## Design

### Notification Events

| Event | Title | Body | Icon | Timeout |
|-------|-------|------|------|---------|
| Scanning | "Visage" | "Scanning face..." | `camera-web` | 2s |
| Success | "Visage" | "Welcome, {label}" or "Face recognized (0.87)" | `security-high` | 3s |
| Failure | "Visage" | "Face auth failed: {reason}" | `security-low` | 3s |

### Implementation

```rust
use notify_rust::Notification;

pub fn send_notification(event: &NotifyEvent) {
    // Fire-and-forget: silently ignore errors
    let _ = Notification::new()
        .summary(&event.title)
        .body(&event.body)
        .icon(&event.icon)
        .timeout(event.timeout_ms as i32)
        .show();
}

pub enum NotifyEvent {
    Scanning,
    Success { label: Option<String>, similarity: f32 },
    Failure { reason: String },
}
```

### Integration Points

- `visage test`: send Scanning before auth, Success/Failure after result
- Config controlled: `notification.enabled`, `notification.on_success`, `notification.on_failure`
- Notifications are advisory only -- never block on them

### D-Bus Session

Notifications use the user's D-Bus session (via `DBUS_SESSION_BUS_ADDRESS`). Works with mako, dunst, swaync, and any freedesktop-compliant notification daemon.

## Tests

- NotifyEvent construction
- Config flag checking logic

## Acceptance Criteria

1. Notifications appear during `visage test` on a Wayland session
2. Controlled by config flags
3. Errors silently ignored (no crash on missing notification daemon)
4. Standard freedesktop icons used

## Verification

```bash
cargo build -p visage-cli
# Manual: cargo run --bin visage -- test  (observe notifications)
```
