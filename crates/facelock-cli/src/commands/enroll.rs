use chrono::Local;

use facelock_core::Config;

use crate::backend::Backend;
use crate::ipc_client;
use crate::message::{FaceMessage, Terminal, fail};

pub fn run(
    config: &Config,
    user: Option<String>,
    label: Option<String>,
    skip_setup_check: bool,
) -> anyhow::Result<()> {
    // Setup gate: prompt user if setup hasn't been run.
    // Setup includes model downloads, encryption, and face enrollment,
    // so if setup runs successfully we're done — no need to enroll again.
    if !skip_setup_check {
        let marker = std::path::Path::new(super::setup::SETUP_COMPLETE_MARKER);
        if !marker.exists() {
            ipc_client::require_root("sudo facelock setup")?;
            Terminal.info(&FaceMessage::SetupNotCompleted);
            if Terminal.confirm(&FaceMessage::ConfirmRunSetupNow)? {
                super::setup::run(false)?;
                if !marker.exists() {
                    return Err(fail(FaceMessage::SetupDidNotComplete));
                }
                // Setup includes face enrollment (Step 7), so we're done
                return Ok(());
            } else {
                Terminal.info(&FaceMessage::RunSetupWhenReady);
                return Ok(());
            }
        }
    }

    ipc_client::require_root("sudo facelock enroll")?;

    // Encryption posture (Plan 04): refuse plaintext enrollment unless opted in;
    // warn prominently when the opt-in is active.
    if config.encryption.method == facelock_core::config::EncryptionMethod::None {
        if config.security.allow_plaintext {
            Terminal.error(&FaceMessage::PlaintextEnrollWarning);
        } else if let Err(message) = config.ensure_enroll_encryption_allowed() {
            anyhow::bail!(message);
        }
    }

    // Models must exist before anything opens a camera. One probe through the
    // shared fact (D7) instead of a per-command re-derivation.
    if !crate::resolved::ModelFiles::probe(config).all_present() {
        return Err(fail(FaceMessage::ModelsMissing {
            dir: config.daemon.model_dir.clone(),
        }));
    }

    let user = ipc_client::resolve_user(user.as_deref());

    // One selection for the whole enrollment (D1). The probe is
    // `name_has_owner`, which never triggers D-Bus activation — the old
    // per-site convention this replaces existed because an unconditional
    // D-Bus call here would *activate* the daemon and silently flip the
    // subsequent enrollment from direct to daemon mode (issue #89 validation
    // fallout). Now the transport cannot change between the label scan and
    // the enrollment.
    let backend = Backend::select(config);

    let label = match label {
        Some(label) => label,
        None => {
            let date = Local::now().format("%Y-%m-%d").to_string();
            next_label(&date, &user, &backend)?
        }
    };

    // Warn if existing models use a different embedder than currently
    // configured. One failure policy (C4, issue #105), owned by the seam: a
    // store or daemon failure propagates instead of silently skipping the
    // warning — the enrollment ahead needs the very store this check just
    // failed to read. A provably absent store reads as "no models, nothing
    // stale" without being created.
    {
        let config_embedder = &config.recognition.embedder_model;
        let has_stale = backend.has_models(&user)?
            && !backend.has_models_for_embedder(&user, config_embedder)?;
        if has_stale {
            Terminal.info(&FaceMessage::StaleEmbedderNote {
                embedder: config_embedder.clone(),
            });
        }
    }

    Terminal.info(&FaceMessage::Enrolling {
        user: user.clone(),
        label: label.clone(),
    });
    Terminal.info(&FaceMessage::EnrollLookAtCamera);

    let (model_id, embedding_count) = backend.enroll(&user, &label)?;

    Terminal.info(&FaceMessage::EnrollComplete {
        model_id,
        count: embedding_count,
        label: label.clone(),
    });
    super::enrollment_marker::refresh(&backend, config, &user);
    check_model_count(&user, &backend);

    Ok(())
}

