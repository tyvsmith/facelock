//! The `facelock status` report's prose.
//!
//! The report *skeleton* — item lines, `[ok]`/`[!!]` markers, `- key:` detail
//! rows — stays structural in the renderer, and config-key-shaped detail keys
//! (`require_ir`, `device.path`, `quirks`, ...) are vocabulary rather than
//! prose, so they stay literal there. Only the sentences live here.

#[cfg(test)]
use super::sample_text as s;
use super::{Message, fill, translate};

/// The `facelock status` report's prose.
///
/// Variant and field names are the machine vocabulary: [`Message::machine`]
/// derives its event line from them.
#[derive(Debug, Clone, PartialEq)]
pub enum StatusMessage {
    StatusHeader,
    StatusLabelConfigFile,
    StatusLabelDaemon,
    StatusLabelOneshotFallback,
    StatusLabelCameraDevice,
    StatusLabelModelDirectory,
    StatusLabelExecutionProvider,
    StatusLabelEncryption,
    StatusLabelEnrolledFaces,
    StatusLabelSecurity,
    StatusLabelNotifications,
    StatusLabelPamModule,
    /// The unknown-is-not-false rendering (N4): the answer could not be
    /// determined, and no value is guessed in its place.
    StatusUnknown {
        why: String,
    },
    /// The one `why` facelock authors itself, rather than quoting from a
    /// probe: every config-dependent fact is undeterminable when the config
    /// did not parse. It is a catalog entry because it is prose that reaches
    /// a user, and it is composed into [`StatusMessage::StatusUnknown`]'s
    /// sentence — so leaving it as a bare Rust literal would embed English
    /// inside an otherwise localized line.
    StatusWhyConfigNotAvailable,
    StatusConfigValid,
    StatusConfigNotFound,
    StatusConfigInvalid {
        error: String,
    },
    StatusDaemonOneshot,
    StatusDaemonResponding,
    StatusDaemonNotResponding {
        error: String,
    },
    StatusFallbackUsable,
    StatusFallbackNotUsable,
    StatusCameraDeviceExists,
    StatusCameraDeviceNotFound,
    StatusCameraAutoDetect,
    StatusModelsDirNotFound,
    StatusModelsAllPresent,
    StatusModelsSomeMissing,
    StatusPresent,
    StatusMissing,
    StatusEpSupported,
    StatusEpNotBuiltIn,
    StatusEpUnknownName,
    StatusEpUnqueryable {
        error: String,
    },
    StatusSealedKey {
        path: String,
    },
    StatusSealedKeyMissing {
        path: String,
    },
    StatusTpmDeviceMissing {
        path: String,
    },
    StatusKeyFile {
        path: String,
    },
    StatusKeyFileMissing {
        path: String,
    },
    StatusPlaintextEmbeddings,
    StatusNoFacesEnrolled,
    StatusModelCount {
        count: usize,
    },
    /// A template with no device id under hard device binding: it still
    /// authenticates, and only a re-enrollment binds it (#312).
    StatusModelUnbound {
        label: String,
    },
    StatusMarkerMismatch {
        marker: u32,
        store: u32,
    },
    StatusMarkerUnreadable {
        why: String,
    },
    StatusSecurityDisabled,
    StatusYes,
    StatusNo,
    StatusNotifyOff,
    StatusNotifyTerminal,
    StatusNotifyDesktop,
    StatusNotifyBoth,
    StatusPamInstalled,
    StatusPamInstalledAt {
        path: String,
    },
    StatusPamNotInstalled,
    /// The services that carry the facelock line, listed and counted.
    /// `overrides` is how many of them are a local copy of a package's file,
    /// which is the one thing about a configured service that needs saying:
    /// the copy will not follow the package's updates.
    StatusPamServices {
        services: String,
        count: String,
    },
    StatusPamServicesWithOverrides {
        services: String,
        count: String,
        overrides: String,
    },
    /// Nothing carries the line, and the scan saw everywhere it meant to.
    StatusPamNoServices,
    /// Nothing carries the line **and** somewhere could not be read, so this
    /// is not an answer about the machine. N4, applied to PAM: an unknown is
    /// never rendered as a value.
    StatusPamNotChecked,
    /// One place the scan could not get an answer from.
    StatusPamNotCheckedAt {
        path: String,
        error: String,
    },
}

