use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::{Mutex, oneshot};
use zbus::connection::Builder;
use zbus::zvariant::Value;
use zbus::{Connection, fdo, interface};

use facelock_core::dbus_interface::{BUS_NAME, INTERFACE_NAME, OBJECT_PATH};

/// Polkit authentication agent that attempts face authentication via the
/// facelock daemon, falling back to declining (so another agent can handle
/// password auth).
struct PolkitAgent {
    system_conn: Connection,
    /// Tracks in-flight authentication attempts by cookie.
    /// Sending on the oneshot signals cancellation.
    active_auths: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

#[interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl PolkitAgent {
    async fn begin_authentication(
        &self,
        action_id: &str,
        message: &str,
        _icon_name: &str,
        _details: HashMap<String, String>,
        cookie: &str,
        identities: Vec<(String, HashMap<String, Value<'_>>)>,
    ) -> fdo::Result<()> {
        tracing::info!(action_id, message, "polkit auth request received");

        let user = extract_username(&identities).unwrap_or_else(current_username);

        // Set up cancellation channel for this auth attempt.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        {
            let mut auths = self.active_auths.lock().await;
            auths.insert(cookie.to_string(), cancel_tx);
        }

        // Race the face auth call against the cancellation signal.
        let face_result = tokio::select! {
            result = try_face_auth(&self.system_conn, &user) => Some(result),
            _ = cancel_rx => {
                tracing::info!(cookie, user = %user, "auth cancelled by polkit");
                None
            }
        };

        // Clean up the tracking entry regardless of outcome.
        {
            let mut auths = self.active_auths.lock().await;
            auths.remove(cookie);
        }

        match face_result {
            // Cancelled.
            None => Err(fdo::Error::Failed("authentication cancelled".to_string())),
            Some(Ok(true)) => {
                respond_to_polkit(&self.system_conn, cookie, &user)
                    .await
                    .map_err(|e| fdo::Error::Failed(format!("polkit response failed: {e}")))?;
                tracing::info!(user = %user, "face auth succeeded");
                Ok(())
            }
            Some(Ok(false)) => {
                tracing::info!(user = %user, "face auth did not match, declining");
                Err(fdo::Error::Failed("face auth: no match".to_string()))
            }
            Some(Err(e)) => {
                tracing::warn!(user = %user, error = %e, "face auth unavailable, declining");
                Err(fdo::Error::Failed(format!("face auth unavailable: {e}")))
            }
        }
    }

    async fn cancel_authentication(&self, cookie: &str) -> fdo::Result<()> {
        tracing::info!(cookie, "authentication cancellation requested");
        let mut auths = self.active_auths.lock().await;
        if let Some(cancel_tx) = auths.remove(cookie) {
            // If the receiver is already dropped (auth completed), send is a
            // harmless no-op.
            let _ = cancel_tx.send(());
            tracing::info!(cookie, "cancel signal sent to in-flight auth");
        } else {
            tracing::debug!(
                cookie,
                "no in-flight auth found for cookie (already completed or never started)"
            );
        }
        Ok(())
    }
}

/// Extract the unix username from the polkit identities list.
fn extract_username(identities: &[(String, HashMap<String, Value<'_>>)]) -> Option<String> {
    for (kind, details) in identities {
        if kind == "unix-user" {
            if let Some(Value::U32(uid)) = details.get("uid") {
                if let Ok(Some(user)) =
                    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(*uid))
                {
                    return Some(user.name);
                }
            }
        }
    }
    None
}

/// Get the current user's name via nix.
fn current_username() -> String {
    let uid = nix::unistd::getuid();
    nix::unistd::User::from_uid(uid)
        .ok()
        .flatten()
        .map(|u| u.name)
        .unwrap_or_else(|| "unknown".to_string())
}

/// Attempt face authentication via the facelock D-Bus daemon.
async fn try_face_auth(system_conn: &Connection, user: &str) -> anyhow::Result<bool> {
    let proxy = zbus::Proxy::new(system_conn, BUS_NAME, OBJECT_PATH, INTERFACE_NAME)
        .await
        .context("failed to build facelock D-Bus proxy")?;

    let result: facelock_core::dbus_interface::AuthResult =
        proxy.call("Authenticate", &(user,)).await?;
    Ok(result.matched)
}

/// Tell polkit that authentication succeeded for the given user/cookie.
async fn respond_to_polkit(
    system_conn: &Connection,
    cookie: &str,
    user: &str,
) -> anyhow::Result<()> {
    let proxy = zbus::Proxy::new(
        system_conn,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await
    .context("failed to build polkit authority proxy")?;

    let uid = nix::unistd::User::from_name(user)?
        .map(|u| u.uid.as_raw())
        .unwrap_or(0);

    let _: () = proxy
        .call("AuthenticationAgentResponse2", &(uid, cookie))
        .await
        .context("AuthenticationAgentResponse2 failed")?;

    Ok(())
}

/// Register this process as a polkit authentication agent.
async fn register_agent(system_conn: &Connection) -> anyhow::Result<()> {
    let authority = zbus::Proxy::new(
        system_conn,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await
    .context("failed to connect to polkit authority")?;

    let session_id = std::env::var("XDG_SESSION_ID").unwrap_or_else(|_| "auto".to_string());

    let subject_kind = "unix-session";
    let mut subject_details: HashMap<String, Value<'_>> = HashMap::new();
    subject_details.insert("session-id".to_string(), Value::from(session_id));

    let locale = std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".to_string());
    let object_path = "/org/facelock/PolkitAgent";

    let _: () = authority
        .call(
            "RegisterAuthenticationAgent",
            &((subject_kind, subject_details), &locale, object_path),
        )
        .await
        .context("failed to register as polkit authentication agent")?;

    tracing::info!("registered as polkit authentication agent");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let system_conn = Connection::system()
        .await
        .context("failed to connect to system D-Bus")?;

    let agent = PolkitAgent {
        system_conn: system_conn.clone(),
        active_auths: Arc::new(Mutex::new(HashMap::new())),
    };

    // Build a session bus connection that serves the agent interface.
    let _session_conn = Builder::session()?
        .serve_at("/org/facelock/PolkitAgent", agent)?
        .build()
        .await
        .context("failed to build session D-Bus connection")?;

    // Register with polkit on the system bus.
    register_agent(&system_conn).await?;

    tracing::info!("facelock polkit agent running, waiting for auth requests");

    // Run until SIGINT / SIGTERM.
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for ctrl-c")?;

    tracing::info!("shutting down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_username_from_unix_user_identity() {
        // UID 0 should resolve to "root" on any Linux system.
        let mut details = HashMap::new();
        details.insert("uid".to_string(), Value::U32(0));
        let identities = vec![("unix-user".to_string(), details)];
        let user = extract_username(&identities);
        assert_eq!(user, Some("root".to_string()));
    }

    #[test]
    fn extract_username_returns_none_for_empty() {
        let identities: Vec<(String, HashMap<String, Value<'_>>)> = vec![];
        assert_eq!(extract_username(&identities), None);
    }

    #[test]
    fn extract_username_ignores_non_unix_user() {
        let mut details = HashMap::new();
        details.insert("uid".to_string(), Value::U32(0));
        let identities = vec![("unix-group".to_string(), details)];
        assert_eq!(extract_username(&identities), None);
    }

    #[test]
    fn current_username_returns_nonempty() {
        let name = current_username();
        assert!(!name.is_empty());
    }
}
