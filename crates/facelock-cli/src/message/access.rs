//! Getting in: config load, privilege, and which backend answered.
//!
//! What a command says before it does its own work — the config file is
//! missing or unparseable, the operation needs root, the bus refused us, or
//! the daemon is not there and the direct path took over.

#[cfg(test)]
use super::sample_text as s;
use super::{Message, fill, translate};

/// Getting in: config, privilege, backend.
///
/// Variant and field names are the machine vocabulary: [`Message::machine`]
/// derives its event line from them.
#[derive(Debug, Clone, PartialEq)]
pub enum AccessMessage {
    // -- config load (ConfigLoad::require, D7) --
    NoConfigFile { path: String },
    InvalidConfig { path: String, error: String },

    // -- privilege and access --
    RootRequired { hint: String },
    SudoReexecPrompt,
    AccessDeniedRootHint,
    EnrollTimedOutClientSide,

    // -- backend selection (D1) --
    DaemonUnreachableFallback,
    DirectByConfigOverride,
    PreviewGraphicalNeedsDaemonOneshot,
    PreviewGraphicalDaemonUnreachable,
    PreviewGraphicalDaemonConfigOverride,
}

impl Message for AccessMessage {
    fn localized(&self) -> String {
        use AccessMessage::*;
        match self {
            NoConfigFile { path } => fill(
                translate("no config file at {path} — run 'sudo facelock setup' to create one"),
                &[("path", path.clone())],
            ),
            InvalidConfig { path, error } => fill(
                translate("invalid config at {path}: {error}"),
                &[("path", path.clone()), ("error", error.clone())],
            ),
            RootRequired { hint } => fill(
                translate("Root required.\n  Run: {hint}"),
                &[("hint", hint.clone())],
            ),
            // The "[Y/n]" hint is appended by the sink, not carried here:
            // `Terminal::confirm_default_yes` owns both the hint and the
            // English answer tokens it parses.
            SudoReexecPrompt => translate("Root required. Re-run with sudo?"),
            AccessDeniedRootHint => translate(
                "Access denied: this operation requires root.\n  Re-run with sudo, or as root.",
            ),
            EnrollTimedOutClientSide => translate(
                "enrollment timed out client-side; the daemon may have completed it — run `facelock list` before retrying",
            ),
            DaemonUnreachableFallback => translate(
                "Warning: daemon.mode = \"daemon\" but the facelock daemon is unreachable — falling back to direct camera access.\nStart the facelock-daemon service to use it.",
            ),
            // Not a warning: the operator chose the file, and the daemon on
            // the bus is doing nothing wrong by reading the default one. The
            // default path is spelled out because it is the whole reason.
            DirectByConfigOverride => translate(
                "Note: --config names a file other than /etc/facelock/config.toml, the only file the facelock daemon reads.\nUsing direct camera access under the selected configuration.",
            ),
            // The flag is `--json` since #169; `--text-only` is a hidden
            // alias that `preview --help` no longer lists, so naming it here
            // taught a spelling the program had stopped showing. "text-only
            // mode" in the second sentence describes the mode, not the flag,
            // and stays.
            PreviewGraphicalNeedsDaemonOneshot => translate(
                "Graphical preview requires the daemon. In oneshot mode, use --json.\nFalling back to text-only mode.\n",
            ),
            PreviewGraphicalDaemonUnreachable => translate(
                "Graphical preview requires the daemon, which is configured but not reachable.\nFalling back to text-only mode. Start the facelock-daemon service to use it.\n",
            ),
            PreviewGraphicalDaemonConfigOverride => translate(
                "Graphical preview requires the daemon, which reads only /etc/facelock/config.toml and is not used under --config.\nFalling back to text-only mode.\n",
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
impl super::Samples for AccessMessage {
    const VARIANT_COUNT: usize = 11;

    fn samples() -> Vec<Self> {
        use AccessMessage::*;
        vec![
            NoConfigFile { path: s("/p") },
            InvalidConfig {
                path: s("/p"),
                error: s("e"),
            },
            RootRequired { hint: s("h") },
            SudoReexecPrompt,
            AccessDeniedRootHint,
            EnrollTimedOutClientSide,
            DaemonUnreachableFallback,
            DirectByConfigOverride,
            PreviewGraphicalNeedsDaemonOneshot,
            PreviewGraphicalDaemonUnreachable,
            PreviewGraphicalDaemonConfigOverride,
        ]
    }
}
