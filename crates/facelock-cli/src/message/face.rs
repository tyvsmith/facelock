//! The enrolled-face lifecycle: `enroll`, `test`, `remove`, `clear`.
//!
//! What the commands that create, exercise and delete face models tell the
//! user, including the "setup has not run yet" preamble they share.

#[cfg(test)]
use super::sample_text as s;
use super::{Message, fill, translate};

/// The enrolled-face lifecycle.
///
/// Variant and field names are the machine vocabulary: [`Message::machine`]
/// derives its event line from them.
#[derive(Debug, Clone, PartialEq)]
pub enum FaceMessage {
    // -- enroll --
    SetupNotCompleted,
    ConfirmRunSetupNow,
    SetupDidNotComplete,
    RunSetupWhenReady,
    PlaintextEnrollWarning,
    ModelsMissing {
        dir: String,
    },
    StaleEmbedderNote {
        embedder: String,
    },
    Enrolling {
        user: String,
        label: String,
    },
    EnrollLookAtCamera,
    EnrollComplete {
        model_id: u32,
        count: u32,
        label: String,
    },
    TooManyModels {
        user: String,
        count: usize,
    },

    // -- test --
    ModelsNotFoundOfferSetup,
    ConfirmDownloadModels,
    ModelsStillMissingAfterSetup,
    ModelsRequired,
    NoModelsEnrolled {
        user: String,
    },
    RunEnrollFirst,
    NoMatchingEmbedder {
        embedder: String,
    },
    ReenrollHint,
    TestingUser {
        user: String,
    },
    TestLookAtCamera,
    IrLedNotLit {
        led: String,
        error: String,
    },
    TestMatched {
        similarity: f32,
        seconds: f64,
    },
    TestMatchedModel {
        model_id: u32,
        label: String,
        similarity: f32,
        seconds: f64,
    },
    TestVarianceBlocked {
        similarity: f32,
        seconds: f64,
    },
    TestNoMatch {
        similarity: f32,
        seconds: f64,
    },

    // -- remove / clear --
    ConfirmRemoveModel {
        model_id: u32,
        user: String,
    },
    Cancelled,
    RemovedModel {
        model_id: u32,
        user: String,
    },
    ModelNotFound {
        model_id: u32,
        user: String,
    },
    ConfirmClearAll {
        user: String,
    },
    ClearedModels {
        count: usize,
        user: String,
    },
    AllModelsRemoved {
        user: String,
    },
}

