mod font;
mod render;
mod text_only;
#[cfg(feature = "wayland")]
mod wayland_preview;
#[cfg(feature = "wayland")]
mod wayland_socket;

use facelock_core::Config;

use crate::backend::{Backend, BackendKind};
use crate::ipc_client;
use crate::message::{AccessMessage, Terminal};

pub fn run(config: &Config, json: bool, user: Option<String>) -> anyhow::Result<()> {
    // DEC-6/N13: `PreviewDetectFrame` is root-only now — it was the last
    // unprivileged consumer of a per-frame similarity score (the
    // hill-climbing oracle N12/N13 close by construction). Root is
    // established by `main`'s `require_root_for` gate (C6).

    // One user-resolution implementation (C5, issue #105). The local
    // getpwuid-only version this replaces resolved `sudo facelock preview`
    // to *root*, so the preview never recognized the actual user.
    let user = ipc_client::resolve_user(user.as_deref());

    let backend = Backend::select(config);

    if !backend.caps().graphical_preview {
        // A direct backend has only the local text preview — a capability
        // stated by the selection, not discovered at a failure site. Why it
        // is unavailable depends on the kind: a stopped daemon is a degraded
        // state, not a configuration choice (the old single message blamed
        // "oneshot mode" for both).
        if !json {
            Terminal.error(&graphical_unavailable_message(backend.kind()));
        }
        return text_only::run_direct(config, &user);
    }

    if json {
        return text_only::run(&user);
    }

    run_graphical(&user)
}

/// Why graphical preview is unavailable on a direct backend, accurately per
/// kind.
fn graphical_unavailable_message(kind: BackendKind) -> AccessMessage {
    match kind {
        BackendKind::DirectByFallback => AccessMessage::PreviewGraphicalDaemonUnreachable,
        BackendKind::DirectByOverride => AccessMessage::PreviewGraphicalDaemonConfigOverride,
        _ => AccessMessage::PreviewGraphicalNeedsDaemonOneshot,
    }
}

#[cfg(feature = "wayland")]
fn run_graphical(user: &str) -> anyhow::Result<()> {
    match wayland_preview::run(user) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("Wayland preview failed: {e}");
            eprintln!(
                "Wayland preview unavailable: {e}\n\
                 Falling back to text-only mode.\n"
            );
            text_only::run(user)
        }
    }
}

#[cfg(not(feature = "wayland"))]
fn run_graphical(user: &str) -> anyhow::Result<()> {
    eprintln!(
        "Graphical preview not available (compiled without wayland feature).\n\
         Using text-only mode.\n"
    );
    text_only::run(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The misattribution pin (D1): a *stopped* daemon must render the
    /// degraded fallback message, never the "configured off" oneshot wording
    /// — the old single message told users with a crashed daemon that they
    /// had chosen oneshot mode.
    #[test]
    fn stopped_daemon_is_not_blamed_on_configuration() {
        assert_eq!(
            graphical_unavailable_message(BackendKind::DirectByFallback),
            AccessMessage::PreviewGraphicalDaemonUnreachable
        );
        assert_eq!(
            graphical_unavailable_message(BackendKind::DirectByConfig),
            AccessMessage::PreviewGraphicalNeedsDaemonOneshot
        );
        // Likewise a daemon bypassed under `--config` (#314) is neither
        // stopped nor configured off.
        assert_eq!(
            graphical_unavailable_message(BackendKind::DirectByOverride),
            AccessMessage::PreviewGraphicalDaemonConfigOverride
        );
    }

    /// C5 (issue #105): preview resolves its user through the one shared
    /// resolver, whose precedence honors SUDO_USER — the deleted local
    /// implementation used getpwuid only, so `sudo facelock preview`
    /// previewed for root and never recognized anyone. The euid-0 half of
    /// the bug cannot be reproduced in a unit test; what this pins is the
    /// resolver's SUDO_USER precedence, which is exactly the behavior the
    /// getpwuid-only version lacked.
    #[test]
    fn shared_resolver_honors_sudo_user() {
        // The environment is injected rather than set: `set_var` is global to
        // the test binary and cargo runs these threaded, so the real thing
        // would be safe only by convention.
        let env = |key: &str| match key {
            "SUDO_USER" => Some("preview-c5-alice".to_string()),
            "USER" => Some("root".to_string()),
            _ => None,
        };

        assert_eq!(
            crate::ipc_client::resolve_user_from_env(None, env),
            "preview-c5-alice"
        );

        // The explicit flag still wins over the environment.
        assert_eq!(
            crate::ipc_client::resolve_user_from_env(Some("bob"), env),
            "bob"
        );
    }
}
