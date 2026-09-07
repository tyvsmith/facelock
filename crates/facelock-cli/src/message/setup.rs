//! The `facelock setup` wizard's spine.
//!
//! Step banners, the per-step failure-and-retry-hint events, the closing
//! summary, and the non-interactive path. Individual steps own their own
//! vocabulary: see [`device`](super::device), [`download`](super::download),
//! [`system`](super::system) and [`pam`](super::pam).

#[cfg(test)]
use super::sample_text as s;
use super::{Message, fill, translate};

/// The setup wizard's spine.
///
/// Variant and field names are the machine vocabulary: [`Message::machine`]
/// derives its event line from them.
#[derive(Debug, Clone, PartialEq)]
pub enum SetupMessage {
    // -- wizard spine --
    SetupIntro {
        version: String,
    },
    SetupStepCamera,
    SetupStepModelQuality,
    SetupStepInferenceDevice,
    SetupStepModelDownload,
    SetupStepEncryption,
    SetupStepDaemon,
    SetupStepEnrollment,
    SetupStepEnrollmentSkipped,
    SetupStepTest,
    SetupStepTestSkipped,
    SetupStepPam,
    SetupCompleteHeader,

    // -- step-level failures (error + retry hint, one event) --
    CameraStepFailed {
        error: String,
        current: String,
    },
    ModelQualityStepFailed {
        error: String,
        current: String,
    },
    InferenceStepFailed {
        error: String,
        current: String,
    },
    ModelDownloadStepFailed {
        error: String,
    },
    EncryptionStepFailed {
        error: String,
    },
    EnrollStepFailed {
        error: String,
    },
    TestStepFailed {
        error: String,
    },
    SystemdStepFailed {
        error: String,
    },
    PamStepFailed {
        error: String,
    },

    // -- enrollment and test steps --
    ConfirmEnrollNow,
    EnrollSkipped,
    ConfirmTestRecognition,
    TestSkipped,

    // -- closing summary --
    SummaryCamera {
        value: String,
    },
    SummaryModels {
        dir: String,
        quality: String,
    },
    SummaryInference {
        value: String,
    },
    SummaryDatabase {
        value: String,
    },
    SummaryEncryption {
        value: String,
    },
    SummaryDaemon {
        status: String,
    },
    DaemonStatusNotConfiguredNoSystemd,
    DaemonStatusFromCommandLine,
    DaemonStatusEnabled,
    DaemonStatusNotConfigured,
    SummaryPam {
        services: String,
    },
    SummaryPamSkipped,
    SummaryPamNone,
    SummaryFaceEnrolled,
    SummaryFaceNotEnrolledNoEnroll,
    SummaryFaceNotEnrolled,

    // -- non-interactive --
    NonInteractivePreparing,
    CheckingModels {
        count: usize,
    },
    SetupCompleteShort,
    SetupCompleteEnroll,

    // -- bootstrap --
    DirectoriesCreated,
    CreatedDefaultConfig {
        path: String,
    },
    EnrollingFace,

    // -- embedding encryption (step 5, and the non-interactive auto policy) --
    EncryptionIntro,
    TpmDetected,
    TpmSealedKeyPresent {
        path: String,
    },
    GeneratingTpmSealedKey,
    TpmSealedKeyWritten {
        path: String,
    },
    EncryptionEnabledTpm,
    KeyfilePresent {
        path: String,
    },
    GeneratingKeyfile,
    KeyfileWritten {
        path: String,
    },
    EncryptionEnabledKeyfile,

