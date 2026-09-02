//! Wiring facelock into the system: the daemon unit and legacy group cleanup.

#[cfg(test)]
use super::sample_text as s;
use super::{Message, fill, translate};

/// Daemon unit setup and legacy `facelock` group cleanup.
///
/// Variant and field names are the machine vocabulary: [`Message::machine`]
/// derives its event line from them.
#[derive(Debug, Clone, PartialEq)]
pub enum SystemMessage {
    // -- enrollment and test steps --
    ConfirmDaemonMode,
    SystemdNotDetected,
    SystemdDeclined,
    SystemdSkippedFlag,
    SystemdFromCommandLine,

    // -- a non-default `--config` (#314): the unit reads only the default --
    SystemdSkippedConfigOverride { path: String },
    SystemdRefusedConfigOverride { path: String },
    SystemdDisableConfigOverride { path: String },

    // -- bringing the daemon up --
    DaemonRestarted,
    DaemonRunning,
    DaemonNotReady { seconds: u64 },

    // -- the legacy facelock system group (ADR 010) --
    RetiredFacelockGroup,

    // -- validating installed assets and controlling the unit --
    DisablingSystemdUnits,
    SystemdUnitsDisabled,
    ValidatingSystemAssets,
    RemovedLegacySystemAsset { path: String },
    PreservedLocalDbusPolicy { path: String },
    SystemAssetsValidated,
    SystemctlDaemonReloadDone,
    SystemctlEnableDone { unit: String },
    DbusActivationEnabled,
}