/// Generate the next available label like "2026-03-15-1", "2026-03-15-2", etc.
///
/// One failure policy (C4, issue #105): a store failure while picking the
/// label propagates via the seam; it must not silently fall back to a "-1"
/// suffix.
fn next_label(date_prefix: &str, user: &str, backend: &Backend) -> anyhow::Result<String> {
    let max_suffix = backend
        .list_models(user)?
        .iter()
        .filter_map(|m| {
            m.label
                .strip_prefix(date_prefix)
                .and_then(|rest| rest.strip_prefix('-'))
                .and_then(|n| n.parse::<u32>().ok())
        })
        .max()
        .unwrap_or(0);

    Ok(format!("{date_prefix}-{}", max_suffix + 1))
}

fn check_model_count(user: &str, backend: &Backend) {
    // Post-success advisory only: the enrollment already committed, so a
    // failed count here must not turn a successful enrollment into a
    // reported failure.
    if let Ok(models) = backend.list_models(user)
        && models.len() > 5
    {
        Terminal.info(&FaceMessage::TooManyModels {
            user: user.to_string(),
            count: models.len(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `mode = "oneshot"` pins a DirectByConfig selection without touching
    /// D-Bus.
    fn oneshot_config_with_db(db_path: &Path) -> Config {
        let mut config = Config::parse("[daemon]\nmode = \"oneshot\"\n").expect("config parses");
        config.storage.db_path = db_path.to_string_lossy().into_owned();
        config
    }

    /// `Absent` vs everything else at the pre-enrollment reads: a fresh
    /// install reads as "no models, nothing stale" and the probes create
    /// nothing — enrollment proper is what brings the database into being.
    #[test]
    fn absent_store_reads_as_no_models_without_creating() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        let config = oneshot_config_with_db(&db_path);
        let backend = Backend::select(&config);

        let has_stale = backend.has_models("alice").unwrap()
            && !backend
                .has_models_for_embedder("alice", "embedder")
                .unwrap();
        assert!(!has_stale);
        assert_eq!(
            next_label("2026-08-13", "alice", &backend).unwrap(),
            "2026-08-13-1"
        );
        assert!(
            !db_path.exists(),
            "pre-enrollment reads must not create the database"
        );
    }

    /// The counterpart: an unreadable (present) store is NOT `Absent` — the
    /// stale-embedder check propagates it instead of skipping the warning.
    #[test]
    fn stale_embedder_check_propagates_unreadable_store() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        std::fs::write(&db_path, b"this is not a sqlite database").unwrap();

        let config = oneshot_config_with_db(&db_path);
        let backend = Backend::select(&config);
        assert!(backend.has_models("alice").is_err());
    }

    /// C4: a store failure while picking the next label propagates — it must
    /// not silently fall back to a "-1" suffix.
    #[test]
    fn next_label_propagates_store_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        std::fs::write(&db_path, b"this is not a sqlite database").unwrap();

        let config = oneshot_config_with_db(&db_path);
        let backend = Backend::select(&config);
        assert!(next_label("2026-08-13", "alice", &backend).is_err());
    }

    #[test]
    fn next_label_counts_existing_labels() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("facelock.db");
        {
            let store = facelock_store::FaceStore::create(&db_path).unwrap();
            let emb = [0.5f32; 512];
            store
                .add_model("alice", "2026-08-13-1", &emb, "embedder")
                .unwrap();
            store
                .add_model("alice", "2026-08-13-2", &emb, "embedder")
                .unwrap();
        }

        let config = oneshot_config_with_db(&db_path);
        let backend = Backend::select(&config);
        assert_eq!(
            next_label("2026-08-13", "alice", &backend).unwrap(),
            "2026-08-13-3"
        );
        // A fresh prefix (or user) starts at -1.
        assert_eq!(
            next_label("2026-08-14", "alice", &backend).unwrap(),
            "2026-08-14-1"
        );
        assert_eq!(
            next_label("2026-08-13", "bob", &backend).unwrap(),
            "2026-08-13-1"
        );
    }
}