impl Message for FaceMessage {
    fn localized(&self) -> String {
        use FaceMessage::*;
        match self {
            SetupNotCompleted => translate("Setup has not been completed."),
            ConfirmRunSetupNow => translate("Run setup now?"),
            SetupDidNotComplete => translate("Setup did not complete successfully."),
            RunSetupWhenReady => translate("Run 'sudo facelock setup' when ready."),
            PlaintextEnrollWarning => translate(
                "WARNING: encryption.method = \"none\" and security.allow_plaintext = true.\nYour face template will be stored UNENCRYPTED (plaintext biometric data at rest).",
            ),
            ModelsMissing { dir } => fill(
                translate(
                    "Face recognition models not found in {dir}.\nRun `sudo facelock setup` to download them.",
                ),
                &[("dir", dir.clone())],
            ),
            StaleEmbedderNote { embedder } => fill(
                translate(
                    "Note: existing models don't use the configured embedder '{embedder}'.\nOld enrollments will not work with the new embedder. Consider removing them with 'facelock remove'.\n",
                ),
                &[("embedder", embedder.clone())],
            ),
            Enrolling { user, label } => fill(
                translate("Enrolling face for user '{user}' with label '{label}'..."),
                &[("user", user.clone()), ("label", label.clone())],
            ),
            EnrollLookAtCamera => {
                translate("Look at the camera. Slowly turn your head left and right.")
            }
            EnrollComplete {
                model_id,
                count,
                label,
            } => fill(
                translate(
                    "\nFace enrolled successfully!\n  Model ID: {model_id}\n  Embeddings: {count}\n  Label: {label}",
                ),
                &[
                    ("model_id", model_id.to_string()),
                    ("count", count.to_string()),
                    ("label", label.clone()),
                ],
            ),
            TooManyModels { user, count } => fill(
                translate(
                    "\nWarning: user '{user}' has {count} face models. Consider removing old ones with 'facelock remove'.",
                ),
                &[("user", user.clone()), ("count", count.to_string())],
            ),
            ModelsNotFoundOfferSetup => translate("Face recognition models not found."),
            ConfirmDownloadModels => translate("Download models now?"),
            ModelsStillMissingAfterSetup => translate("Models still not found after setup."),
            ModelsRequired => translate("Models required. Run `facelock setup` to download them."),
            NoModelsEnrolled { user } => fill(
                translate("No face models enrolled for user '{user}'."),
                &[("user", user.clone())],
            ),
            RunEnrollFirst => translate("Run 'facelock enroll' to enroll a face first."),
            NoMatchingEmbedder { embedder } => fill(
                translate("Warning: no enrolled models use the configured embedder '{embedder}'."),
                &[("embedder", embedder.clone())],
            ),
            ReenrollHint => translate("Re-enroll with 'facelock enroll' to use the current model."),
            TestingUser { user } => fill(
                translate("Testing face recognition for user '{user}'..."),
                &[("user", user.clone())],
            ),
            TestLookAtCamera => translate("Look at the camera."),
            IrLedNotLit { led, error } => fill(
                translate("Warning: IR LED {led} cannot be lit: {error}"),
                &[("led", led.clone()), ("error", error.clone())],
            ),
            TestMatched {
                similarity,
                seconds,
            } => fill(
                translate("Matched (similarity: {similarity}) in {seconds}s"),
                &[
                    ("similarity", format!("{similarity:.2}")),
                    ("seconds", format!("{seconds:.2}")),
                ],
            ),
            TestMatchedModel {
                model_id,
                label,
                similarity,
                seconds,
            } => fill(
                translate(
                    "Matched model #{model_id} '{label}' (similarity: {similarity}) in {seconds}s",
                ),
                &[
                    ("model_id", model_id.to_string()),
                    ("label", label.clone()),
                    ("similarity", format!("{similarity:.2}")),
                    ("seconds", format!("{seconds:.2}")),
                ],
            ),
            TestVarianceBlocked {
                similarity,
                seconds,
            } => fill(
                translate(
                    "Face matched (best: {similarity}) but the liveness variance check was not satisfied after {seconds}s — try moving slightly, or tune security.frame_variance_max_similarity",
                ),
                &[
                    ("similarity", format!("{similarity:.2}")),
                    ("seconds", format!("{seconds:.1}")),
                ],
            ),
            TestNoMatch {
                similarity,
                seconds,
            } => fill(
                translate("No match (best: {similarity}) after {seconds}s"),
                &[
                    ("similarity", format!("{similarity:.2}")),
                    ("seconds", format!("{seconds:.1}")),
                ],
            ),
            ConfirmRemoveModel { model_id, user } => fill(
                translate("Remove face model #{model_id} for user '{user}'?"),
                &[("model_id", model_id.to_string()), ("user", user.clone())],
            ),
            Cancelled => translate("Cancelled."),
            RemovedModel { model_id, user } => fill(
                translate("Removed face model #{model_id} for user '{user}'."),
                &[("model_id", model_id.to_string()), ("user", user.clone())],
            ),
            ModelNotFound { model_id, user } => fill(
                translate("Model #{model_id} not found for user '{user}'."),
                &[("model_id", model_id.to_string()), ("user", user.clone())],
            ),
            ConfirmClearAll { user } => fill(
                translate("Remove ALL face models for user '{user}'?"),
                &[("user", user.clone())],
            ),
            ClearedModels { count, user } => fill(
                translate("Removed {count} face model(s) for user '{user}'."),
                &[("count", count.to_string()), ("user", user.clone())],
            ),
            AllModelsRemoved { user } => fill(
                translate("All face models removed for user '{user}'."),
                &[("user", user.clone())],
            ),
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
impl super::Samples for FaceMessage {
    const VARIANT_COUNT: usize = 33;

    fn samples() -> Vec<Self> {
        use FaceMessage::*;
        vec![
            SetupNotCompleted,
            ConfirmRunSetupNow,
            SetupDidNotComplete,
            RunSetupWhenReady,
            PlaintextEnrollWarning,
            ModelsMissing { dir: s("/m") },
            StaleEmbedderNote { embedder: s("e") },
            Enrolling {
                user: s("u"),
                label: s("l"),
            },
            EnrollLookAtCamera,
            EnrollComplete {
                model_id: 1,
                count: 2,
                label: s("l"),
            },
            TooManyModels {
                user: s("u"),
                count: 6,
            },
            ModelsNotFoundOfferSetup,
            ConfirmDownloadModels,
            ModelsStillMissingAfterSetup,
            ModelsRequired,
            NoModelsEnrolled { user: s("u") },
            RunEnrollFirst,
            NoMatchingEmbedder { embedder: s("e") },
            ReenrollHint,
            TestingUser { user: s("u") },
            TestLookAtCamera,
            IrLedNotLit {
                led: s("l"),
                error: s("e"),
            },
            TestMatched {
                similarity: 0.5,
                seconds: 1.0,
            },
            TestMatchedModel {
                model_id: 1,
                label: s("l"),
                similarity: 0.5,
                seconds: 1.0,
            },
            TestVarianceBlocked {
                similarity: 0.5,
                seconds: 1.0,
            },
            TestNoMatch {
                similarity: 0.5,
                seconds: 1.0,
            },
            ConfirmRemoveModel {
                model_id: 1,
                user: s("u"),
            },
            Cancelled,
            RemovedModel {
                model_id: 1,
                user: s("u"),
            },
            ModelNotFound {
                model_id: 1,
                user: s("u"),
            },
            ConfirmClearAll { user: s("u") },
            ClearedModels {
                count: 1,
                user: s("u"),
            },
            AllModelsRemoved { user: s("u") },
        ]
    }
}
