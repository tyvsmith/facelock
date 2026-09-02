use anyhow::Context;
use nix::unistd::Uid;
use zbus::blocking::Proxy;

use facelock_core::dbus_interface::*;
use facelock_core::ipc::{IpcDeviceInfo, PreviewFace};
use facelock_core::types::{FaceModelInfo, MatchResult};
use facelock_daemon::auth::{AuthOutcome, CANCELLED_MESSAGE, ErrorKind};

use crate::message::{AccessMessage, explain};

/// Check if running as root; if not, offer to re-exec via sudo.
///
/// `hint` is a human-readable description like "sudo facelock setup".
/// If stdin is a TTY, prompts the user and re-execs. Otherwise bails
/// with an actionable error message.
pub fn require_root(hint: &str) -> anyhow::Result<()> {
    require_root_preserving(hint, &[])
}

/// Like [`require_root`], but the sudo re-exec keeps the named environment
/// variables (`sudo --preserve-env=A,B`).
///
/// This is not a general passthrough: the list is chosen at compile time by
/// the caller's arm in `require_root_for`, and today only the preview uses
/// it, for `WAYLAND_DISPLAY` — so a session running several compositors
/// previews on the one that launched the command. The preview validates the
/// carried value down to a bare socket name inside the invoking uid's
/// runtime directory (see `preview::wayland_socket`); nothing else reads it.
pub fn require_root_preserving(hint: &str, preserve_env: &[&str]) -> anyhow::Result<()> {
    use crate::message::{Terminal, fail};

    if Uid::current().is_root() {
        return Ok(());
    }

    let is_tty = unsafe { libc::isatty(0) } != 0;

    // Non-interactive: just bail with instructions
    if !is_tty {
        return Err(fail(AccessMessage::RootRequired {
            hint: hint.to_string(),
        }));
    }

    // Interactive: offer to re-exec with sudo. The prompt defaults to yes,
    // and the hint that says so is appended by the sink — answer tokens and
    // their advertised spelling are one English contract and never enter a
    // catalog, or a translator could localize a hint the parser will not
    // honour.
    if !Terminal.confirm_default_yes(&AccessMessage::SudoReexecPrompt)? {
        return Err(fail(AccessMessage::RootRequired {
            hint: hint.to_string(),
        }));
    }

    // Re-exec with sudo, preserving all arguments
    let args: Vec<String> = std::env::args().collect();
    let argv = sudo_argv(preserve_env, |var| std::env::var_os(var).is_some(), &args);
    let status = std::process::Command::new("sudo")
        .args(&argv)
        .status()
        .context("failed to execute sudo")?;

    std::process::exit(status.code().unwrap_or(1));
}

/// The argument vector for the sudo re-exec: a `--preserve-env=` list for
/// the requested variables that are actually set, then the original argv.
///
/// Split out so the shape is testable without exec'ing sudo. Unset
/// variables are dropped from the list rather than passed through — naming
/// them anyway would be harmless today, but the flag must never grow beyond
/// what the run demonstrably carries.
fn sudo_argv(preserve_env: &[&str], is_set: impl Fn(&str) -> bool, args: &[String]) -> Vec<String> {
    let keep: Vec<&str> = preserve_env.iter().copied().filter(|v| is_set(v)).collect();
    let mut argv = Vec::with_capacity(args.len() + 1);
    if !keep.is_empty() {
        argv.push(format!("--preserve-env={}", keep.join(",")));
    }
    argv.extend(args.iter().cloned());
    argv
}

/// Like [`require_root`], but never offers an interactive re-exec prompt.
///
/// For commands that are typically invoked non-interactively or scripted
/// (`facelock audit`, `facelock pam add|remove`, and `facelock daemon run` —
/// which every shipped service unit invokes) — a "Re-run with sudo? [Y/n]"
/// prompt would just hang a script instead of failing loudly.
pub fn require_root_scripted(hint: &str) -> anyhow::Result<()> {
    if Uid::current().is_root() {
        return Ok(());
    }

    Err(crate::message::fail(
        crate::message::AccessMessage::RootRequired {
            hint: hint.to_string(),
        },
    ))
}

/// Process-wide daemon proxy, built once and reused for every request.
static PROXY: std::sync::OnceLock<Proxy<'static>> = std::sync::OnceLock::new();

