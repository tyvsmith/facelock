use facelock_core::config::NotificationConfig;
use tracing::debug;

/// Events that can trigger desktop notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum NotifyEvent {
    /// Face scanning has started.
    Scanning,
    /// Face recognition succeeded.
    Success {
        label: Option<String>,
        similarity: f32,
    },
    /// Face recognition failed.
    Failure { reason: String },
}

impl NotifyEvent {
    /// Title for the notification.
    pub fn title(&self) -> &str {
        "Facelock"
    }

    /// Body text for the notification.
    pub fn body(&self) -> String {
        match self {
            NotifyEvent::Scanning => "Scanning face...".to_string(),
            NotifyEvent::Success { label, similarity } => {
                if let Some(label) = label {
                    format!("Welcome, {label}")
                } else {
                    format!("Face recognized ({similarity:.2})")
                }
            }
            NotifyEvent::Failure { reason } => format!("Face auth failed: {reason}"),
        }
    }

    /// Freedesktop icon name for the notification.
    pub fn icon(&self) -> &str {
        match self {
            NotifyEvent::Scanning => "camera-web",
            NotifyEvent::Success { .. } => "security-high",
            NotifyEvent::Failure { .. } => "security-low",
        }
    }

    /// Timeout in milliseconds for the notification.
    pub fn timeout_ms(&self) -> i32 {
        match self {
            NotifyEvent::Scanning => 2000,
            NotifyEvent::Success { .. } | NotifyEvent::Failure { .. } => 3000,
        }
    }
}

/// Resolve the original (non-root) user for privilege de-escalation.
/// Returns the username from SUDO_USER or DOAS_USER if running under sudo/doas.
fn original_user() -> Option<String> {
    std::env::var("SUDO_USER")
        .ok()
        .or_else(|| std::env::var("DOAS_USER").ok())
}

/// Send a desktop notification via D-Bus org.freedesktop.Notifications.
///
/// Connects to the session bus and calls the standard Notify method.
fn send_notification_dbus(event: &NotifyEvent) -> anyhow::Result<()> {
    let connection = zbus::blocking::Connection::session()?;

    let proxy = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?;

    let _: u32 = proxy.call(
        "Notify",
        &(
            "Facelock",                    // app_name
            0u32,                          // replaces_id
            event.icon(),                  // app_icon
            "Facelock",                    // summary
            event.body(),                  // body
            Vec::<String>::new(),          // actions
            std::collections::HashMap::<String, zbus::zvariant::Value>::new(), // hints
            event.timeout_ms(),            // expire_timeout
        ),
    )?;

    Ok(())
}

/// Send a desktop notification for the given event.
///
/// This is fire-and-forget: errors are logged at debug level but never propagated.
/// Notifications should never block or fail the auth flow.
///
/// When running as root (via sudo), D-Bus rejects connections from UID 0 to the
/// user's session bus. We handle this by resolving the original user's session bus
/// address and connecting directly, after dropping privileges with setpriv.
pub fn send_notification(event: &NotifyEvent) {
    debug!(?event, "sending desktop notification");

    if nix::unistd::Uid::current().is_root() {
        if let Some(user) = original_user() {
            send_as_user(&user, event);
        } else {
            debug!("running as root with no SUDO_USER/DOAS_USER, skipping notification");
        }
        return;
    }

    match send_notification_dbus(event) {
        Ok(()) => debug!("notification sent via D-Bus"),
        Err(e) => debug!("notification failed: {e}"),
    }
}

/// Send notification as a specific user by dropping privileges with setpriv.
///
/// Uses setpriv to drop to the target user and run a small helper that connects
/// to the user's session D-Bus and sends the notification via the standard
/// Notifications interface. This avoids shelling out to notify-send.
fn send_as_user(user: &str, event: &NotifyEvent) {
    use std::process::Command;

    // Resolve the user's UID/GID for setpriv and DBUS_SESSION_BUS_ADDRESS
    let user_info = match nix::unistd::User::from_name(user) {
        Ok(Some(u)) => u,
        _ => {
            debug!(user, "could not resolve user for notification");
            return;
        }
    };
    let uid = user_info.uid.as_raw();
    let gid = user_info.gid.as_raw();

    let bus_path = format!("/run/user/{uid}/bus");
    if !std::path::Path::new(&bus_path).exists() {
        debug!(user, bus_path, "D-Bus session bus not found, skipping notification");
        return;
    }

    let bus_addr = format!("unix:path={bus_path}");
    let uid_str = uid.to_string();
    let gid_str = gid.to_string();

    // Use gdbus call to invoke the Notifications interface directly.
    // This avoids depending on notify-send and uses the same D-Bus API.
    let body = event.body();
    let icon = event.icon();
    let timeout = event.timeout_ms().to_string();

    // gdbus call syntax for org.freedesktop.Notifications.Notify:
    // (app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout)
    let notify_args = format!(
        "('Facelock', uint32 0, '{}', 'Facelock', '{}', @as [], @a{{sv}} {{}}, int32 {})",
        escape_gvariant(icon),
        escape_gvariant(&body),
        timeout,
    );

    let result = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid", &uid_str,
            "--regid", &gid_str,
            "--init-groups",
            "--",
            "gdbus", "call",
            "--session",
            "--dest", "org.freedesktop.Notifications",
            "--object-path", "/org/freedesktop/Notifications",
            "--method", "org.freedesktop.Notifications.Notify",
            &notify_args,
        ])
        .env("DBUS_SESSION_BUS_ADDRESS", &bus_addr)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();

    match result {
        Ok(output) if output.status.success() => debug!(user, "notification sent via setpriv+gdbus"),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(user, %output.status, %stderr, "gdbus via setpriv failed");
        }
        Err(e) => debug!(user, %e, "failed to spawn setpriv"),
    }
}

