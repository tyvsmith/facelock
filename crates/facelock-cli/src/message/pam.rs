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
    /// The wizard's softer version of [`PamMessage::PamModuleNotInstalled`]:
    /// step 9 skips PAM rather than failing. Names every candidate for the
    /// same reason.
    PamModuleMissing {
        paths: String,
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
    PamBackupCleanupFailed {
        service: String,
        error: String,
    },
    NoPamServicesSelected,
    /// Every path the resolver tried, comma-separated. Not one path: a
    /// service can be absent from `/etc/pam.d` and present in a vendor
    /// directory, so naming only the first place looked would send the
    /// operator to create a file that already exists somewhere else.
    PamFileNotFound {
        paths: String,
    },
    PamLineAlreadyPresent {
        path: String,
    },
    PamInsertBeforeAuthHint,
    PamInsertAfterHeaderHint,
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
    /// The rollback instructions — the one message that tells an operator who
    /// has just changed an auth stack how to get back in. On
    /// [`super::Terminal::notice`]: stdout, so a normal run prints what it
    /// always printed, and unsuppressible, so `--quiet` cannot take it.
    /// `--json` drops it, because the document's `backup` field is the same
    /// fact in the form that caller reads.
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

    // -- vendor directories (P1) -------------------------------------------
    /// The preview of a vendor copy. Names no backup, because there is none:
    /// the file being created did not exist, and the vendor original is left
    /// alone, so deleting the new file is the undo.
    PamOverridePreview {
        path: String,
        vendor: String,
        line: String,
        hint: String,
    },
    /// A local override was created from a vendor file. On
    /// [`super::Terminal::notice`], like [`PamMessage::PamInstalled`] and for
    /// the same reason: the operator has changed an auth stack and needs to
    /// know how to undo it — and here also that the new file shadows a
    /// package's and will not follow its updates.
    PamVendorOverridden {
        path: String,
        vendor: String,
        service: String,
    },
    /// Removal retired an unchanged Facelock-created local vendor copy.
    PamVendorOverrideRemoved {
        path: String,
        vendor: String,
    },
    /// The module line was removed, but drift means the local copy remains.
    PamVendorOverrideRetained {
        path: String,
        vendor: String,
    },
    /// The generated header names a configured vendor candidate, but no
    /// current source exists. Removal may take out the module line, but the
    /// local override remains because its origin cannot be revalidated.
    PamVendorOverrideSourceAbsent {
        path: String,
        vendor: String,
    },
    /// The restart shape already has no module line; the absent configured
    /// vendor source is the explicit reason the local override remains.
    PamVendorOverrideSourceAbsentNoLine {
        path: String,
        vendor: String,
    },
    /// `--dry-run`: an unchanged local copy would be deleted after removal.
    PamPlanDeleteOverride {
        path: String,
        vendor: String,
    },
    /// A service whose only copy is package-owned, on a verb that does not
    /// write there.
    PamVendorOnly {
        path: String,
    },
    /// `status`: the service exists only as a vendor file and carries no
    /// facelock line.
    PamStatusVendorOnly {
        path: String,
    },
    /// `--dry-run`: the override that would be created.
    PamPlanOverride {
        path: String,
        vendor: String,
        hint: String,
    },
    /// The override a vendor-only service needs cannot be created. A phase-one
    /// refusal, so nothing is written for the whole run.
    PamOverrideDirUnwritable {
        dir: String,
        path: String,
        error: String,
    },

    // -- `facelock pam` (#174) ---------------------------------------------
    /// The closing copy-pasteable hint. One variant carrying the whole block,
    /// because it is one paragraph a translator has to be able to reflow, not
    /// four independent sentences.
    PamExtensionHint {
        line: String,
    },
    PamInvalidServiceName {
        service: String,
    },
    /// A symlinked service file. Names the link text for diagnosis, but the
    /// writer never follows it, even when it appears to remain in-directory.
    PamServiceSymlinkedOutside {
        path: String,
        target: String,
        dir: String,
    },
    /// A service file reachable by more than one name. `links` is the link
    /// count, pre-formatted: it is a number in a sentence, not a quantity to
    /// compute with, and the seam's `fill` takes strings.
    PamServiceHardLinked {
        path: String,
        links: String,
    },
    /// `remedy` is the flag that unlocks this surface: `--allow-sensitive` on
    /// `pam add`, `--yes` on the `setup --pam` alias, which keeps its
    /// combined meaning. Carrying it as data is what lets one message be
    /// truthful on both.
    PamSensitiveRefused {
        service: String,
        remedy: String,
    },
    /// `paths` is every candidate the probe tried, comma-separated, and
    /// `path` the primary one the install hint names. Both: an operator on an
    /// unlisted layout needs to see what was looked for, and still needs one
    /// concrete place to put it.
    PamModuleNotInstalled {
        paths: String,
        path: String,
    },
    PamServiceAbsentSkipped {
        path: String,
    },
    PamPlanAdd {
        path: String,
        hint: String,
    },
    PamPlanRemove {
        path: String,
    },
    PamPlanNoChange {
        path: String,
    },
    PamPlanAbsent {
        path: String,
    },
    PamStatusPresent {
        path: String,
    },
    PamStatusMissing {
        path: String,
    },
    /// Every path the resolver tried, comma-separated — the same answer
    /// [`PamMessage::PamFileNotFound`] gives on `add` and `remove`, so the two
    /// verbs do not answer "where did you look?" differently. The row's
    /// machine `path` field is still the single first-directory path.
    PamStatusAbsent {
        paths: String,
    },
    PamStatusUnknown {
        path: String,
        error: String,
    },

    // -- `pam status --all` (P3) -------------------------------------------
    /// `status`: the service carries the line, and the file it carries it in
    /// is a local copy hiding a package's own. Configured, with the
    /// maintenance consequence named — the override will not follow the
    /// vendor file's updates.
    PamStatusOverride {
        path: String,
        vendor: String,
    },
    /// A directory on the search path that could not be listed. **Not** the
    /// same as one that held nothing: this says the answer is incomplete, and
    /// `--all` exits 2 rather than claiming a service count it could not
    /// count.
    PamStatusDirUnreadable {
        dir: String,
        error: String,
    },
    /// `--all` found nothing. Names the directories, so "none" is an answer
    /// about somewhere rather than a bare word.
    PamStatusNoServices {
        dirs: String,
    },
    /// `--all` found nothing **and** could not read everywhere. The same
    /// sentence as [`PamMessage::PamStatusNoServices`] would be a claim about
    /// a directory that was never opened, which is the confusion the whole
    /// flag exists to remove — so the emptiness is scoped to what was read and
    /// the rest is named in the same breath.
    PamStatusNoServicesIncomplete {
        dirs: String,
        unchecked: String,
    },
    /// `--all` could not read **any** directory on the search path. There is
    /// no set of directories an emptiness could be scoped to, so this asserts
    /// only what is true; it exists so stdout carries a line in every branch
    /// rather than falling silent exactly when the machine is worst off.
    PamStatusNothingReadable {
        dirs: String,
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
            PamModuleMissing { paths } => fill(
                translate(
                    "  PAM module not found. Tried: {paths}\n  Install it first, then run: sudo facelock setup --pam",
                ),
                &[("paths", paths.clone())],
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
                    "  The following line will be added to each selected /etc/pam.d/<service>:\n\n      {line}\n\n  It is inserted above the first existing 'auth' line. A root-only backup\n  is saved under /var/lib/facelock/pam-backups before any change, and you'll\n  be asked to confirm each file individually.\n",
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
            PamBackupCleanupFailed { service, error } => fill(
                translate(
                    "  PAM service {service} reached the requested state, but rollback-state cleanup failed: {error}",
                ),
                &[("service", service.clone()), ("error", error.clone())],
            ),
            NoPamServicesSelected => translate("  No PAM services selected."),
            PamFileNotFound { paths } => fill(
                translate("PAM service file not found: {paths}"),
                &[("paths", paths.clone())],
            ),
            PamLineAlreadyPresent { path } => fill(
                translate("PAM line already present in {path}. Nothing to do."),
                &[("path", path.clone())],
            ),
            PamInsertBeforeAuthHint => translate("inserted before the first 'auth' line"),
            PamInsertAfterHeaderHint => {
                translate("no 'auth' line found — inserted after the PAM header")
            }
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
                    "Installed facelock PAM line into {path}\n\nTo rollback:\n  sudo cp {backup} {path}\n  # or: sudo facelock pam remove --service {service}",
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
            PamOverridePreview {
                path,
                vendor,
                line,
                hint,
            } => fill(
                translate(
                    "\nAbout to create {path} from the vendor file {vendor}:\n  + {line}    ({hint})\n  {vendor} is package-owned and is not modified.\n\n(To configure manually instead, copy that file and add the line above yourself.)",
                ),
                &[
                    ("path", path.clone()),
                    ("vendor", vendor.clone()),
                    ("line", line.clone()),
                    ("hint", hint.clone()),
                ],
            ),
            PamVendorOverridden {
                path,
                vendor,
                service,
            } => fill(
                translate(
                    "Created {path} from {vendor} with the facelock PAM line.\nThis local override shadows the vendor file and will not track vendor updates.\n\nTo rollback:\n  sudo facelock pam remove --service {service}\nAn unchanged Facelock-created override is deleted; a modified override is kept after its facelock line is removed.",
                ),
                &[
                    ("path", path.clone()),
                    ("vendor", vendor.clone()),
                    ("service", service.clone()),
                ],
            ),
            PamVendorOverrideRemoved { path, vendor } => fill(
                translate(
                    "Removed facelock PAM line from {path}.\nDeleted unchanged local override; {vendor} is authoritative again.",
                ),
                &[("path", path.clone()), ("vendor", vendor.clone())],
            ),
            PamVendorOverrideRetained { path, vendor } => fill(
                translate(
                    "Removed facelock PAM line from {path}.\nKept local override because administrator or vendor drift no longer matches {vendor}.",
                ),
                &[("path", path.clone()), ("vendor", vendor.clone())],
            ),
            PamVendorOverrideSourceAbsent { path, vendor } => fill(
                translate(
                    "Removed facelock PAM line from {path}.\nKept local override because the vendor source is absent: {vendor}.",
                ),
                &[("path", path.clone()), ("vendor", vendor.clone())],
            ),
            PamVendorOverrideSourceAbsentNoLine { path, vendor } => fill(
                translate(
                    "No facelock PAM line found in {path}.\nKept local override because the vendor source is absent: {vendor}.",
                ),
                &[("path", path.clone()), ("vendor", vendor.clone())],
            ),
            PamPlanDeleteOverride { path, vendor } => fill(
                translate(
                    "Would remove the facelock PAM line from {path}, delete the unchanged local override, and make {vendor} authoritative again.",
                ),
                &[("path", path.clone()), ("vendor", vendor.clone())],
            ),
            PamVendorOnly { path } => fill(
                translate(
                    "PAM service file exists only at {path}, which is package-owned and never modified by facelock. Nothing to remove.",
                ),
                &[("path", path.clone())],
            ),
            PamStatusVendorOnly { path } => fill(
                translate("{path}: vendor file only, no facelock PAM line"),
                &[("path", path.clone())],
            ),
            PamPlanOverride { path, vendor, hint } => fill(
                translate("Would create {path} from {vendor} with the facelock PAM line ({hint})."),
                &[
                    ("path", path.clone()),
                    ("vendor", vendor.clone()),
                    ("hint", hint.clone()),
                ],
            ),
            PamOverrideDirUnwritable { dir, path, error } => fill(
                translate(
                    "Cannot create the local override {path}: {dir} is not writable ({error}).\nThe vendor copy of this service is package-owned, so facelock will not edit it in place.",
                ),
                &[
                    ("dir", dir.clone()),
                    ("path", path.clone()),
                    ("error", error.clone()),
                ],
            ),
            PamExtensionHint { line } => fill(
                translate(
                    "\n==> facelock PAM line for manual extension to other services:\n==>   {line}\n==> Add the above line above the first 'auth' line in any /etc/pam.d/<service> file.",
                ),
                &[("line", line.clone())],
            ),
            PamInvalidServiceName { service } => fill(
                translate(
                    "Invalid PAM service name '{service}': a service is one file name under /etc/pam.d, so it may not be empty, contain '/', or be '.' or '..'.",
                ),
                &[("service", service.clone())],
            ),
            PamServiceSymlinkedOutside { path, target, dir } => fill(
                translate(
                    "Refusing to touch {path}: it is a symlink to {target}. PAM service links are never followed beneath {dir}.\nEdit the real service file directly, or the tool that generates it — authselect regenerates system-auth and password-auth from /etc/authselect.",
                ),
                &[
                    ("path", path.clone()),
                    ("target", target.clone()),
                    ("dir", dir.clone()),
                ],
            ),
            PamServiceHardLinked { path, links } => fill(
                translate(
                    "Refusing to touch {path}: it is one of {links} names for the same file, so an edit here would change a file outside /etc/pam.d that facelock cannot name.\nBreak the link first: sudo cp -p {path} {path}.new && sudo mv {path}.new {path}",
                ),
                &[("path", path.clone()), ("links", links.clone())],
            ),
            PamSensitiveRefused { service, remedy } => fill(
                translate(
                    "Refusing to modify '{service}': this is a sensitive PAM service.\nRe-run with {remedy} to accept the risk of locking yourself out.",
                ),
                &[("service", service.clone()), ("remedy", remedy.clone())],
            ),
            PamModuleNotInstalled { paths, path } => fill(
                translate(
                    "PAM module not found. Tried: {paths}\nInstall it first: cargo build --release -p pam-facelock && sudo cp target/release/libpam_facelock.so {path}",
                ),
                &[("paths", paths.clone()), ("path", path.clone())],
            ),
            PamServiceAbsentSkipped { path } => fill(
                translate("PAM service file absent: {path}. Nothing to add."),
                &[("path", path.clone())],
            ),
            PamPlanAdd { path, hint } => fill(
                translate("Would add the facelock PAM line to {path} ({hint})."),
                &[("path", path.clone()), ("hint", hint.clone())],
            ),
            PamPlanRemove { path } => fill(
                translate("Would remove the facelock PAM line from {path}."),
                &[("path", path.clone())],
            ),
            PamPlanNoChange { path } => fill(
                translate("No change needed for {path}."),
                &[("path", path.clone())],
            ),
            PamPlanAbsent { path } => fill(
                translate("PAM service file absent: {path}. Nothing to do."),
                &[("path", path.clone())],
            ),
            PamStatusPresent { path } => fill(
                translate("{path}: facelock PAM line present"),
                &[("path", path.clone())],
            ),
            PamStatusMissing { path } => fill(
                translate("{path}: no facelock PAM line"),
                &[("path", path.clone())],
            ),
            PamStatusAbsent { paths } => fill(
                translate("{paths}: service file absent"),
                &[("paths", paths.clone())],
            ),
            PamStatusUnknown { path, error } => fill(
                translate("{path}: unreadable ({error})"),
                &[("path", path.clone()), ("error", error.clone())],
            ),
            PamStatusOverride { path, vendor } => fill(
                translate("{path}: facelock PAM line present (local override of {vendor})"),
                &[("path", path.clone()), ("vendor", vendor.clone())],
            ),
            PamStatusDirUnreadable { dir, error } => fill(
                translate("{dir}: directory not checked ({error})"),
                &[("dir", dir.clone()), ("error", error.clone())],
            ),
            PamStatusNoServices { dirs } => fill(
                translate("No service file under {dirs} carries the facelock PAM line."),
                &[("dirs", dirs.clone())],
            ),
            PamStatusNoServicesIncomplete { dirs, unchecked } => fill(
                translate(
                    "No service file under {dirs} carries the facelock PAM line, but {unchecked} could not be checked, so this is not an answer about the whole machine.",
                ),
                &[("dirs", dirs.clone()), ("unchecked", unchecked.clone())],
            ),
            PamStatusNothingReadable { dirs } => fill(
                translate("No directory on the search path could be read: {dirs}."),
                &[("dirs", dirs.clone())],
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
impl super::Samples for PamMessage {
    const VARIANT_COUNT: usize = 54;

    fn samples() -> Vec<Self> {
        use PamMessage::*;
        vec![
            PamSkippedFlag { dir: s("/d") },
            PamModuleMissing { paths: s("/p") },
            ConfiguringPamFor { service: s("sudo") },
            NoPamCandidates { dir: s("/d") },
            PamLinePreview {
                line: s("auth ..."),
            },
            PromptSelectPamServices,
            PamConfigureFailed {
                service: s("sudo"),
                error: s("e"),
            },
            PamBackupCleanupFailed {
                service: s("sudo"),
                error: s("e"),
            },
            NoPamServicesSelected,
            PamFileNotFound { paths: s("/p") },
            PamLineAlreadyPresent { path: s("/p") },
            PamInsertBeforeAuthHint,
            PamInsertAfterHeaderHint,
            PamInsertAtTopHint,
            PamModifyPreview {
                path: s("/p"),
                line: s("auth"),
                hint: s("h"),
                backup: s("/b"),
            },
            ConfirmProceed,
            PamSkippedFile { path: s("/p") },
            PamBackedUp {
                path: s("/p"),
                backup: s("/b"),
            },
            PamInstalled {
                path: s("/p"),
                backup: s("/b"),
                service: s("sudo"),
            },
            PamRemoved { path: s("/p") },
            PamNoLineFound { path: s("/p") },
            PamServiceAbsent { path: s("/p") },
            PamBackupExists {
                path: s("/p"),
                backup: s("/b"),
            },
            PamOverridePreview {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
                line: s("auth"),
                hint: s("h"),
            },
            PamVendorOverridden {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
                service: s("polkit-1"),
            },
            PamVendorOverrideRemoved {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
            },
            PamVendorOverrideRetained {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
            },
            PamVendorOverrideSourceAbsent {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
            },
            PamVendorOverrideSourceAbsentNoLine {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
            },
            PamPlanDeleteOverride {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
            },
            PamVendorOnly {
                path: s("/usr/lib/pam.d/polkit-1"),
            },
            PamStatusVendorOnly {
                path: s("/usr/lib/pam.d/polkit-1"),
            },
            PamPlanOverride {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
                hint: s("h"),
            },
            PamOverrideDirUnwritable {
                dir: s("/etc/pam.d"),
                path: s("/etc/pam.d/polkit-1"),
                error: s("e"),
            },
            PamExtensionHint {
                line: s("auth ..."),
            },
            PamInvalidServiceName { service: s("../x") },
            PamServiceSymlinkedOutside {
                path: s("/etc/pam.d/system-auth"),
                target: s("/etc/authselect/system-auth"),
                dir: s("/etc/pam.d"),
            },
            PamServiceHardLinked {
                path: s("/etc/pam.d/sudo"),
                links: s("2"),
            },
            PamSensitiveRefused {
                service: s("sshd"),
                remedy: s("--allow-sensitive"),
            },
            PamModuleNotInstalled {
                paths: s("/lib/security/pam_facelock.so, /usr/lib64/security/pam_facelock.so"),
                path: s("/lib/security/pam_facelock.so"),
            },
            PamServiceAbsentSkipped { path: s("/p") },
            PamPlanAdd {
                path: s("/p"),
                hint: s("h"),
            },
            PamPlanRemove { path: s("/p") },
            PamPlanNoChange { path: s("/p") },
            PamPlanAbsent { path: s("/p") },
            PamStatusPresent { path: s("/p") },
            PamStatusMissing { path: s("/p") },
            PamStatusAbsent { paths: s("/p") },
            PamStatusUnknown {
                path: s("/p"),
                error: s("e"),
            },
            PamStatusOverride {
                path: s("/etc/pam.d/polkit-1"),
                vendor: s("/usr/lib/pam.d/polkit-1"),
            },
            PamStatusDirUnreadable {
                dir: s("/etc/pam.d"),
                error: s("Permission denied (os error 13)"),
            },
            PamStatusNoServices {
                dirs: s("/etc/pam.d, /usr/lib/pam.d"),
            },
            PamStatusNoServicesIncomplete {
                dirs: s("/etc/pam.d"),
                unchecked: s("/usr/lib/pam.d"),
            },
            PamStatusNothingReadable {
                dirs: s("/etc/pam.d, /usr/lib/pam.d"),
            },
        ]
    }
}
