//! Notification vocabulary and delivery seam (D9).
//!
//! [`NotifyEvent`] is the shared vocabulary for "tell a human something
//! happened"; [`Notifier`] is the delivery seam. Events carry data only —
//! rendering (body text, icon, timeout) belongs to the implementation, which
//! is also where the i18n boundary will eventually sit (the sink decides
//! presentation).
//!
//! Implementations live where their delivery mechanics live: the desktop and
//! terminal notifiers are in `facelock-cli` (`notifications.rs`), next to the
//! hard-won `setpriv` privilege-drop machinery. This module stays free of new
//! dependencies so that any crate — including the daemon D-Bus server after
//! its planned move out of `facelock-cli` — can reach the seam.
//!
//! The PAM module (`pam-facelock`) cannot depend on this crate; its
//! `PamNotificationConfig` mirrors the `[notification]` schema and gates its
//! terminal conversation text on the same flags (`notify_prompt` ↔
//! [`NotifyEvent::Scanning`], `notify_on_success` ↔
//! [`NotifyEvent::Success`]). Keep the vocabularies in lockstep by hand.

use crate::config::NotificationConfig;

/// Something that happened which a human may want to hear about.
///
/// Variants enumerate what the code actually sends today; carry the data and
/// let the [`Notifier`] implementation render it.
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

/// Delivery seam: something that can tell a human about a [`NotifyEvent`].
///
/// The contract is that delivery must never *fail* the auth flow: an
/// implementation logs its errors and returns normally, and `notify` has no
/// return value precisely so a caller cannot branch on one.
///
/// It is not "fire-and-forget". Implementations may block, and the desktop
/// one does — it runs `setpriv` + `notify-send` and waits for the child —
/// which is why the daemon sends notifications after releasing the capture
/// slot but still before returning its reply. Keep delivery cheap.
pub trait Notifier {
    fn notify(&self, event: &NotifyEvent);
}

/// A [`Notifier`] that discards every event. For tests and for contexts
/// where notifications are structurally impossible.
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn notify(&self, _event: &NotifyEvent) {}
}

/// Builds a [`Notifier`] targeting a specific user's session.
///
/// The daemon needs this indirection: it runs as root with no
/// `SUDO_USER`, and learns the target user per request — so it is handed a
/// factory at startup instead of a single notifier, keeping the delivery
/// implementation (and its `setpriv` mechanics) out of the server code.
pub type NotifierFactory = std::sync::Arc<dyn Fn(&str) -> Box<dyn Notifier> + Send + Sync>;

/// Whether a desktop notification should be sent for `event` under `config`.
pub fn should_notify_desktop(config: &NotificationConfig, event: &NotifyEvent) -> bool {
    if !config.desktop() {
        return false;
    }
    match event {
        NotifyEvent::Scanning => config.notify_prompt.enabled(false),
        NotifyEvent::Success { .. } => config.notify_on_success.enabled(false),
        NotifyEvent::Failure { .. } => config.notify_on_failure.enabled(false),
    }
}

/// Deliver `event` through `notifier` iff the desktop channel is enabled for
/// it; otherwise log at debug level and do nothing.
pub fn notify_desktop_if_enabled(
    config: &NotificationConfig,
    notifier: &dyn Notifier,
    event: &NotifyEvent,
) {
    if should_notify_desktop(config, event) {
        notifier.notify(event);
    } else {
        tracing::debug!(?event, "notification filtered by config");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotificationMode;

    /// Minimal in-crate recorder (facelock-test-support has the shared
    /// `RecordingNotifier`, but depending on it from here would be a
    /// dev-dependency cycle).
    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<NotifyEvent>>);

    impl Notifier for Recorder {
        fn notify(&self, event: &NotifyEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    /// Helper: config with desktop notifications fully enabled
    fn desktop_config() -> NotificationConfig {
        NotificationConfig {
            mode: NotificationMode::Both,
            notify_prompt: true.into(),
            notify_on_success: true.into(),
            notify_on_failure: true.into(),
        }
    }

    fn success() -> NotifyEvent {
        NotifyEvent::Success {
            label: None,
            similarity: 0.9,
        }
    }

    fn failure() -> NotifyEvent {
        NotifyEvent::Failure {
            reason: "no match".into(),
        }
    }

    #[test]
    fn mode_off_blocks_all() {
        let config = NotificationConfig {
            mode: NotificationMode::Off,
            ..desktop_config()
        };
        assert!(!should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(!should_notify_desktop(&config, &success()));
        assert!(!should_notify_desktop(&config, &failure()));
    }

    #[test]
    fn mode_terminal_blocks_desktop() {
        let config = NotificationConfig {
            mode: NotificationMode::Terminal,
            ..desktop_config()
        };
        assert!(!should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(config.terminal());
        assert!(!config.desktop());
    }

    #[test]
    fn mode_desktop_enables_desktop() {
        let config = NotificationConfig {
            mode: NotificationMode::Desktop,
            ..desktop_config()
        };
        assert!(should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(!config.terminal());
        assert!(config.desktop());
    }

    #[test]
    fn default_is_terminal_only() {
        let config = NotificationConfig::default();
        assert!(config.terminal());
        assert!(!config.desktop());
        assert!(!config.notify_on_failure.enabled(false));
    }

    #[test]
    fn mode_both_enables_all() {
        let config = desktop_config();
        assert!(config.terminal());
        assert!(config.desktop());
        assert!(should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(should_notify_desktop(&config, &success()));
        assert!(should_notify_desktop(&config, &failure()));
    }

    #[test]
    fn notify_prompt_controls_scanning() {
        let config = NotificationConfig {
            notify_prompt: false.into(),
            ..desktop_config()
        };
        assert!(!should_notify_desktop(&config, &NotifyEvent::Scanning));
        // Success/failure still enabled
        assert!(should_notify_desktop(&config, &success()));
    }

    #[test]
    fn notify_on_success_controls_success() {
        let config = NotificationConfig {
            notify_on_success: false.into(),
            ..desktop_config()
        };
        assert!(should_notify_desktop(&config, &NotifyEvent::Scanning));
        assert!(!should_notify_desktop(&config, &success()));
        assert!(should_notify_desktop(&config, &failure()));
    }

    #[test]
    fn notify_on_failure_controls_failure() {
        let config = NotificationConfig {
            notify_on_failure: false.into(),
            ..desktop_config()
        };
        assert!(should_notify_desktop(&config, &success()));
        assert!(!should_notify_desktop(&config, &failure()));
    }

    #[test]
    fn filtered_dispatch_delivers_iff_enabled() {
        let recorder = Recorder::default();
        notify_desktop_if_enabled(&desktop_config(), &recorder, &failure());
        assert_eq!(recorder.0.lock().unwrap().as_slice(), &[failure()]);

        let recorder = Recorder::default();
        notify_desktop_if_enabled(&NotificationConfig::default(), &recorder, &failure());
        assert!(recorder.0.lock().unwrap().is_empty());
    }

    #[test]
    fn null_notifier_accepts_events() {
        NullNotifier.notify(&NotifyEvent::Scanning);
    }
}