impl Message for StatusMessage {
    fn localized(&self) -> String {
        use StatusMessage::*;
        match self {
            StatusHeader => translate("facelock system status"),
            StatusLabelConfigFile => translate("Config file"),
            StatusLabelDaemon => translate("Daemon"),
            StatusLabelOneshotFallback => translate("Oneshot fallback"),
            StatusLabelCameraDevice => translate("Camera device"),
            StatusLabelModelDirectory => translate("Model directory"),
            StatusLabelExecutionProvider => translate("Execution provider"),
            StatusLabelEncryption => translate("Encryption"),
            StatusLabelEnrolledFaces => translate("Enrolled faces"),
            StatusLabelSecurity => translate("Security"),
            StatusLabelNotifications => translate("Notifications"),
            StatusLabelPamModule => translate("PAM module"),
            StatusUnknown { why } => fill(
                translate("cannot determine: {why}"),
                &[("why", why.clone())],
            ),
            StatusWhyConfigNotAvailable => translate("config not available"),
            StatusConfigValid => translate("valid"),
            StatusConfigNotFound => translate("not found"),
            StatusConfigInvalid { error } => {
                fill(translate("invalid: {error}"), &[("error", error.clone())])
            }
            StatusDaemonOneshot => translate("oneshot mode (no daemon)"),
            StatusDaemonResponding => translate("responding"),
            StatusDaemonNotResponding { error } => fill(
                translate("not responding: {error}"),
                &[("error", error.clone())],
            ),
            StatusFallbackUsable => translate(
                "prerequisites present (binary, models and database in place for daemon-less auth)",
            ),
            StatusFallbackNotUsable => translate(
                "prerequisites missing (PAM would fall through to the next auth method if the daemon is unreachable)",
            ),
            StatusCameraDeviceExists => translate("device exists"),
            StatusCameraDeviceNotFound => translate("device not found"),
            StatusCameraAutoDetect => translate("auto-detect enabled"),
            StatusModelsDirNotFound => translate("directory not found"),
            StatusModelsAllPresent => translate("all configured models present"),
            StatusModelsSomeMissing => translate("some models missing (run 'facelock setup')"),
            StatusPresent => translate("present"),
            StatusMissing => translate("MISSING"),
            StatusEpSupported => translate("supported by the installed ONNX Runtime"),
            StatusEpNotBuiltIn => translate(
                "not built into the installed ONNX Runtime — inference will fall back to CPU",
            ),
            // The provider names are spelled out rather than joined from
            // `ProviderKind::all_names()` because this string is translated:
            // an assembled sentence cannot be localized. The literal is held
            // to the enum by `unknown_provider_hint_lists_every_provider`.
            StatusEpUnknownName => {
                translate("unknown execution provider (valid: cpu, cuda, rocm, openvino)")
            }
            StatusEpUnqueryable { error } => fill(
                translate("ONNX Runtime not loadable: {error}"),
                &[("error", error.clone())],
            ),
            StatusSealedKey { path } => {
                fill(translate("sealed key: {path}"), &[("path", path.clone())])
            }
            StatusSealedKeyMissing { path } => fill(
                translate("sealed key missing: {path}"),
                &[("path", path.clone())],
            ),
            StatusTpmDeviceMissing { path } => fill(
                translate("TPM device missing: {path}"),
                &[("path", path.clone())],
            ),
            StatusKeyFile { path } => {
                fill(translate("key file: {path}"), &[("path", path.clone())])
            }
            StatusKeyFileMissing { path } => fill(
                translate("key file missing: {path}"),
                &[("path", path.clone())],
            ),
            StatusPlaintextEmbeddings => translate(
                "embeddings stored as plaintext (run 'facelock setup' to enable encryption)",
            ),
            StatusNoFacesEnrolled => translate("no faces enrolled (run 'facelock enroll')"),
            StatusModelCount { count } => fill(
                translate("{count} model(s)"),
                &[("count", count.to_string())],
            ),
            StatusModelUnbound { label } => fill(
                translate("{label}, unbound (re-enroll to bind)"),
                &[("label", label.clone())],
            ),
            StatusMarkerMismatch { marker, store } => fill(
                translate(
                    "out of date (marker says {marker}, database has {store}) — run 'sudo facelock setup' to reconcile",
                ),
                &[("marker", marker.to_string()), ("store", store.to_string())],
            ),
            StatusMarkerUnreadable { why } => {
                fill(translate("unreadable: {why}"), &[("why", why.clone())])
            }
            StatusSecurityDisabled => translate("ALL SECURITY CHECKS DISABLED"),
            StatusYes => translate("yes"),
            StatusNo => translate("no"),
            StatusNotifyOff => translate("off"),
            StatusNotifyTerminal => translate("terminal"),
            StatusNotifyDesktop => translate("desktop"),
            StatusNotifyBoth => translate("terminal + desktop"),
            StatusPamInstalled => translate("installed"),
            StatusPamInstalledAt { path } => {
                fill(translate("installed at {path}"), &[("path", path.clone())])
            }
            StatusPamNotInstalled => translate("not installed"),
            StatusPamServices { services, count } => fill(
                translate("{services} ({count} configured)"),
                &[("services", services.clone()), ("count", count.clone())],
            ),
            StatusPamServicesWithOverrides {
                services,
                count,
                overrides,
            } => fill(
                translate("{services} ({count} configured, {overrides} shadowing a vendor file)"),
                &[
                    ("services", services.clone()),
                    ("count", count.clone()),
                    ("overrides", overrides.clone()),
                ],
            ),
            StatusPamNoServices => translate("none configured"),
            StatusPamNotChecked => translate("not checked"),
            StatusPamNotCheckedAt { path, error } => fill(
                translate("{path} ({error})"),
                &[("path", path.clone()), ("error", error.clone())],
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
impl super::Samples for StatusMessage {
    const VARIANT_COUNT: usize = 60;

    fn samples() -> Vec<Self> {
        use StatusMessage::*;
        vec![
            StatusHeader,
            StatusLabelConfigFile,
            StatusLabelDaemon,
            StatusLabelOneshotFallback,
            StatusLabelCameraDevice,
            StatusLabelModelDirectory,
            StatusLabelExecutionProvider,
            StatusLabelEncryption,
            StatusLabelEnrolledFaces,
            StatusLabelSecurity,
            StatusLabelNotifications,
            StatusLabelPamModule,
            StatusUnknown { why: s("w") },
            StatusWhyConfigNotAvailable,
            StatusConfigValid,
            StatusConfigNotFound,
            StatusConfigInvalid { error: s("e") },
            StatusDaemonOneshot,
            StatusDaemonResponding,
            StatusDaemonNotResponding { error: s("e") },
            StatusFallbackUsable,
            StatusFallbackNotUsable,
            StatusCameraDeviceExists,
            StatusCameraDeviceNotFound,
            StatusCameraAutoDetect,
            StatusModelsDirNotFound,
            StatusModelsAllPresent,
            StatusModelsSomeMissing,
            StatusPresent,
            StatusMissing,
            StatusEpSupported,
            StatusEpNotBuiltIn,
            StatusEpUnknownName,
            StatusEpUnqueryable { error: s("e") },
            StatusSealedKey { path: s("/p") },
            StatusSealedKeyMissing { path: s("/p") },
            StatusTpmDeviceMissing { path: s("/p") },
            StatusKeyFile { path: s("/p") },
            StatusKeyFileMissing { path: s("/p") },
            StatusPlaintextEmbeddings,
            StatusNoFacesEnrolled,
            StatusModelCount { count: 2 },
            StatusModelUnbound { label: s("front") },
            StatusMarkerMismatch {
                marker: 3,
                store: 2,
            },
            StatusMarkerUnreadable { why: s("w") },
            StatusSecurityDisabled,
            StatusYes,
            StatusNo,
            StatusNotifyOff,
            StatusNotifyTerminal,
            StatusNotifyDesktop,
            StatusNotifyBoth,
            StatusPamInstalled,
            StatusPamInstalledAt { path: s("/p") },
            StatusPamNotInstalled,
            StatusPamServices {
                services: s("sudo, polkit-1"),
                count: s("2"),
            },
            StatusPamServicesWithOverrides {
                services: s("sudo, polkit-1"),
                count: s("2"),
                overrides: s("1"),
            },
            StatusPamNoServices,
            StatusPamNotChecked,
            StatusPamNotCheckedAt {
                path: s("/usr/lib/pam.d"),
                error: s("Permission denied (os error 13)"),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift pin for the one place a provider list is written out by hand.
    /// `facelock-face` derives every other list from `ProviderKind::ALL`;
    /// this hint cannot, because it is a translated sentence. So instead of
    /// assembling it, hold it to the enum: adding a provider without
    /// mentioning it here (and in the translations) fails this test.
    #[test]
    fn unknown_provider_hint_lists_every_provider() {
        let hint = StatusMessage::StatusEpUnknownName.localized();
        for name in facelock_face::ProviderKind::all_names() {
            assert!(
                hint.contains(name),
                "execution-provider hint omits {name:?}: {hint}"
            );
        }
    }
}