/// Get the blocking D-Bus proxy to the daemon (15-second method timeout).
///
/// The underlying connection is created once per process and reused, so a
/// multi-request session (a preview streaming frames) does not pay a bus
/// handshake per call. Failures are not cached — the next call retries the
/// connection.
fn create_proxy() -> anyhow::Result<Proxy<'static>> {
    if let Some(proxy) = PROXY.get() {
        return Ok(proxy.clone());
    }

    let connection = zbus::blocking::connection::Builder::system()
        .map_err(|e| anyhow::anyhow!("D-Bus connection failed: {e}"))?
        .method_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow::anyhow!("D-Bus connection failed: {e}"))?;

    let proxy = Proxy::new_owned(connection, BUS_NAME, OBJECT_PATH, INTERFACE_NAME)
        .map_err(|e| anyhow::anyhow!("D-Bus proxy failed: {e}"))?;

    Ok(PROXY.get_or_init(|| proxy).clone())
}

/// Extra headroom on top of the daemon's enrollment deadline: D-Bus
/// activation / daemon startup, camera warmup, and inference on the final
/// frame all happen inside the same method call.
const ENROLL_TIMEOUT_MARGIN: std::time::Duration = std::time::Duration::from_secs(15);

/// Send an Enroll request on a dedicated connection whose method timeout
/// exceeds the daemon's enrollment deadline.
///
/// Enrollment runs synchronously inside the D-Bus method call for up to
/// `Config::enroll_timeout_secs()` seconds (server side). The shared proxy's
/// 15-second timeout is at or below that deadline, so it aborted the call
/// with "I/O error: timed out" while the daemon was still enrolling
/// (issue #89). The deadline is derived from the caller's `Config` here
/// rather than passed in, so no caller can supply one that disagrees with
/// the daemon's.
pub fn send_enroll(
    user: &str,
    label: &str,
    config: &facelock_core::Config,
) -> anyhow::Result<(u32, u32)> {
    let timeout =
        std::time::Duration::from_secs(config.enroll_timeout_secs()) + ENROLL_TIMEOUT_MARGIN;
    let connection = zbus::blocking::connection::Builder::system()
        .map_err(|e| anyhow::anyhow!("D-Bus connection failed: {e}"))?
        .method_timeout(timeout)
        .build()
        .map_err(|e| anyhow::anyhow!("D-Bus connection failed: {e}"))?;
    let proxy = Proxy::new_owned(connection, BUS_NAME, OBJECT_PATH, INTERFACE_NAME)
        .map_err(|e| anyhow::anyhow!("D-Bus proxy failed: {e}"))?;

    // A client-side timeout ends this process, which drops the bus connection;
    // the daemon's caller-departure watch then cancels the enrollment and
    // nothing is stored (#308). The one exception is a commit that landed
    // before we gave up: the reply is lost but the template is real, which is
    // why the message sends the user to `facelock list` before retrying.
    let result: (u32, u32) = proxy.call("Enroll", &(user, label)).map_err(|e| {
        if is_timeout_error(&e) {
            anyhow::Error::new(e).context(explain(&AccessMessage::EnrollTimedOutClientSide))
        } else {
            anyhow::Error::new(e).context("D-Bus Enroll call failed")
        }
    })?;
    Ok(result)
}

/// True for the client-side method-timeout shape: `method_timeout` expiring
/// surfaces as an I/O `TimedOut` error.
fn is_timeout_error(err: &zbus::Error) -> bool {
    matches!(err, zbus::Error::InputOutput(io) if io.kind() == std::io::ErrorKind::TimedOut)
}

/// True if a D-Bus error name denotes AccessDenied.
fn is_access_denied_name(name: &str) -> bool {
    name == "org.freedesktop.DBus.Error.AccessDenied"
}

/// True if the error chain contains a D-Bus AccessDenied — either the system
/// bus policy rejecting the caller outright, or the daemon's own
/// authorization check.
pub fn is_access_denied(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        match cause.downcast_ref::<zbus::Error>() {
            // What a remote denial arrives as: the bus or the daemon replies
            // with an error name.
            Some(zbus::Error::MethodError(name, _, _)) => is_access_denied_name(name.as_str()),
            // Locally constructed denials (never produced by `Proxy::call`).
            Some(zbus::Error::FDO(fdo_err)) => {
                matches!(**fdo_err, zbus::fdo::Error::AccessDenied(_))
            }
            _ => false,
        }
    })
}

/// Append an actionable hint to AccessDenied errors.
///
/// Under DEC-6 (root-by-default CLI) every method the CLI calls over D-Bus is
/// root-only, and under ADR 010 the bus itself admits a non-root caller to
/// `Authenticate` alone — so whether the denial came from the daemon's own
/// `require_root` or from the bus policy, the fix is the same: run as root.
/// (Issue #108: telling a root-only rejection to "join the facelock group"
/// was actively wrong even before the group was retired.)
fn add_access_denied_hint(err: anyhow::Error) -> anyhow::Error {
    if !is_access_denied(&err) {
        return err;
    }
    // Context strings render on stderr for a human, so they localize (D10);
    // `explain` also emits the machine line the sinks emit, so the hint stays
    // visible in the debug event stream.
    err.context(explain(&AccessMessage::AccessDeniedRootHint))
}

