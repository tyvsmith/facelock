//! Editing `/etc/pam.d/*`.
//!
//! The most safety-sensitive text the CLI prints: it previews a change to the
//! files that decide whether the machine can be logged into, so these events
//! carry the exact line, the backup path and the rollback command.

#[cfg(test)]
use super::sample_text as s;
use super::{Message, fill, translate};

/// PAM service-file configuration.
///
/// Variant and field names are the machine vocabulary: [`Message::machine`]
/// derives its event line from them.
#[derive(Debug, Clone, PartialEq)]
pub enum PamMessage {
    PamSkippedFlag {
        dir: String,
    },
    PamModuleMissing {
        path: String,
    },
    ConfiguringPamFor {
        service: String,
    },
    NoPamCandidates {
        dir: String,
    },
    PamLinePreview {
        line: String,
    },
    PromptSelectPamServices,
    PamConfigureFailed {
        service: String,
        error: String,
    },
    NoPamServicesSelected,
    PamFileNotFound {
        path: String,
    },
    PamLineAlreadyPresent {
        path: String,
    },
    PamInsertBeforeAuthHint,
    PamInsertAtTopHint,
    PamModifyPreview {
        path: String,
        line: String,
        hint: String,
        backup: String,
    },
    ConfirmProceed,
    PamSkippedFile {
        path: String,
    },
    PamBackedUp {
        path: String,
        backup: String,
    },
    PamInstalled {
        path: String,
        backup: String,
        service: String,
    },
    PamRemoved {
        path: String,
    },
    PamNoLineFound {
        path: String,
    },
    PamServiceAbsent {
        path: String,
    },
    PamBackupExists {
        path: String,
        backup: String,
    },
}

impl Message for PamMessage {
    fn localized(&self) -> String {
        use PamMessage::*;
        match self {
            PamSkippedFlag { dir } => fill(
                translate(
                    "  Skipping PAM configuration (--no-pam).\n  No file under {dir} is read or modified.",
                ),
                &[("dir", dir.clone())],
            ),
            PamModuleMissing { path } => fill(
                translate(
                    "  PAM module not found at {path}.\n  Install it first, then run: sudo facelock setup --pam",
                ),
                &[("path", path.clone())],
            ),
            ConfiguringPamFor { service } => fill(
                translate("  Configuring PAM for {service}..."),
                &[("service", service.clone())],
            ),
            NoPamCandidates { dir } => fill(
                translate("  No supported PAM service files found in {dir}/."),
                &[("dir", dir.clone())],
            ),
            PamLinePreview { line } => fill(
                translate(
                    "  The following line will be added to each selected /etc/pam.d/<service>:\n\n      {line}\n\n  It is inserted above the first existing 'auth' line. A backup\n  (.facelock-backup) is saved before any change, and you'll be asked\n  to confirm each file individually.\n",
                ),
                &[("line", line.clone())],
            ),
            PromptSelectPamServices => {
                translate("Select services to enable face authentication for")
            }
            PamConfigureFailed { service, error } => fill(
                translate("  Failed to configure {service}: {error}"),
                &[("service", service.clone()), ("error", error.clone())],
            ),
            NoPamServicesSelected => translate("  No PAM services selected."),
            PamFileNotFound { path } => fill(
                translate("PAM service file not found: {path}"),
                &[("path", path.clone())],
            ),
            PamLineAlreadyPresent { path } => fill(
                translate("PAM line already present in {path}. Nothing to do."),
                &[("path", path.clone())],
            ),
            PamInsertBeforeAuthHint => translate("inserted before the first 'auth' line"),
            PamInsertAtTopHint => {
                translate("no 'auth' line found — inserted at the top of the file")
            }
            PamModifyPreview {
                path,
                line,
                hint,
                backup,
            } => fill(
                translate(
                    "\nAbout to modify {path}:\n  + {line}    ({hint})\n  Backup will be saved to: {backup}\n\n(To configure manually instead, add the line above to each service yourself.)",
                ),
                &[
                    ("path", path.clone()),
                    ("line", line.clone()),
                    ("hint", hint.clone()),
                    ("backup", backup.clone()),
                ],
            ),
            ConfirmProceed => translate("Proceed?"),
            PamSkippedFile { path } => {
                fill(translate("Skipped {path}."), &[("path", path.clone())])
            }
            PamBackedUp { path, backup } => fill(
                translate("Backed up {path} -> {backup}"),
                &[("path", path.clone()), ("backup", backup.clone())],
            ),
            PamInstalled {
                path,
                backup,
                service,
            } => fill(
                translate(
                    "Installed facelock PAM line into {path}\n\nTo rollback:\n  sudo cp {backup} {path}\n  # or: sudo facelock setup --pam --remove --service {service}",
                ),
                &[
                    ("path", path.clone()),
                    ("backup", backup.clone()),
                    ("service", service.clone()),
                ],
            ),
            PamRemoved { path } => fill(
                translate("Removed facelock PAM line from {path}"),
                &[("path", path.clone())],
            ),
            PamNoLineFound { path } => fill(
                translate("No facelock PAM line found in {path}. Nothing to remove."),
                &[("path", path.clone())],
            ),
            PamServiceAbsent { path } => fill(
                translate("PAM service file absent: {path}. Nothing to remove."),
                &[("path", path.clone())],
            ),
            PamBackupExists { path, backup } => fill(
                translate("Backup exists at {backup}\nTo restore: sudo cp {backup} {path}"),
                &[("path", path.clone()), ("backup", backup.clone())],
            ),
        }
    }
}

