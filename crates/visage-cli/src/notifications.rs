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

/// Send a desktop notification for the given event.
///
/// This is fire-and-forget: errors are logged at debug level but never propagated.
/// Notifications should never block or fail the auth flow.
pub fn send_notification(event: &NotifyEvent) {
    debug!(?event, "sending desktop notification");
    let _ = Notification::new()
        .summary(event.title())
        .body(&event.body())
        .icon(event.icon())
        .timeout(event.timeout_ms())
        .show();
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
