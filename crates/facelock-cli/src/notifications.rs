//! Desktop and terminal [`Notifier`] implementations (D9).
//!
//! The vocabulary ([`NotifyEvent`]) and the seam ([`Notifier`]) live in
//! `facelock_core::notify`; this module owns rendering and delivery. The
//! privilege-drop mechanics in [`send_as_user`] were hard-won against the
//! daemon's systemd hardening — wrap them, don't rewrite them.

use facelock_core::notify::{Notifier, NotifierFactory, NotifyEvent};
use tracing::debug;

use crate::message::{Message, NotifyMessage};

/// Body text for the notification: user-facing, so it renders through the
/// message seam (gettext at this edge, D10). The `reason` inside
/// `NotifyEvent::Failure` is still free text composed upstream — making it
/// structured is an H4 vocabulary follow-up.
fn body(event: &NotifyEvent) -> String {
    NotifyMessage::from(event).localized()
}

/// Freedesktop icon name for the notification.
fn icon(event: &NotifyEvent) -> &'static str {
    match event {
        NotifyEvent::Scanning => "camera-web",
        NotifyEvent::Success { .. } => "security-high",
        NotifyEvent::Failure { .. } => "security-low",
    }
}

/// Timeout in milliseconds for the notification.
fn timeout_ms(event: &NotifyEvent) -> i32 {
    match event {
        NotifyEvent::Scanning => 2000,
        NotifyEvent::Success { .. } | NotifyEvent::Failure { .. } => 3000,
    }
}

/// Resolve the original (non-root) user for privilege de-escalation.
/// Returns the username from SUDO_USER or DOAS_USER if running under sudo/doas,
/// in the shared resolver's precedence.
fn original_user() -> Option<String> {
    crate::ipc_client::invoking_user()
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
            "Facelock",                                                        // app_name
            0u32,                                                              // replaces_id
            icon(event),                                                       // app_icon
            "Facelock",                                                        // summary
            body(event),                                                       // body
            Vec::<String>::new(),                                              // actions
            std::collections::HashMap::<String, zbus::zvariant::Value>::new(), // hints
            timeout_ms(event),                                                 // expire_timeout
        ),
    )?;

    Ok(())
}

