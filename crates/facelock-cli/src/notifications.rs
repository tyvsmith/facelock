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

/// Send notification as a specific user.
///
/// Uses `runuser` to run `notify-send` as the target user with a proper
/// login environment. This works even from systemd services where the
/// daemon's mount namespace may not include `/run/user/<uid>/`.
///
/// Falls back to `su -c` if `runuser` is not available.
fn send_as_user(user: &str, event: &NotifyEvent) {
    use std::process::Command;

    let user_info = match nix::unistd::User::from_name(user) {
        Ok(Some(u)) => u,
        _ => {
            debug!(user, "could not resolve user for notification");
            return;
        }
    };
    let uid = user_info.uid.as_raw();
    let bus_addr = format!("unix:path=/run/user/{uid}/bus");
    let timeout = event.timeout_ms().to_string();

    // Build the notify-send command string
    let notify_cmd = format!(
        "DBUS_SESSION_BUS_ADDRESS='{}' notify-send --app-name Facelock -i '{}' -t {} Facelock '{}'",
        bus_addr,
        event.icon(),
        timeout,
        event.body().replace('\'', "'\\''"),
    );

    // Try runuser first (available on most systems, works from systemd services),
    // fall back to su -c
    let result = Command::new("runuser")
        .args(["-u", user, "--", "sh", "-c", &notify_cmd])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .or_else(|_| {
            Command::new("su")
                .args(["-", user, "-c", &notify_cmd])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output()
        });

    match result {
        Ok(output) if output.status.success() => {
            tracing::info!(user, "desktop notification sent");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(user, %output.status, %stderr, "notify-send failed");
        }
        Err(e) => tracing::warn!(user, %e, "failed to send notification"),
    }
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
    if !should_notify_desktop(config, event) {
        debug!(?event, "notification filtered by config");
        return;
    }
    send_as_user(user, event);
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

    /// Helper: config with desktop notifications fully enabled
    fn desktop_config() -> NotificationConfig {
        NotificationConfig {
            mode: NotificationMode::Both,
            notify_prompt: true,
            notify_on_success: true,
            notify_on_failure: true,
        }
    }

    #[test]
    fn default_is_terminal_only() {
        let config = NotificationConfig::default();
        assert!(config.terminal());
        assert!(!config.desktop());
        assert!(!config.notify_on_failure);
    }

    #[test]
    fn mode_both_enables_all() {
        let config = desktop_config();
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
            ..desktop_config()
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
            ..desktop_config()
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
            ..desktop_config()
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