/// The typed daemon transport (D5): one function per D-Bus method, returning
/// domain values. The handler's own request/response enums are internal to
/// `facelock-daemon`; nothing here (or in any caller) names them. Every call
/// gets the AccessDenied hint treatment via [`hinted`].
fn hinted<T>(result: anyhow::Result<T>) -> anyhow::Result<T> {
    result.map_err(add_access_denied_hint)
}

/// Call the daemon's root-only `TestAuthenticate` and decode the reply.
///
/// This is deliberately the CLI's *only* authentication transport. The
/// always-charging `Authenticate` method belongs to the real authenticators
/// — PAM, the polkit agent — and has no CLI caller: `facelock test` is a
/// diagnostic, and routing it through `Authenticate` is what once let a
/// failed face auth at a `sudo` prompt (setuid-root, UID 0 at the daemon)
/// escape the rate limiter, because the daemon had to guess from the
/// caller's privilege which of the two it was serving.
pub fn test_authenticate(user: &str) -> anyhow::Result<AuthOutcome> {
    hinted((|| {
        let proxy = create_proxy()?;
        let result: AuthResult = proxy
            .call("TestAuthenticate", &(user,))
            .context("D-Bus TestAuthenticate call failed")?;
        Ok(decode_auth_result(result))
    })())
}

/// Decode the in-band sentinels of an `AuthResult` (see docs/contracts.md
/// "Authenticate error encoding"): `model_id == -2` is a recoverable daemon
/// error travelling as data (label carries the message, byte-identical —
/// PAM string-matches "rate limited"), `-3` is the `suppress_unknown`
/// short-circuit, `-4` is a no-match in which the detector did see a face.
/// Split from the transport so the sentinel decoding is testable without a
/// bus, and so any future method replying with an `AuthResult` decodes it
/// identically.
///
/// The wire has no field for the rejection's class, so `-2` is re-classified
/// from its message here. [`ErrorKind::classify`] is the inverse of the
/// daemon's renderer and the one place that match lives on this side.
fn decode_auth_result(result: AuthResult) -> AuthOutcome {
    if !result.matched && result.model_id == -2 {
        // A cancellation shares the recoverable-error encoding but is not a
        // rejection class: it says the attempt was abandoned, not that this
        // face was refused. Recovered by its frozen string, the same way
        // `ErrorKind::classify` recovers the others.
        if result.label == CANCELLED_MESSAGE {
            return AuthOutcome::Cancelled;
        }
        return AuthOutcome::Error {
            kind: ErrorKind::classify(&result.label),
            message: result.label,
        };
    }
    if !result.matched && result.model_id == -3 {
        return AuthOutcome::Suppressed;
    }
    AuthOutcome::AuthResult(MatchResult {
        matched: result.matched,
        model_id: if result.model_id >= 0 {
            Some(result.model_id as u32)
        } else {
            None
        },
        label: if result.label.is_empty() {
            None
        } else {
            Some(result.label)
        },
        similarity: result.similarity as f32,
        face_detected: result.matched || result.model_id == -4,
        // Not part of the D-Bus AuthResult contract; derived client-side
        // where needed (see test_cmd).
        failure_reason: None,
    })
}

pub fn list_models(user: &str) -> anyhow::Result<Vec<FaceModelInfo>> {
    hinted((|| {
        let proxy = create_proxy()?;
        let models: Vec<ModelInfo> = proxy
            .call("ListModels", &(user,))
            .context("D-Bus ListModels call failed")?;
        Ok(models
            .into_iter()
            .map(|m| FaceModelInfo {
                id: m.id,
                user: m.user,
                label: m.label,
                created_at: m.created_at,
                embedder_model: m.embedder_model,
                // Empty string sentinel (D-Bus) maps back to NULL.
                device_id: Some(m.device_id).filter(|s| !s.is_empty()),
            })
            .collect())
    })())
}

pub fn remove_model(user: &str, model_id: u32) -> anyhow::Result<()> {
    hinted((|| {
        let proxy = create_proxy()?;
        proxy
            .call("RemoveModel", &(user, model_id))
            .context("D-Bus RemoveModel call failed")
    })())
}

pub fn clear_models(user: &str) -> anyhow::Result<()> {
    hinted((|| {
        let proxy = create_proxy()?;
        proxy
            .call("ClearModels", &(user,))
            .context("D-Bus ClearModels call failed")
    })())
}

