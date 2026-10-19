use std::time::Instant;

use facelock_core::Config;

use facelock_core::notify::{NotifyEvent, notify_desktop_if_enabled};

use crate::backend::{Backend, BackendKind};
use crate::ipc_client;
use crate::message::{FaceMessage, Terminal, fail};
use crate::notifications::DesktopNotifier;

pub fn run(config: &Config, user: Option<String>) -> anyhow::Result<()> {
    // N11 (issue #96): `facelock test` is root-only regardless of transport
    // (direct or daemon-mediated) — keeps similarity scores and full detail,
    // and is what makes it safe for both paths to exempt failed test runs
    // from rate-limit consumption (root already has unrestricted access to
    // the rate-limit table). On the daemon transport that exemption is the
    // root-only `TestAuthenticate` method, asked for explicitly, rather than
    // something the daemon infers from this process being root. Root is
    // established by `main`'s `require_root_for` gate, before any prompt,
    // output, or the config parse itself (C6).

    // Check models exist — offer to run setup if missing
    if !crate::resolved::ModelFiles::probe(config).all_present() {
        Terminal.info(&FaceMessage::ModelsNotFoundOfferSetup);
        if Terminal.confirm(&FaceMessage::ConfirmDownloadModels)? {
            crate::commands::setup::run(false)?;
            // Deliberate re-probe: setup just changed the disk. The judgment
            // still uses the Config this process parsed, as it always has —
            // setup may have rewritten the file, and picking that up would be
            // a mid-command re-read.
            if !crate::resolved::ModelFiles::probe(config).all_present() {
                return Err(fail(FaceMessage::ModelsStillMissingAfterSetup));
            }
        } else {
            return Err(fail(FaceMessage::ModelsRequired));
        }
    }

    let user = ipc_client::resolve_user(user.as_deref());
    let notif_config = &config.notification;
    let notifier = DesktopNotifier::for_current_session();

    // One selection for the whole test (D1) — after the setup offer above,
    // which may have just installed and started the daemon.
    let backend = Backend::select(config);

    // Check if user has enrolled models before attempting auth. Three-way
    // discrimination (C7, issue #105), owned by the backend seam: a store
    // that opens with zero models and a store that doesn't exist yet both
    // mean "not enrolled" — but a store that is present and cannot be read
    // is an error, and must never be reported as "no models enrolled".
    let has_models = enrolled_check(&backend, config, &user)?;
    if !has_models {
        Terminal.info(&FaceMessage::NoModelsEnrolled { user: user.clone() });
        Terminal.info(&FaceMessage::RunEnrollFirst);
        return Ok(());
    }

    // Warn if no enrolled models match the current embedder. Same failure
    // policy as above (C4): the store was readable one query ago, so a
    // failure here is propagated rather than guessed either way (the two
    // branches used to guess in opposite directions).
    {
        let config_embedder = &config.recognition.embedder_model;
        if !backend.has_models_for_embedder(&user, config_embedder)? {
            Terminal.info(&FaceMessage::NoMatchingEmbedder {
                embedder: config_embedder.clone(),
            });
            Terminal.info(&FaceMessage::ReenrollHint);
            return Ok(());
        }
    }

    Terminal.info(&FaceMessage::TestingUser { user: user.clone() });
    Terminal.info(&FaceMessage::TestLookAtCamera);

    // The daemon opens the camera out of reach of this process, so a
    // configured LED is checked from sysfs instead: a wrong path or an
    // unwritable node is named here rather than only in the daemon log
    // (#413). The direct transport names the failure from the open itself.
    if backend.kind() == BackendKind::Daemon
        && let Some(warning) = ir_led_warning(config)
    {
        Terminal.error(&warning);
    }

    notify_desktop_if_enabled(notif_config, &notifier, &NotifyEvent::Scanning);

    let start = Instant::now();
    let result = match backend.recognize(&user) {
        Ok(result) => result,
        Err(e) => {
            // One failure policy for both transports: the error's own text is
            // the notification body, and the error propagates.
            notify_desktop_if_enabled(
                notif_config,
                &notifier,
                &NotifyEvent::Failure {
                    reason: e.to_string(),
                },
            );
            return Err(e);
        }
    };
    let elapsed = start.elapsed();

    if result.matched {
        // Presentation, not transport: the two renderings predate the seam
        // and are kept byte-stable. The direct line has always omitted the
        // model id/label.
        match backend.kind() {
            BackendKind::Daemon => {
                let model_id = result.model_id.unwrap_or(0);
                let label = result.label.as_deref().unwrap_or("unknown");
                Terminal.info(&FaceMessage::TestMatchedModel {
                    model_id,
                    label: label.to_string(),
                    similarity: result.similarity,
                    seconds: elapsed.as_secs_f64(),
                });
            }
            _ => {
                Terminal.info(&FaceMessage::TestMatched {
                    similarity: result.similarity,
                    seconds: elapsed.as_secs_f64(),
                });
            }
        }
        notify_desktop_if_enabled(
            notif_config,
            &notifier,
            &NotifyEvent::Success {
                label: result.label.clone(),
                similarity: result.similarity,
            },
        );
        return Ok(());
    }

    // Not matched: name the liveness gate when it was the blocker. The direct
    // result carries the reason; the D-Bus AuthResult contract does not, so
    // the daemon side infers it — matched=false with similarity above the
    // recognition threshold means frame variance blocked the attempt.
    let variance_blocked = match backend.kind() {
        BackendKind::Daemon => {
            config.security.require_frame_variance
                && result.similarity >= config.recognition.threshold
        }
        _ => {
            result.failure_reason
                == Some(facelock_core::types::AuthFailureReason::VarianceNotSatisfied)
        }
    };

    if variance_blocked {
        Terminal.info(&FaceMessage::TestVarianceBlocked {
            similarity: result.similarity,
            seconds: elapsed.as_secs_f64(),
        });
        notify_desktop_if_enabled(
            notif_config,
            &notifier,
            &NotifyEvent::Failure {
                reason: "face matched but liveness variance not satisfied".to_string(),
            },
        );
    } else {
        Terminal.info(&FaceMessage::TestNoMatch {
            similarity: result.similarity,
            seconds: elapsed.as_secs_f64(),
        });
        notify_desktop_if_enabled(
            notif_config,
            &notifier,
            &NotifyEvent::Failure {
                reason: format!("no match (best similarity: {:.2})", result.similarity),
            },
        );
    }

    Ok(())
}