    /// The plaintext-storage warning. It reaches [`super::Terminal::notice`]:
    /// stdout, so the bytes are the ones this text has always printed, and
    /// unsuppressible, so `--quiet` cannot turn "your biometric templates are
    /// stored in the clear" into silence. `error` would make it unsuppressible
    /// too, but by moving it to stderr, which is the change the byte-identity
    /// pin refuses — `notice` exists precisely for that pair of requirements.
    /// Do not "fix" this into `error`.
    EncryptionDisabledWarning,
    EncryptionAlreadyConfigured {
        method: String,
    },
    GeneratedTpmKeyAt {
        path: String,
    },
    EncryptionEnabledTpmAuto,
    GeneratedKeyfileAt {
        path: String,
    },
    EncryptionEnabledKeyfileAuto,
    /// Context for the "Delete orphaned models and continue?" confirmation,
    /// and for the refusal that replaces it when setup is not interactive. On
    /// [`super::Terminal::notice`] for that reason: a prompt whose subject
    /// `--quiet` swallowed is unanswerable, and the answer here deletes face
    /// models.
    OrphanModelsWarning {
        db_path: String,
    },
    OrphanModelsRemoved {
        count: u32,
    },

    // -- carrying an existing key across an `--encryption` method change
    // -- (issue #354): `setup_encryption_tpm_key` / `setup_encryption_keyfile`
    // -- reuse the key already on disk instead of minting a new one.
    SealingExistingKeyfile {
        key_path: String,
        sealed_path: String,
    },
    UnsealingExistingSealedKey {
        sealed_path: String,
        key_path: String,
    },
    /// The target is a keyfile, a TPM-sealed key already exists, but no
    /// usable TPM can unseal it right now: a fresh keyfile is minted instead,
    /// and the sealed key is left on disk rather than deleted.
    SealedKeyLeftInPlace {
        sealed_path: String,
    },
    /// Both a plaintext keyfile and a TPM-sealed key exist and no longer
    /// carry the same key material — most likely left behind by a `facelock
    /// setup` run from before this guard existed.
    DivergedKeysNotice {
        key_path: String,
        sealed_path: String,
    },
    /// A TPM device node exists but did not initialize; surfaced because the
    /// caller silently falls back to a software keyfile from here.
    TpmNotFunctional {
        tcti: String,
        reason: String,
    },
    /// `encryption.method` is already `"tpm"`, but no usable TPM was found on
    /// this run of the wizard: the sealed key stays in place, and this run
    /// falls through to the keyfile choice.
    TpmConfiguredButUnavailable {
        sealed_path: String,
    },

    // -- hyprlock handoff --
    HyprlockHint,
    HyprlockApplied {
        user: String,
    },

    /// A spacer between blocks, and the one variant that says nothing.
    ///
    /// It goes through the sink rather than staying a bare `println!()` so
    /// that `--quiet` silences the spacing along with the block it spaces —
    /// a quiet run that still emitted blank lines would be a stray newline
    /// on an otherwise empty stdout.
    BlankLine,
}