/// Send a desktop notification for the given event.
///
/// Errors are logged at debug level and never propagated, so a notification
/// cannot fail the auth flow. It can, however, delay it: the delivery below
/// runs a child process and waits for it.
///
/// When running as root (via sudo), D-Bus rejects connections from UID 0 to the
/// user's session bus. We handle this by resolving the original user's session bus
/// address and connecting directly, after dropping privileges with setpriv.
fn send_notification(event: &NotifyEvent) {
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
/// Uses `setpriv` to become the target user (real+effective uid/gid plus
/// supplementary groups) and run `notify-send` in that context. `setpriv`
/// does NOT open a PAM session, unlike `runuser`/`su`. That matters because
/// the daemon runs under a hardened systemd sandbox: `runuser` triggers a
/// full PAM login stack (pam_systemd trying to register a logind session,
/// pam_limits trying to adjust rlimits, etc.), and those modules fail under
/// the sandbox's restrictions, silently killing the notification. `setpriv`
/// only needs the CAP_SETUID/CAP_SETGID capabilities the daemon already
/// retains (see commit e006edc) and performs the uid/gid switch directly via
/// syscalls, with no session machinery to fail.
///
/// Falls back to `runuser` if `setpriv` is not available (e.g. non-systemd
/// environments without the hardened sandbox, where a PAM session is fine).
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
    let gid = user_info.gid.as_raw();
    let bus_addr = format!("unix:path=/run/user/{uid}/bus");
    let timeout = timeout_ms(event).to_string();

    // Build the notify-send command string
    let notify_cmd = format!(
        "DBUS_SESSION_BUS_ADDRESS='{}' notify-send --app-name Facelock -i '{}' -t {} Facelock '{}'",
        bus_addr,
        icon(event),
        timeout,
        body(event).replace('\'', "'\\''"),
    );

    // Try setpriv first: it switches real+effective uid/gid and supplementary
    // groups without opening a PAM session, so it works under the hardened
    // systemd sandbox. `--ambient-caps -all` defensively strips the daemon's
    // ambient CAP_SETUID/CAP_SETGID from the child (the kernel also clears
    // them on the uid change, but we drop them explicitly too).
    let setpriv_args: Vec<String> = vec![
        "--reuid".into(),
        uid.to_string(),
        "--regid".into(),
        gid.to_string(),
        "--init-groups".into(),
        "--ambient-caps".into(),
        "-all".into(),
        "--".into(),
        "sh".into(),
        "-c".into(),
        notify_cmd.clone(),
    ];

    let result = Command::new("setpriv")
        .args(&setpriv_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .or_else(|_| {
            // setpriv missing entirely (unlikely on util-linux systems): fall
            // back to runuser, which is fine outside the hardened sandbox.
            Command::new("runuser")
                .args(["-u", user, "--", "sh", "-c", &notify_cmd])
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

/// Desktop popup delivery. Pure delivery — config filtering happens at the
/// decision site via [`facelock_core::notify::notify_desktop_if_enabled`].
pub struct DesktopNotifier {
    /// `None`: notify the invoking user's session ([`send_notification`] —
    /// session bus directly, or setpriv via SUDO_USER/DOAS_USER when root).
    /// `Some(user)`: notify that user's session ([`send_as_user`] — the
    /// daemon path, which runs as root with no SUDO_USER).
    target_user: Option<String>,
}

impl DesktopNotifier {
    /// Notify the session of whoever invoked this process.
    pub fn for_current_session() -> Self {
        Self { target_user: None }
    }

    /// Notify a specific user's session. Used by the daemon.
    pub fn for_user(user: impl Into<String>) -> Self {
        Self {
            target_user: Some(user.into()),
        }
    }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, event: &NotifyEvent) {
        match &self.target_user {
            Some(user) => send_as_user(user, event),
            None => send_notification(event),
        }
    }
}

/// The production [`NotifierFactory`] for the daemon: per-request, per-user
/// desktop delivery. Injected into `commands::daemon::run` from `main` so the
/// server code never names this module.
pub fn daemon_notifier_factory() -> NotifierFactory {
    std::sync::Arc::new(|user: &str| Box::new(DesktopNotifier::for_user(user)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #213: the de-escalation target comes from the shared resolver's
    /// elevation half — SUDO_USER, then DOAS_USER — and is `None` when neither
    /// is set, even though `$USER` is: as root with no invoking user there is
    /// no session bus to reach, so the notification is skipped rather than
    /// sent as the current user.
    #[test]
    fn original_user_precedence_is_the_shared_resolvers() {
        let both = |key: &str| match key {
            "SUDO_USER" => Some("notify-alice".to_string()),
            "DOAS_USER" => Some("notify-dave".to_string()),
            "USER" => Some("root".to_string()),
            _ => None,
        };
        assert_eq!(
            crate::ipc_client::invoking_user_from_env(both).as_deref(),
            Some("notify-alice")
        );

        let doas = |key: &str| match key {
            "DOAS_USER" => Some("notify-dave".to_string()),
            "USER" => Some("root".to_string()),
            _ => None,
        };
        assert_eq!(
            crate::ipc_client::invoking_user_from_env(doas).as_deref(),
            Some("notify-dave")
        );

        let neither = |key: &str| match key {
            "USER" => Some("root".to_string()),
            _ => None,
        };
        assert_eq!(crate::ipc_client::invoking_user_from_env(neither), None);
    }

    #[test]
    fn scanning_event_rendering() {
        let event = NotifyEvent::Scanning;
        assert_eq!(body(&event), "Scanning face...");
        assert_eq!(icon(&event), "camera-web");
        assert_eq!(timeout_ms(&event), 2000);
    }

    #[test]
    fn success_event_with_label() {
        let event = NotifyEvent::Success {
            label: Some("Alice".to_string()),
            similarity: 0.87,
        };
        assert_eq!(body(&event), "Welcome, Alice");
        assert_eq!(icon(&event), "security-high");
        assert_eq!(timeout_ms(&event), 3000);
    }

    #[test]
    fn success_event_without_label() {
        let event = NotifyEvent::Success {
            label: None,
            similarity: 0.87,
        };
        assert_eq!(body(&event), "Face recognized (0.87)");
    }

    #[test]
    fn failure_event_rendering() {
        let event = NotifyEvent::Failure {
            reason: "no match".to_string(),
        };
        assert_eq!(body(&event), "Face auth failed: no match");
        assert_eq!(icon(&event), "security-low");
        assert_eq!(timeout_ms(&event), 3000);
    }

    /// The factory hands out per-user desktop notifiers targeting the given
    /// session — pin the plumbing without touching a real bus.
    #[test]
    fn factory_targets_the_requested_user() {
        let notifier = DesktopNotifier::for_user("alice");
        assert_eq!(notifier.target_user.as_deref(), Some("alice"));
        assert!(DesktopNotifier::for_current_session().target_user.is_none());
    }
}