/// The warning for a configured `device.ir_led` that cannot be lit, read
/// from sysfs without writing to it; `None` when unset or usable.
fn ir_led_warning(config: &Config) -> Option<FaceMessage> {
    let led = config.device.ir_led.as_ref()?;
    facelock_camera::ir_led::probe(led)
        .err()
        .map(|error| FaceMessage::IrLedNotLit {
            led: led.clone(),
            error,
        })
}

/// The C7 enrolled check, with the direct transport's store failures reworded
/// for the human running `test`: the per-user enrollment marker is consulted
/// **for the message only** — it is readable in exactly the cases the
/// database is not, and lets the error say what the user actually wants to
/// know ("you appear to be enrolled; the database is the problem"). Daemon
/// errors pass through untouched: they are not store reads, and an
/// AccessDenied already carries its own actionable hint.
fn enrolled_check(backend: &Backend, config: &Config, user: &str) -> anyhow::Result<bool> {
    backend.has_models(user).map_err(|e| {
        if backend.kind().is_direct() {
            unreadable_store_error(config, user, &e)
        } else {
            e
        }
    })
}

/// Build the "store present but unreadable" error (C7). With a readable
/// marker the message leads with the enrollment count; without one it still
/// names the real problem — it must never claim "no models enrolled".
fn unreadable_store_error(config: &Config, user: &str, cause: &anyhow::Error) -> anyhow::Error {
    use super::enrollment_marker::{MarkerState, marker_dir, read_marker_in};
    match read_marker_in(&marker_dir(config), user) {
        MarkerState::Enrolled(marker) => anyhow::anyhow!(
            "You appear to have {} enrolled model(s), but the face database at {} \
             can't be read right now ({cause:#}).",
            marker.models,
            config.storage.db_path
        ),
        _ => anyhow::anyhow!(
            "the face database at {} can't be read ({cause:#})",
            config.storage.db_path
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn no_ir_led_warning_when_none_is_configured() {
        assert!(ir_led_warning(&Config::default()).is_none());
    }

    #[test]
    fn a_configured_led_that_does_not_exist_is_named() {
        let mut config = Config::default();
        let led = "/sys/class/leds/facelock-test-absent-413".to_string();
        config.device.ir_led = Some(led.clone());
        match ir_led_warning(&config) {
            Some(FaceMessage::IrLedNotLit { led: named, error }) => {
                assert_eq!(named, led);
                assert!(error.contains("facelock-test-absent-413"), "{error}");
            }
            other => panic!("expected IrLedNotLit, got {other:?}"),
        }
    }

    /// `mode = "oneshot"` pins a DirectByConfig selection without touching
    /// D-Bus.
    fn config_with_db(db_path: &Path) -> Config {
        let mut config = Config::parse("[daemon]\nmode = \"oneshot\"\n").expect("config parses");
        config.storage.db_path = db_path.to_string_lossy().into_owned();
        config
    }

    /// C7 branch 1+2: a fresh (absent) store and an open store with zero
    /// models both report "not enrolled".
    #[test]
    fn absent_store_and_zero_models_read_as_not_enrolled() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        let config = config_with_db(&db_path);
        let backend = Backend::select(&config);

        // Absent (fresh): the StoreError::Absent variant, read as "not
        // enrolled" without creating anything.
        assert!(!enrolled_check(&backend, &config, "alice").unwrap());

        // Open store, models for someone else only.
        {
            let store = facelock_store::FaceStore::create(&db_path).unwrap();
            store
                .add_model("bob", "front", &[0.5f32; 512], "embedder")
                .unwrap();
        }
        assert!(!enrolled_check(&backend, &config, "alice").unwrap());
        assert!(enrolled_check(&backend, &config, "bob").unwrap());
    }

    /// The `Absent` arm answers "not enrolled" as a *value*: the probe must
    /// not manufacture the empty database the create-based `open_store` used
    /// to leave behind.
    #[test]
    fn absent_store_probe_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        let config = config_with_db(&db_path);
        let backend = Backend::select(&config);

        assert!(!enrolled_check(&backend, &config, "alice").unwrap());
        assert!(
            !db_path.exists(),
            "probing enrollment must not create the database it reports absent"
        );
    }

    /// C7 branch 3: a present-but-unreadable store is an error and must not
    /// claim "no models". Without a marker the message still names the real
    /// problem.
    #[test]
    fn unreadable_store_is_an_error_not_no_models() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        std::fs::write(&db_path, b"this is not a sqlite database").unwrap();
        let config = config_with_db(&db_path);
        let backend = Backend::select(&config);

        let err = enrolled_check(&backend, &config, "alice").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("can't be read"),
            "error must name the real problem: {msg}"
        );
        assert!(
            !msg.contains("appear to have"),
            "no marker, so no enrollment claim: {msg}"
        );
    }

    /// C7 branch 3 with a readable marker: the error leads with the (hedged)
    /// enrollment count.
    #[test]
    fn unreadable_store_error_reports_marker_count() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        std::fs::write(&db_path, b"this is not a sqlite database").unwrap();

        let config = config_with_db(&db_path);
        let backend = Backend::select(&config);
        let marker_base = super::super::enrollment_marker::marker_dir(&config);
        super::super::enrollment_marker::write_marker_in(&marker_base, "alice", 3, None).unwrap();

        let err = enrolled_check(&backend, &config, "alice").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("appear to have 3 enrolled model(s)"),
            "marker must enrich the message: {msg}"
        );
        assert!(msg.contains("can't be read"), "{msg}");
    }

    /// `--user other` degradation: no marker for that user, so the error
    /// falls back to the plain accurate message — still an error, never
    /// "no models enrolled".
    #[test]
    fn unreadable_store_for_other_user_degrades_to_plain_error() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        std::fs::write(&db_path, b"this is not a sqlite database").unwrap();

        let config = config_with_db(&db_path);
        let backend = Backend::select(&config);
        let marker_base = super::super::enrollment_marker::marker_dir(&config);
        super::super::enrollment_marker::write_marker_in(&marker_base, "alice", 3, None).unwrap();

        let err = enrolled_check(&backend, &config, "someone-else").unwrap_err();
        let msg = format!("{err:#}");
        assert!(!msg.contains("appear to have"), "{msg}");
        assert!(msg.contains("can't be read"), "{msg}");
    }
}