/// Escape a string for use in a GVariant text format string.
/// Single quotes need to be escaped.
fn escape_gvariant(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Check whether a desktop notification should be sent for the given event.
pub fn should_notify_desktop(config: &NotificationConfig, event: &NotifyEvent) -> bool {
    if !config.desktop() {
        return false;
    }
    match event {
        NotifyEvent::Scanning => config.notify_prompt,
        NotifyEvent::Success { .. } => config.notify_on_success,
        NotifyEvent::Failure { .. } => config.notify_on_failure,
    }
}

/// Conditionally send a desktop notification based on config.
pub fn notify_if_enabled(config: &NotificationConfig, event: &NotifyEvent) {
    if should_notify_desktop(config, event) {
        send_notification(event);
    }
}

/// Conditionally send a desktop notification to a specific user's session.
/// Used by the daemon, which runs as root without SUDO_USER/DOAS_USER.
pub fn notify_if_enabled_for_user(
    config: &NotificationConfig,
    event: &NotifyEvent,
    user: &str,
) {
    if should_notify_desktop(config, event) {
        send_as_user(user, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanning_event_fields() {
        let event = NotifyEvent::Scanning;
        assert_eq!(event.title(), "Facelock");
        assert_eq!(event.body(), "Scanning face...");
        assert_eq!(event.icon(), "camera-web");
        assert_eq!(event.timeout_ms(), 2000);
    }

    #[test]
    fn success_event_with_label() {
        let event = NotifyEvent::Success {
            label: Some("Alice".to_string()),
            similarity: 0.87,
        };
        assert_eq!(event.body(), "Welcome, Alice");
        assert_eq!(event.icon(), "security-high");
        assert_eq!(event.timeout_ms(), 3000);
    }

    #[test]
    fn success_event_without_label() {
        let event = NotifyEvent::Success {
            label: None,
            similarity: 0.87,
        };
        assert_eq!(event.body(), "Face recognized (0.87)");
    }

    #[test]
    fn failure_event_fields() {
        let event = NotifyEvent::Failure {
            reason: "no match".to_string(),
        };
        assert_eq!(event.body(), "Face auth failed: no match");
        assert_eq!(event.icon(), "security-low");
        assert_eq!(event.timeout_ms(), 3000);
    }

    #[test]
    fn escape_gvariant_plain() {
        assert_eq!(escape_gvariant("hello"), "hello");
    }

    #[test]
    fn escape_gvariant_quotes() {
        assert_eq!(escape_gvariant("it's"), "it'\\''s");
    }

    use facelock_core::config::NotificationMode;

    #[test]
    fn mode_off_blocks_all() {
        let config = NotificationConfig {
            mode: NotificationMode::Off,
            ..Default::default()
        };
        assert!(!should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(!should_notify_desktop(
            &config,
            &NotifyEvent::Success { label: None, similarity: 0.9 }
        ));
    }

    #[test]
    fn mode_terminal_blocks_desktop() {
        let config = NotificationConfig {
            mode: NotificationMode::Terminal,
            ..Default::default()
        };
        assert!(!should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(config.terminal());
        assert!(!config.desktop());
    }

    #[test]
    fn mode_desktop_enables_desktop() {
        let config = NotificationConfig {
            mode: NotificationMode::Desktop,
            ..Default::default()
        };
        assert!(should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(!config.terminal());
        assert!(config.desktop());
    }

    #[test]
    fn mode_both_enables_all() {
        let config = NotificationConfig::default();
        assert!(config.terminal());
        assert!(config.desktop());
        assert!(should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(should_notify_desktop(
            &config,
            &NotifyEvent::Success { label: None, similarity: 0.5 }
        ));
        assert!(should_notify_desktop(
            &config,
            &NotifyEvent::Failure { reason: "err".into() }
        ));
    }

    #[test]
    fn notify_prompt_controls_scanning() {
        let config = NotificationConfig {
            notify_prompt: false,
            ..Default::default()
        };
        assert!(!should_notify_desktop(&config, &NotifyEvent::Scanning));
        // Success/failure still enabled
        assert!(should_notify_desktop(
            &config,
            &NotifyEvent::Success { label: None, similarity: 0.9 }
        ));
    }

    #[test]
    fn notify_on_success_controls_success() {
        let config = NotificationConfig {
            notify_on_success: false,
            ..Default::default()
        };
        assert!(should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(!should_notify_desktop(
            &config,
            &NotifyEvent::Success { label: None, similarity: 0.9 }
        ));
        assert!(should_notify_desktop(
            &config,
            &NotifyEvent::Failure { reason: "no match".into() }
        ));
    }

    #[test]
    fn notify_on_failure_controls_failure() {
        let config = NotificationConfig {
            notify_on_failure: false,
            ..Default::default()
        };
        assert!(should_notify_desktop(
            &config,
            &NotifyEvent::Success { label: Some("Bob".into()), similarity: 0.8 }
        ));
        assert!(!should_notify_desktop(
            &config,
            &NotifyEvent::Failure { reason: "timeout".into() }
        ));
    }
}