/// One sample per variant, in enum order, for the placeholder sweep.
///
/// [`Self::next_sample`] is an exhaustive `match` with no wildcard arm, so a
/// new variant stops this compiling until it is given a sample and linked
/// into the walk — the sweep cannot silently fall behind the vocabulary.
#[cfg(test)]
impl super::Samples for PamMessage {
    fn first_sample() -> Self {
        use PamMessage::*;
        PamSkippedFlag { dir: s("/d") }
    }

    fn next_sample(&self) -> Option<Self> {
        use PamMessage::*;
        Some(match self {
            PamSkippedFlag { .. } => PamModuleMissing { path: s("/p") },
            PamModuleMissing { .. } => ConfiguringPamFor { service: s("sudo") },
            ConfiguringPamFor { .. } => NoPamCandidates { dir: s("/d") },
            NoPamCandidates { .. } => PamLinePreview {
                line: s("auth ..."),
            },
            PamLinePreview { .. } => PromptSelectPamServices,
            PromptSelectPamServices => PamConfigureFailed {
                service: s("sudo"),
                error: s("e"),
            },
            PamConfigureFailed { .. } => NoPamServicesSelected,
            NoPamServicesSelected => PamFileNotFound { path: s("/p") },
            PamFileNotFound { .. } => PamLineAlreadyPresent { path: s("/p") },
            PamLineAlreadyPresent { .. } => PamInsertBeforeAuthHint,
            PamInsertBeforeAuthHint => PamInsertAtTopHint,
            PamInsertAtTopHint => PamModifyPreview {
                path: s("/p"),
                line: s("auth"),
                hint: s("h"),
                backup: s("/b"),
            },
            PamModifyPreview { .. } => ConfirmProceed,
            ConfirmProceed => PamSkippedFile { path: s("/p") },
            PamSkippedFile { .. } => PamBackedUp {
                path: s("/p"),
                backup: s("/b"),
            },
            PamBackedUp { .. } => PamInstalled {
                path: s("/p"),
                backup: s("/b"),
                service: s("sudo"),
            },
            PamInstalled { .. } => PamRemoved { path: s("/p") },
            PamRemoved { .. } => PamNoLineFound { path: s("/p") },
            PamNoLineFound { .. } => PamServiceAbsent { path: s("/p") },
            PamServiceAbsent { .. } => PamBackupExists {
                path: s("/p"),
                backup: s("/b"),
            },
            PamBackupExists { .. } => return None,
        })
    }
}
