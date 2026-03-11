use visage_core::config::NotificationConfig;
use notify_rust::Notification;
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
        "Visage"
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

/// Send a desktop notification for the given event.
///
/// This is fire-and-forget: errors are logged at debug level but never propagated.
/// Notifications should never block or fail the auth flow.
///
/// When running as root (via sudo), D-Bus rejects connections from UID 0 to the
/// user's session bus. We handle this by either:
/// - Sending directly if we're the session owner (not root)
/// - Dropping to the original user via runuser/sudo -u for the notification
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

    match Notification::new()
        .summary(event.title())
        .body(&event.body())
        .icon(event.icon())
        .timeout(event.timeout_ms())
        .show()
    {
        Ok(_handle) => debug!("notification sent"),
        Err(e) => debug!("notification failed: {e}"),
    }
}

/// Send notification as a specific user by dropping privileges with setpriv.
///
/// Uses setpriv instead of runuser/su because those tools open PAM sessions,
/// which deadlocks when called from within a PAM authentication (e.g., during sudo).
/// setpriv directly changes UID/GID via setuid(2) without touching PAM.
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
    let timeout_str = format!("{}", event.timeout_ms());
    let uid_str = uid.to_string();
    let gid_str = gid.to_string();

    // setpriv changes UID/GID without opening a PAM session, avoiding
    // deadlocks when called from within PAM auth (sudo, login, etc.)
    let result = Command::new("/usr/bin/setpriv")
        .args(["--reuid", &uid_str, "--regid", &gid_str, "--init-groups",
               "--", "/usr/bin/notify-send",
               "--app-name", "Visage",
               "-i", event.icon(),
               "-t", &timeout_str,
               event.title(),
               &event.body()])
        .env("DBUS_SESSION_BUS_ADDRESS", &bus_addr)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();

    match result {
        Ok(output) if output.status.success() => debug!(user, "notification sent via setpriv"),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(user, %output.status, %stderr, "notify-send via setpriv failed");
        }
        Err(e) => debug!(user, %e, "failed to spawn setpriv"),
    }
}

/// Check whether a notification should be sent for the given event, based on config.
pub fn should_notify(config: &NotificationConfig, event: &NotifyEvent) -> bool {
    if !config.enabled {
        return false;
    }
    match event {
        NotifyEvent::Scanning => true,
        NotifyEvent::Success { .. } => config.on_success,
        NotifyEvent::Failure { .. } => config.on_failure,
    }
}

/// Conditionally send a notification based on config.
pub fn notify_if_enabled(config: &NotificationConfig, event: &NotifyEvent) {
    if should_notify(config, event) {
        send_notification(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanning_event_fields() {
        let event = NotifyEvent::Scanning;
        assert_eq!(event.title(), "Visage");
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
    fn should_notify_respects_enabled() {
        let config = NotificationConfig {
            enabled: false,
            on_success: true,
            on_failure: true,
        };
        assert!(!should_notify(&config, &NotifyEvent::Scanning));
        assert!(!should_notify(
            &config,
            &NotifyEvent::Success {
                label: None,
                similarity: 0.9
            }
        ));
    }

    #[test]
    fn should_notify_respects_on_success() {
        let config = NotificationConfig {
            enabled: true,
            on_success: false,
            on_failure: true,
        };
        // Scanning always goes through when enabled
        assert!(should_notify(&config, &NotifyEvent::Scanning));
        // Success is suppressed
        assert!(!should_notify(
            &config,
            &NotifyEvent::Success {
                label: None,
                similarity: 0.9
            }
        ));
        // Failure still goes through
        assert!(should_notify(
            &config,
            &NotifyEvent::Failure {
                reason: "no match".into()
            }
        ));
    }

    #[test]
    fn should_notify_respects_on_failure() {
        let config = NotificationConfig {
            enabled: true,
            on_success: true,
            on_failure: false,
        };
        assert!(should_notify(
            &config,
            &NotifyEvent::Success {
                label: Some("Bob".into()),
                similarity: 0.8
            }
        ));
        assert!(!should_notify(
            &config,
            &NotifyEvent::Failure {
                reason: "timeout".into()
            }
        ));
    }

    #[test]
    fn should_notify_all_enabled() {
        let config = NotificationConfig::default();
        assert!(should_notify(&config, &NotifyEvent::Scanning));
        assert!(should_notify(
            &config,
            &NotifyEvent::Success {
                label: None,
                similarity: 0.5
            }
        ));
        assert!(should_notify(
            &config,
            &NotifyEvent::Failure {
                reason: "err".into()
            }
        ));
    }
}