impl Message for SetupMessage {
    fn localized(&self) -> String {
        use SetupMessage::*;
        match self {
            SetupIntro { version } => fill(
                translate(
                    "\n  Facelock v{version}\n  Linux face authentication\n\n  This wizard will walk you through initial setup:\n    - Camera detection\n    - Model quality and inference device\n    - Model downloads\n    - Embedding encryption (TPM or software)\n    - Daemon configuration\n    - Face enrollment\n    - PAM configuration\n",
                ),
                &[("version", version.clone())],
            ),
            SetupStepCamera => translate("\n--- Step 1: Camera Selection ---\n"),
            SetupStepModelQuality => translate("\n--- Step 2: Model Quality ---\n"),
            SetupStepInferenceDevice => translate("\n--- Step 3: Inference Device ---\n"),
            SetupStepModelDownload => translate("\n--- Step 4: Model Download ---\n"),
            SetupStepEncryption => translate("\n--- Step 5: Embedding Encryption ---\n"),
            SetupStepDaemon => translate("\n--- Step 6: Daemon Configuration ---\n"),
            SetupStepEnrollment => translate("\n--- Step 7: Face Enrollment ---\n"),
            SetupStepEnrollmentSkipped => {
                translate("\n--- Step 7: Face Enrollment (skipped, --no-enroll) ---\n")
            }
            SetupStepTest => translate("\n--- Step 8: Test Recognition ---\n"),
            SetupStepTestSkipped => {
                translate("\n--- Step 8: Test Recognition (skipped, no face enrolled) ---\n")
            }
            SetupStepPam => translate("\n--- Step 9: PAM Configuration ---\n"),
            SetupCompleteHeader => translate("\n--- Setup Complete ---\n"),
            CameraStepFailed { error, current } => fill(
                translate(
                    "  Camera detection failed: {error}\n  You can configure the camera later in the config file.\n  Continuing with current setting: {current}",
                ),
                &[("error", error.clone()), ("current", current.clone())],
            ),
            ModelQualityStepFailed { error, current } => fill(
                translate(
                    "  Model quality selection failed: {error}\n  Continuing with current setting: {current}",
                ),
                &[("error", error.clone()), ("current", current.clone())],
            ),
            InferenceStepFailed { error, current } => fill(
                translate(
                    "  Inference device selection failed: {error}\n  Continuing with current setting: {current}",
                ),
                &[("error", error.clone()), ("current", current.clone())],
            ),
            ModelDownloadStepFailed { error } => fill(
                translate(
                    "  Model download failed: {error}\n  You can retry later with: sudo facelock setup --non-interactive",
                ),
                &[("error", error.clone())],
            ),
            // The msgid changed on purpose: ADR 009 renamed `facelock
            // encrypt` to `facelock tpm encrypt`, so the old string pointed
            // at a spelling this binary no longer accepts. Regenerated into
            // `po/facelock.pot` by `just pot`.
            EncryptionStepFailed { error } => fill(
                translate(
                    "  Encryption setup failed: {error}\n  You can configure encryption later with: sudo facelock tpm encrypt --generate-key",
                ),
                &[("error", error.clone())],
            ),
            EnrollStepFailed { error } => fill(
                translate(
                    "  Enrollment failed: {error}\n  You can enroll later with: facelock enroll",
                ),
                &[("error", error.clone())],
            ),
            TestStepFailed { error } => fill(
                translate("  Test failed: {error}\n  You can test later with: facelock test"),
                &[("error", error.clone())],
            ),
            SystemdStepFailed { error } => fill(
                translate(
                    "  Systemd setup failed: {error}\n  You can enable it later with: sudo facelock setup --systemd",
                ),
                &[("error", error.clone())],
            ),
            PamStepFailed { error } => fill(
                translate(
                    "  PAM setup failed: {error}\n  You can configure PAM later with: sudo facelock setup --pam",
                ),
                &[("error", error.clone())],
            ),
            ConfirmEnrollNow => translate("Would you like to enroll a face now?"),
            EnrollSkipped => translate("  Skipping face enrollment."),
            ConfirmTestRecognition => translate("Would you like to test recognition?"),
            TestSkipped => translate("  Skipping recognition test."),
            SummaryCamera { value } => fill(
                translate("  Camera:     {value}"),
                &[("value", value.clone())],
            ),
            SummaryModels { dir, quality } => fill(
                translate("  Models:     {dir} ({quality})"),
                &[("dir", dir.clone()), ("quality", quality.clone())],
            ),
            SummaryInference { value } => fill(
                translate("  Inference:  {value}"),
                &[("value", value.clone())],
            ),
            SummaryDatabase { value } => fill(
                translate("  Database:   {value}"),
                &[("value", value.clone())],
            ),
            SummaryEncryption { value } => fill(
                translate("  Encryption: {value}"),
                &[("value", value.clone())],
            ),
            SummaryDaemon { status } => fill(
                translate("  Daemon:   {status}"),
                &[("status", status.clone())],
            ),
            DaemonStatusNotConfiguredNoSystemd => translate("not configured (--no-systemd)"),
            DaemonStatusFromCommandLine => translate("configured from the command line"),
            DaemonStatusEnabled => translate("enabled (D-Bus activation)"),
            DaemonStatusNotConfigured => translate("not configured"),
            SummaryPam { services } => fill(
                translate("  PAM:      {services}"),
                &[("services", services.clone())],
            ),
            SummaryPamSkipped => translate("  PAM:      not configured (--no-pam)"),
            SummaryPamNone => translate("  PAM:      not configured"),
            SummaryFaceEnrolled => translate("  Face:     enrolled"),
            SummaryFaceNotEnrolledNoEnroll => {
                translate("  Face:     not enrolled (--no-enroll; run `facelock enroll`)")
            }
            SummaryFaceNotEnrolled => translate("  Face:     not enrolled (run `facelock enroll`)"),
            NonInteractivePreparing => translate("facelock setup: preparing system...\n"),
            CheckingModels { count } => fill(
                translate("Checking {count} model(s)...\n"),
                &[("count", count.to_string())],
            ),
            SetupCompleteShort => translate("\nSetup complete."),
            SetupCompleteEnroll => {
                translate("\nSetup complete. Run `facelock enroll` to register your face.")
            }
            DirectoriesCreated => translate("  Directories created."),
            CreatedDefaultConfig { path } => fill(
                translate("  Created default config at {path}"),
                &[("path", path.clone())],
            ),
            EnrollingFace => translate("\nEnrolling face..."),
            EncryptionIntro => {
                translate("  Setting up AES-256-GCM encryption for face embeddings.")
            }
            TpmDetected => translate("  TPM 2.0 detected and functional."),
            TpmSealedKeyPresent { path } => fill(
                translate("  TPM-sealed key already exists at {path}."),
                &[("path", path.clone())],
            ),
            GeneratingTpmSealedKey => translate("  Generating and sealing AES key with TPM..."),
            TpmSealedKeyWritten { path } => fill(
                translate("  TPM-sealed key written to {path} (permissions: 0600)."),
                &[("path", path.clone())],
            ),
            EncryptionEnabledTpm => translate("  Encryption enabled (TPM-sealed key)."),
            KeyfilePresent { path } => fill(
                translate("  Encryption key already exists at {path}."),
                &[("path", path.clone())],
            ),
            GeneratingKeyfile => translate("  Generating encryption key..."),
            KeyfileWritten { path } => fill(
                translate("  Key written to {path} (permissions: 0600)."),
                &[("path", path.clone())],
            ),
            EncryptionEnabledKeyfile => translate("  Encryption enabled."),
            EncryptionDisabledWarning => translate(
                "  ⚠ WARNING: encryption disabled (--encryption=none).\n    Biometric templates will be stored UNENCRYPTED in the database.\n    `facelock enroll` refuses to write plaintext embeddings unless\n    security.allow_plaintext is also set in the config.",
            ),
            EncryptionAlreadyConfigured { method } => fill(
                translate("  Encryption already configured ({method})."),
                &[("method", method.clone())],
            ),
            GeneratedTpmKeyAt { path } => fill(
                translate("  [ok] Generated TPM-sealed encryption key at {path}"),
                &[("path", path.clone())],
            ),
            EncryptionEnabledTpmAuto => {
                translate("  [ok] AES-256-GCM encryption enabled (TPM-sealed key).")
            }
            GeneratedKeyfileAt { path } => fill(
                translate("  [ok] Generated encryption key at {path}"),
                &[("path", path.clone())],
            ),
            EncryptionEnabledKeyfileAuto => translate("  [ok] AES-256-GCM encryption enabled."),
            OrphanModelsWarning { db_path } => fill(
                translate(
                    "\n  WARNING: encrypted face models already exist in {db_path} but the\n  encryption key is missing. Generating a new key would make them unreadable.\n",
                ),
                &[("db_path", db_path.clone())],
            ),
            OrphanModelsRemoved { count } => fill(
                translate("  Removed {count} orphaned model(s)."),
                &[("count", count.to_string())],
            ),
            SealingExistingKeyfile {
                key_path,
                sealed_path,
            } => fill(
                translate(
                    "  Sealing the existing key at {key_path} with the TPM (writing {sealed_path})...",
                ),
                &[
                    ("key_path", key_path.clone()),
                    ("sealed_path", sealed_path.clone()),
                ],
            ),
            UnsealingExistingSealedKey {
                sealed_path,
                key_path,
            } => fill(
                translate(
                    "  Unsealing the existing TPM-sealed key at {sealed_path} into {key_path}...",
                ),
                &[
                    ("sealed_path", sealed_path.clone()),
                    ("key_path", key_path.clone()),
                ],
            ),
            SealedKeyLeftInPlace { sealed_path } => fill(
                translate(
                    "  NOTE: leaving the TPM-sealed key at {sealed_path} in place; models sealed under it stay readable while a TPM can still unseal it.",
                ),
                &[("sealed_path", sealed_path.clone())],
            ),
            DivergedKeysNotice {
                key_path,
                sealed_path,
            } => fill(
                translate(
                    "  NOTE: {key_path} and {sealed_path} no longer hold the same key; models sealed under either stay readable only while that file is kept.",
                ),
                &[
                    ("key_path", key_path.clone()),
                    ("sealed_path", sealed_path.clone()),
                ],
            ),
            TpmNotFunctional { tcti, reason } => fill(
                translate(
                    "  NOTE: a TPM device is present but not usable (tcti: {tcti}): {reason}. Falling back to a software keyfile.",
                ),
                &[("tcti", tcti.clone()), ("reason", reason.clone())],
            ),
            TpmConfiguredButUnavailable { sealed_path } => fill(
                translate(
                    "  NOTE: encryption.method is \"tpm\" but no usable TPM was found right now; the sealed key at {sealed_path} stays in place. Continuing with a software keyfile for this run.",
                ),
                &[("sealed_path", sealed_path.clone())],
            ),
            HyprlockHint => translate(
                "\n==> To finish hyprlock integration, run as your normal user:\n==>     facelock hyprlock enable",
            ),
            HyprlockApplied { user } => fill(
                translate("  hyprlock integration applied for {user}."),
                &[("user", user.clone())],
            ),
            // Never `translate("")`: gettext answers an empty msgid with the
            // catalog's own metadata header, so an "empty" translation would
            // print the .mo file's Content-Type block.
            BlankLine => String::new(),
        }
    }
}