pub fn preview_detect_frame(user: &str) -> anyhow::Result<(Vec<u8>, Vec<PreviewFace>)> {
    hinted((|| {
        let proxy = create_proxy()?;
        let (jpeg_data, face_infos): (Vec<u8>, Vec<PreviewFaceInfo>) = proxy
            .call("PreviewDetectFrame", &(user,))
            .context("D-Bus PreviewDetectFrame call failed")?;
        let faces = face_infos
            .into_iter()
            .map(|f| PreviewFace {
                x: f.x as f32,
                y: f.y as f32,
                width: f.width as f32,
                height: f.height as f32,
                confidence: f.confidence as f32,
                similarity: f.similarity as f32,
                recognized: f.recognized,
            })
            .collect();
        Ok((jpeg_data, faces))
    })())
}

pub fn list_devices() -> anyhow::Result<Vec<IpcDeviceInfo>> {
    hinted((|| {
        let proxy = create_proxy()?;
        let devices: Vec<DeviceInfo> = proxy
            .call("ListDevices", &())
            .context("D-Bus ListDevices call failed")?;
        Ok(devices
            .into_iter()
            .map(|d| IpcDeviceInfo {
                path: d.path,
                name: d.name,
                driver: d.driver,
                is_ir: d.is_ir,
                // The D-Bus DeviceInfo type carries no formats (C9, Phase E);
                // `BackendCaps::device_formats` states this up front.
                formats: vec![],
            })
            .collect())
    })())
}

pub fn release_camera() -> anyhow::Result<()> {
    hinted((|| {
        let proxy = create_proxy()?;
        proxy
            .call("ReleaseCamera", &())
            .context("D-Bus ReleaseCamera call failed")
    })())
}

pub fn ping() -> anyhow::Result<()> {
    hinted((|| {
        let proxy = create_proxy()?;
        let _: String = proxy.call("Ping", &()).context("D-Bus Ping call failed")?;
        Ok(())
    })())
}

/// Resolve the target user for commands.
///
/// Priority: explicit --user flag > SUDO_USER > DOAS_USER > current user.
pub fn resolve_user(flag: Option<&str>) -> String {
    resolve_user_from_env(flag, |key| std::env::var(key).ok())
}