impl Message for SystemMessage {
    fn localized(&self) -> String {
        use SystemMessage::*;
        match self {
            ConfirmDaemonMode => translate("Enable daemon mode with D-Bus activation?"),
            SystemdNotDetected => translate(
                "  systemd not detected. Skipping daemon configuration.\n  Facelock will use oneshot mode for authentication.",
            ),
            SystemdDeclined => {
                translate("  Skipping systemd setup. Facelock will use oneshot mode.")
            }
            SystemdSkippedFlag => translate(
                "  Skipping daemon configuration (--no-systemd).\n  No unit files are written and systemctl is not invoked.",
            ),
            SystemdFromCommandLine => translate("  Answered on the command line."),
            // The default path is spelled out in both: it is the whole reason,
            // and the refusal's two ways out are named so the operator does
            // not have to work them out from the diagnosis.
            SystemdSkippedConfigOverride { path } => fill(
                translate(
                    "  Skipping daemon configuration: --config {path} is not /etc/facelock/config.toml,\n  the only file the facelock-daemon unit reads.\n  Enrollment and the recognition test use direct camera access under {path}.",
                ),
                &[("path", path.clone())],
            ),
            SystemdRefusedConfigOverride { path } => fill(
                translate(
                    "--systemd is not supported with --config {path}: the facelock-daemon unit runs `facelock daemon`,\n  which reads only /etc/facelock/config.toml, so the daemon it enables would not use this configuration.\n  Either copy {path} to /etc/facelock/config.toml and re-run without --config,\n  or re-run without --systemd to enroll with direct camera access under {path}.",
                ),
                &[("path", path.clone())],
            ),
            // Disabling reads no config file and goes ahead; the note says
            // which daemon that is, since the named file is not its.
            SystemdDisableConfigOverride { path } => fill(
                translate(
                    "Note: --config {path} names another file; this stops the unit that reads /etc/facelock/config.toml.",
                ),
                &[("path", path.clone())],
            ),
            DaemonRestarted => translate(
                "  facelock-daemon was already running; restarted so enrollment uses\n  the new configuration.",
            ),
            DaemonRunning => translate("  facelock-daemon is running."),
            DaemonNotReady { seconds } => fill(
                translate(
                    "  facelock-daemon did not answer within {seconds}s.\n  Continuing with direct camera access; check: systemctl status facelock-daemon",
                ),
                &[("seconds", seconds.to_string())],
            ),
            RetiredFacelockGroup => {
                translate("  Removed the legacy 'facelock' group; face unlock no longer uses it.")
            }
            DisablingSystemdUnits => translate("Disabling facelock-daemon systemd units..."),
            SystemdUnitsDisabled => translate("facelock-daemon service disabled and stopped."),
            ValidatingSystemAssets => {
                translate("Validating installed facelock-daemon systemd and D-Bus assets...")
            }
            RemovedLegacySystemAsset { path } => fill(
                translate("  Removed exact known legacy {path}"),
                &[("path", path.clone())],
            ),
            PreservedLocalDbusPolicy { path } => fill(
                translate(
                    "  Preserved merged local D-Bus policy {path}; review it if Facelock bus authorization is unexpected.",
                ),
                &[("path", path.clone())],
            ),
            SystemAssetsValidated => translate("  Installed systemd and D-Bus assets validated."),
            SystemctlDaemonReloadDone => translate("  systemctl daemon-reload done."),
            SystemctlEnableDone { unit } => fill(
                translate("  systemctl enable {unit} done."),
                &[("unit", unit.clone())],
            ),
            DbusActivationEnabled => {
                translate("\nfacelock-daemon D-Bus activation is now enabled.")
            }
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
impl super::Samples for SystemMessage {
    const VARIANT_COUNT: usize = 21;

    fn samples() -> Vec<Self> {
        use SystemMessage::*;
        vec![
            ConfirmDaemonMode,
            SystemdNotDetected,
            SystemdDeclined,
            SystemdSkippedFlag,
            SystemdFromCommandLine,
            SystemdSkippedConfigOverride { path: s("/p") },
            SystemdRefusedConfigOverride { path: s("/p") },
            SystemdDisableConfigOverride { path: s("/p") },
            DaemonRestarted,
            DaemonRunning,
            DaemonNotReady { seconds: 20 },
            RetiredFacelockGroup,
            DisablingSystemdUnits,
            SystemdUnitsDisabled,
            ValidatingSystemAssets,
            RemovedLegacySystemAsset { path: s("/p") },
            PreservedLocalDbusPolicy { path: s("/p") },
            SystemAssetsValidated,
            SystemctlDaemonReloadDone,
            SystemctlEnableDone {
                unit: s("u.service"),
            },
            DbusActivationEnabled,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unit-install narration, pinned to the bytes `run_systemd`
    /// printed. `facelock setup --systemd` is a scripted entry point
    /// (packaging, Omarchy), so these lines are read by more than humans.
    #[test]
    fn system_fallback_is_byte_identical() {
        use SystemMessage::*;

        assert_eq!(
            DisablingSystemdUnits.localized(),
            "Disabling facelock-daemon systemd units..."
        );
        assert_eq!(
            SystemdUnitsDisabled.localized(),
            "facelock-daemon service disabled and stopped."
        );
        assert_eq!(
            ValidatingSystemAssets.localized(),
            "Validating installed facelock-daemon systemd and D-Bus assets..."
        );
        assert_eq!(
            RemovedLegacySystemAsset {
                path: "/etc/systemd/system/facelock-daemon.service".into()
            }
            .localized(),
            "  Removed exact known legacy /etc/systemd/system/facelock-daemon.service"
        );
        assert_eq!(
            PreservedLocalDbusPolicy {
                path: "/etc/dbus-1/system.d/90-facelock-admin.conf".into()
            }
            .localized(),
            "  Preserved merged local D-Bus policy /etc/dbus-1/system.d/90-facelock-admin.conf; review it if Facelock bus authorization is unexpected."
        );
        assert_eq!(
            SystemAssetsValidated.localized(),
            "  Installed systemd and D-Bus assets validated."
        );
        assert_eq!(
            SystemctlDaemonReloadDone.localized(),
            "  systemctl daemon-reload done."
        );
        assert_eq!(
            SystemctlEnableDone {
                unit: "facelock-daemon.service".into()
            }
            .localized(),
            "  systemctl enable facelock-daemon.service done."
        );
        assert_eq!(
            DbusActivationEnabled.localized(),
            "\nfacelock-daemon D-Bus activation is now enabled."
        );
    }

    /// The two `--config` lines (#314) spell the default path out because it
    /// is the whole reason; a moved default must move them too.
    #[test]
    fn config_override_lines_name_the_default_path() {
        use SystemMessage::*;
        let default = facelock_core::paths::DEFAULT_CONFIG_PATH;

        let skipped = SystemdSkippedConfigOverride {
            path: "/tmp/x.toml".into(),
        }
        .localized();
        assert!(skipped.contains(default), "{skipped}");
        assert!(skipped.contains("--config /tmp/x.toml"), "{skipped}");
        assert!(skipped.contains("under /tmp/x.toml"), "{skipped}");

        let refused = SystemdRefusedConfigOverride {
            path: "/tmp/x.toml".into(),
        }
        .localized();
        assert!(refused.contains(default), "{refused}");
        assert!(refused.contains("--config /tmp/x.toml"), "{refused}");
        assert!(refused.contains("copy /tmp/x.toml to"), "{refused}");
        assert!(refused.contains("without --config"), "{refused}");
        assert!(refused.contains("without --systemd"), "{refused}");

        let disabling = SystemdDisableConfigOverride {
            path: "/tmp/x.toml".into(),
        }
        .localized();
        assert!(disabling.contains(default), "{disabling}");
        assert!(disabling.contains("--config /tmp/x.toml"), "{disabling}");
        assert!(disabling.contains("stops the unit"), "{disabling}");
    }
}