/// One sample per variant, in enum order, for the sweeps in [`super::Samples`].
///
/// The list is flat, so it cannot cycle and cannot name a variant twice
/// without saying so; `VARIANT_COUNT` is what fails the sweep when a new
/// variant is not sampled at all. The compiler's share of this is `localized`
/// above: no wildcard arm, so a variant that renders nothing does not build.
#[cfg(test)]
impl super::Samples for SetupMessage {
    const VARIANT_COUNT: usize = 76;

    fn samples() -> Vec<Self> {
        use SetupMessage::*;
        vec![
            SetupIntro { version: s("1.0") },
            SetupStepCamera,
            SetupStepModelQuality,
            SetupStepInferenceDevice,
            SetupStepModelDownload,
            SetupStepEncryption,
            SetupStepDaemon,
            SetupStepEnrollment,
            SetupStepEnrollmentSkipped,
            SetupStepTest,
            SetupStepTestSkipped,
            SetupStepPam,
            SetupCompleteHeader,
            CameraStepFailed {
                error: s("e"),
                current: s("c"),
            },
            ModelQualityStepFailed {
                error: s("e"),
                current: s("c"),
            },
            InferenceStepFailed {
                error: s("e"),
                current: s("c"),
            },
            ModelDownloadStepFailed { error: s("e") },
            EncryptionStepFailed { error: s("e") },
            EnrollStepFailed { error: s("e") },
            TestStepFailed { error: s("e") },
            SystemdStepFailed { error: s("e") },
            PamStepFailed { error: s("e") },
            ConfirmEnrollNow,
            EnrollSkipped,
            ConfirmTestRecognition,
            TestSkipped,
            SummaryCamera { value: s("v") },
            SummaryModels {
                dir: s("/d"),
                quality: s("q"),
            },
            SummaryInference { value: s("v") },
            SummaryDatabase { value: s("v") },
            SummaryEncryption { value: s("v") },
            SummaryDaemon { status: s("st") },
            DaemonStatusNotConfiguredNoSystemd,
            DaemonStatusFromCommandLine,
            DaemonStatusEnabled,
            DaemonStatusNotConfigured,
            SummaryPam {
                services: s("sudo"),
            },
            SummaryPamSkipped,
            SummaryPamNone,
            SummaryFaceEnrolled,
            SummaryFaceNotEnrolledNoEnroll,
            SummaryFaceNotEnrolled,
            NonInteractivePreparing,
            CheckingModels { count: 2 },
            SetupCompleteShort,
            SetupCompleteEnroll,
            DirectoriesCreated,
            CreatedDefaultConfig { path: s("/c") },
            EnrollingFace,
            EncryptionIntro,
            TpmDetected,
            TpmSealedKeyPresent { path: s("/k") },
            GeneratingTpmSealedKey,
            TpmSealedKeyWritten { path: s("/k") },
            EncryptionEnabledTpm,
            KeyfilePresent { path: s("/k") },
            GeneratingKeyfile,
            KeyfileWritten { path: s("/k") },
            EncryptionEnabledKeyfile,
            EncryptionDisabledWarning,
            EncryptionAlreadyConfigured { method: s("Tpm") },
            GeneratedTpmKeyAt { path: s("/k") },
            EncryptionEnabledTpmAuto,
            GeneratedKeyfileAt { path: s("/k") },
            EncryptionEnabledKeyfileAuto,
            OrphanModelsWarning { db_path: s("/db") },
            OrphanModelsRemoved { count: 2 },
            SealingExistingKeyfile {
                key_path: s("/k"),
                sealed_path: s("/s"),
            },
            UnsealingExistingSealedKey {
                sealed_path: s("/s"),
                key_path: s("/k"),
            },
            SealedKeyLeftInPlace {
                sealed_path: s("/s"),
            },
            DivergedKeysNotice {
                key_path: s("/k"),
                sealed_path: s("/s"),
            },
            TpmNotFunctional {
                tcti: s("device:/dev/tpmrm0"),
                reason: s("e"),
            },
            TpmConfiguredButUnavailable {
                sealed_path: s("/s"),
            },
            HyprlockHint,
            HyprlockApplied { user: s("u") },
            BlankLine,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wizard's step banners, in the order `run_wizard` prints them.
    ///
    /// The numbering is the whole point of the list: the daemon is configured
    /// at step 6, ahead of enrollment and the recognition test, so that both
    /// run against the transport every later authentication uses instead of
    /// against the direct-camera fallback. A banner whose number no longer
    /// matches its position is the symptom of a step that moved without the
    /// steps around it being renumbered.
    #[test]
    fn step_banners_are_numbered_in_the_order_the_wizard_runs_them() {
        use SetupMessage::*;

        let expected = [
            (SetupStepCamera, "\n--- Step 1: Camera Selection ---\n"),
            (SetupStepModelQuality, "\n--- Step 2: Model Quality ---\n"),
            (
                SetupStepInferenceDevice,
                "\n--- Step 3: Inference Device ---\n",
            ),
            (SetupStepModelDownload, "\n--- Step 4: Model Download ---\n"),
            (
                SetupStepEncryption,
                "\n--- Step 5: Embedding Encryption ---\n",
            ),
            (SetupStepDaemon, "\n--- Step 6: Daemon Configuration ---\n"),
            (SetupStepEnrollment, "\n--- Step 7: Face Enrollment ---\n"),
            (SetupStepTest, "\n--- Step 8: Test Recognition ---\n"),
            (SetupStepPam, "\n--- Step 9: PAM Configuration ---\n"),
        ];
        for (step, banner) in &expected {
            assert_eq!(&step.localized(), banner);
        }

        // The skipped twins carry their unskipped twin's number, so they
        // renumber with it rather than drifting into a second numbering.
        assert_eq!(
            SetupStepEnrollmentSkipped.localized(),
            "\n--- Step 7: Face Enrollment (skipped, --no-enroll) ---\n"
        );
        assert_eq!(
            SetupStepTestSkipped.localized(),
            "\n--- Step 8: Test Recognition (skipped, no face enrolled) ---\n"
        );
    }

    /// The intro promises the same order the steps run in.
    #[test]
    fn the_intro_lists_the_daemon_before_enrollment() {
        let intro = SetupMessage::SetupIntro {
            version: "0.0.0".into(),
        }
        .localized();
        let daemon = intro.find("Daemon configuration").expect("daemon bullet");
        let enrollment = intro.find("Face enrollment").expect("enrollment bullet");
        let pam = intro.find("PAM configuration").expect("pam bullet");
        assert!(daemon < enrollment, "daemon is configured before enrolling");
        assert!(enrollment < pam);
    }

    /// Every string this domain took over from a `println!` in
    /// `commands/setup.rs`, pinned to the bytes that call site printed.
    ///
    /// The pins are the contract, not a snapshot: a failing assertion means
    /// the *string* changed, and the fix is to restore the string. Editing
    /// the expectation to match new output inverts the test — a wording
    /// change is a separate decision with its own review, because container
    /// suites and downstream integrations grep this output.
    #[test]
    fn setup_fallback_is_byte_identical() {
        use SetupMessage::*;

        // -- bootstrap --
        assert_eq!(DirectoriesCreated.localized(), "  Directories created.");
        assert_eq!(
            CreatedDefaultConfig {
                path: "/etc/facelock/config.toml".into()
            }
            .localized(),
            "  Created default config at /etc/facelock/config.toml"
        );
        assert_eq!(EnrollingFace.localized(), "\nEnrolling face...");

        // -- encryption, interactive --
        assert_eq!(
            EncryptionIntro.localized(),
            "  Setting up AES-256-GCM encryption for face embeddings."
        );
        assert_eq!(
            TpmDetected.localized(),
            "  TPM 2.0 detected and functional."
        );
        assert_eq!(
            TpmSealedKeyPresent {
                path: "/etc/facelock/sealed.key".into()
            }
            .localized(),
            "  TPM-sealed key already exists at /etc/facelock/sealed.key."
        );
        assert_eq!(
            GeneratingTpmSealedKey.localized(),
            "  Generating and sealing AES key with TPM..."
        );
        assert_eq!(
            TpmSealedKeyWritten {
                path: "/etc/facelock/sealed.key".into()
            }
            .localized(),
            "  TPM-sealed key written to /etc/facelock/sealed.key (permissions: 0600)."
        );
        assert_eq!(
            EncryptionEnabledTpm.localized(),
            "  Encryption enabled (TPM-sealed key)."
        );
        assert_eq!(
            KeyfilePresent {
                path: "/etc/facelock/facelock.key".into()
            }
            .localized(),
            "  Encryption key already exists at /etc/facelock/facelock.key."
        );
        assert_eq!(
            GeneratingKeyfile.localized(),
            "  Generating encryption key..."
        );
        assert_eq!(
            KeyfileWritten {
                path: "/etc/facelock/facelock.key".into()
            }
            .localized(),
            "  Key written to /etc/facelock/facelock.key (permissions: 0600)."
        );
        assert_eq!(
            EncryptionEnabledKeyfile.localized(),
            "  Encryption enabled."
        );
        assert_eq!(
            EncryptionDisabledWarning.localized(),
            "  \u{26a0} WARNING: encryption disabled (--encryption=none).\n    Biometric templates will be stored UNENCRYPTED in the database.\n    `facelock enroll` refuses to write plaintext embeddings unless\n    security.allow_plaintext is also set in the config."
        );

        // -- encryption, the non-interactive auto policy --
        assert_eq!(
            EncryptionAlreadyConfigured {
                method: "Tpm".into()
            }
            .localized(),
            "  Encryption already configured (Tpm)."
        );
        assert_eq!(
            GeneratedTpmKeyAt {
                path: "/etc/facelock/sealed.key".into()
            }
            .localized(),
            "  [ok] Generated TPM-sealed encryption key at /etc/facelock/sealed.key"
        );
        assert_eq!(
            EncryptionEnabledTpmAuto.localized(),
            "  [ok] AES-256-GCM encryption enabled (TPM-sealed key)."
        );
        assert_eq!(
            GeneratedKeyfileAt {
                path: "/etc/facelock/facelock.key".into()
            }
            .localized(),
            "  [ok] Generated encryption key at /etc/facelock/facelock.key"
        );
        assert_eq!(
            EncryptionEnabledKeyfileAuto.localized(),
            "  [ok] AES-256-GCM encryption enabled."
        );

        // -- the orphaned-template guard --
        assert_eq!(
            OrphanModelsWarning {
                db_path: "/var/lib/facelock/facelock.db".into()
            }
            .localized(),
            "\n  WARNING: encrypted face models already exist in /var/lib/facelock/facelock.db but the\n  encryption key is missing. Generating a new key would make them unreadable.\n"
        );
        assert_eq!(
            OrphanModelsRemoved { count: 3 }.localized(),
            "  Removed 3 orphaned model(s)."
        );

        // -- carrying an existing key across a method change (#354) --
        assert_eq!(
            SealingExistingKeyfile {
                key_path: "/etc/facelock/facelock.key".into(),
                sealed_path: "/etc/facelock/sealed.key".into(),
            }
            .localized(),
            "  Sealing the existing key at /etc/facelock/facelock.key with the TPM (writing /etc/facelock/sealed.key)..."
        );
        assert_eq!(
            UnsealingExistingSealedKey {
                sealed_path: "/etc/facelock/sealed.key".into(),
                key_path: "/etc/facelock/facelock.key".into(),
            }
            .localized(),
            "  Unsealing the existing TPM-sealed key at /etc/facelock/sealed.key into /etc/facelock/facelock.key..."
        );
        assert_eq!(
            SealedKeyLeftInPlace {
                sealed_path: "/etc/facelock/sealed.key".into()
            }
            .localized(),
            "  NOTE: leaving the TPM-sealed key at /etc/facelock/sealed.key in place; models sealed under it stay readable while a TPM can still unseal it."
        );
        assert_eq!(
            DivergedKeysNotice {
                key_path: "/etc/facelock/facelock.key".into(),
                sealed_path: "/etc/facelock/sealed.key".into(),
            }
            .localized(),
            "  NOTE: /etc/facelock/facelock.key and /etc/facelock/sealed.key no longer hold the same key; models sealed under either stay readable only while that file is kept."
        );
        assert_eq!(
            TpmNotFunctional {
                tcti: "device:/dev/tpmrm0".into(),
                reason: "no such device".into(),
            }
            .localized(),
            "  NOTE: a TPM device is present but not usable (tcti: device:/dev/tpmrm0): no such device. Falling back to a software keyfile."
        );
        assert_eq!(
            TpmConfiguredButUnavailable {
                sealed_path: "/etc/facelock/sealed.key".into()
            }
            .localized(),
            "  NOTE: encryption.method is \"tpm\" but no usable TPM was found right now; the sealed key at /etc/facelock/sealed.key stays in place. Continuing with a software keyfile for this run."
        );

        // -- hyprlock handoff --
        assert_eq!(
            HyprlockHint.localized(),
            "\n==> To finish hyprlock integration, run as your normal user:\n==>     facelock hyprlock enable"
        );
        assert_eq!(
            HyprlockApplied {
                user: "alice".into()
            }
            .localized(),
            "  hyprlock integration applied for alice."
        );
    }

    /// The spacer renders as nothing, so the sink's trailing newline is the
    /// whole line — the bytes `println!()` produced.
    ///
    /// It must never reach gettext: `dgettext` answers an empty msgid with
    /// the catalog's metadata header, so a translated build would print the
    /// `.mo` file's `Content-Type` block where a blank line belongs. Under a
    /// real catalog this test would fail if the arm ever grew a
    /// `translate("")`, because the C locale used here returns the msgid.
    #[test]
    fn blank_line_is_empty_and_never_translated() {
        assert_eq!(SetupMessage::BlankLine.localized(), "");
    }
}