/// The resolution order itself, with the environment supplied by the caller.
///
/// Split out so the precedence can be tested without `set_var`: mutating the
/// process environment is global to the whole test binary, which cargo runs
/// multi-threaded, so a test that does it can only be kept safe by a promise
/// that no concurrent test reads the same variable — a promise nothing
/// enforces and any later test may quietly break.
pub(crate) fn resolve_user_from_env(
    flag: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> String {
    flag.map(String::from)
        .or_else(|| env("SUDO_USER"))
        .or_else(|| env("DOAS_USER"))
        .unwrap_or_else(|| {
            env("USER").unwrap_or_else(|| {
                // Fall back to getpwuid if $USER is not set (e.g. in containers)
                let uid = unsafe { libc::getuid() };
                let pw = unsafe { libc::getpwuid(uid) };
                if pw.is_null() {
                    "unknown".into()
                } else {
                    let cstr = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) };
                    cstr.to_str().unwrap_or("unknown").to_string()
                }
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_denied_name_is_recognized() {
        assert!(is_access_denied_name(
            "org.freedesktop.DBus.Error.AccessDenied"
        ));
    }

    #[test]
    fn other_error_names_are_not_access_denied() {
        assert!(!is_access_denied_name(
            "org.freedesktop.DBus.Error.ServiceUnknown"
        ));
        assert!(!is_access_denied_name("org.facelock.Error.AccessDenied"));
        assert!(!is_access_denied_name(""));
    }

    // Covers the locally constructed FDO variant, not the wire path: a denial
    // from the bus or the daemon arrives as `zbus::Error::MethodError`, whose
    // name is checked by `is_access_denied_name` above.
    //
    // ADR 010: a bus-policy denial can only mean a non-root caller reached
    // for a root-only method (every local user may send `Authenticate`), so
    // the hint says root — never "join the group"; there is no group. The
    // daemon's own `authorize_method` -> `require_root` denial (issue #108)
    // gets the same hint: under DEC-6 it is the common case.
    #[test]
    fn access_denied_gets_root_hint() {
        for (detail, call) in [
            ("rejected by policy", "PreviewFrame"),
            (
                "ListModels requires root (caller: 'alice', UID 1000)",
                "ListModels",
            ),
        ] {
            let err = anyhow::Error::new(zbus::Error::FDO(Box::new(
                zbus::fdo::Error::AccessDenied(detail.into()),
            )))
            .context(format!("D-Bus {call} call failed"));
            let hinted = add_access_denied_hint(err);
            let msg = format!("{hinted:#}");
            assert!(msg.contains("Re-run with sudo"), "got: {msg}");
            assert!(!msg.contains("usermod"), "got: {msg}");
            assert!(!msg.contains("facelock' group"), "got: {msg}");
        }
    }

    #[test]
    fn non_access_denied_error_is_unchanged() {
        let err = anyhow::Error::new(zbus::Error::Failure("connection refused".into()))
            .context("D-Bus Ping call failed");
        let msg_before = format!("{err:#}");
        let hinted = add_access_denied_hint(err);
        assert_eq!(format!("{hinted:#}"), msg_before);
    }

    /// The default re-exec shape: no preservation flag at all. Every command
    /// except the preview goes through this arm, so their sudo environment
    /// stays exactly what it was before `require_root_preserving` existed.
    #[test]
    fn sudo_argv_without_preserve_list_is_just_the_args() {
        let args = vec!["facelock".to_string(), "enroll".to_string()];
        assert_eq!(sudo_argv(&[], |_| true, &args), args);
    }

    #[test]
    fn sudo_argv_preserves_only_set_variables() {
        let args = vec!["facelock".to_string(), "preview".to_string()];
        let argv = sudo_argv(&["WAYLAND_DISPLAY"], |v| v == "WAYLAND_DISPLAY", &args);
        assert_eq!(argv[0], "--preserve-env=WAYLAND_DISPLAY");
        assert_eq!(&argv[1..], &args[..]);

        // Unset variables drop out of the flag entirely.
        assert_eq!(sudo_argv(&["WAYLAND_DISPLAY"], |_| false, &args), args);
    }

    #[test]
    fn require_root_scripted_passes_when_root() {
        if Uid::current().is_root() {
            assert!(require_root_scripted("sudo facelock audit").is_ok());
        }
    }

    #[test]
    fn require_root_scripted_never_prompts_non_root() {
        // Can't simulate an attached TTY here, but the important property is
        // structural: unlike `require_root`, this function has no branch that
        // reads stdin or writes a prompt — it only checks the UID and bails.
        if !Uid::current().is_root() {
            let err = require_root_scripted("sudo facelock audit").unwrap_err();
            assert!(format!("{err:#}").contains("Root required"));
        }
    }

    fn reply(matched: bool, model_id: i32, label: &str) -> AuthResult {
        AuthResult {
            matched,
            model_id,
            label: label.to_string(),
            similarity: 0.9,
        }
    }

    /// The `-2` sentinel is a recoverable daemon decision carried as data,
    /// and the message must survive byte-identical: PAM substring-matches
    /// "rate limited" and "IR camera required" on the same encoding.
    #[test]
    fn recoverable_error_sentinel_decodes_with_the_message_intact() {
        match decode_auth_result(reply(false, -2, "rate limited")) {
            AuthOutcome::Error { kind, message } => {
                assert_eq!(message, "rate limited");
                // ...and the class is recovered from it, so nothing
                // downstream has to match the text again.
                assert_eq!(kind, ErrorKind::RateLimited);
            }
            other => panic!("expected a recoverable error, got {other:?}"),
        }
    }

    #[test]
    fn suppressed_sentinel_decodes_to_suppressed() {
        assert!(matches!(
            decode_auth_result(reply(false, -3, "")),
            AuthOutcome::Suppressed
        ));
    }

    #[test]
    fn no_match_decodes_without_a_model() {
        match decode_auth_result(reply(false, -1, "")) {
            AuthOutcome::AuthResult(result) => {
                assert!(!result.matched);
                assert_eq!(result.model_id, None);
                assert_eq!(result.label, None);
            }
            other => panic!("expected an ordinary non-match, got {other:?}"),
        }
    }

    #[test]
    fn match_decodes_with_its_model_and_label() {
        match decode_auth_result(reply(true, 4, "front")) {
            AuthOutcome::AuthResult(result) => {
                assert!(result.matched);
                assert_eq!(result.model_id, Some(4));
                assert_eq!(result.label.as_deref(), Some("front"));
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn resolve_user_with_flag() {
        let user = resolve_user(Some("alice"));
        assert_eq!(user, "alice");
    }

    #[test]
    fn resolve_user_no_flag_falls_through() {
        // When no flag is set, it checks env vars then current user.
        // We can't control env vars reliably in tests, but at minimum
        // it should return a non-empty string.
        let user = resolve_user(None);
        assert!(!user.is_empty());
    }
}
