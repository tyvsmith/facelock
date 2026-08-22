//! Shared clap argument groups for the `facelock` binary.
//!
//! Flag spelling is a compatibility surface — `pam_facelock.so` spawns a
//! `facelock auth` argv byte for byte, and wrapper scripts hard-code the rest —
//! but it was previously re-declared per command, so it drifted: `--user` had
//! `-u` on six commands and not on `auth`, `--yes` accepted `--no-confirm` on
//! `setup` alone. One struct per flag family removes the opportunity: a command
//! flattens the family or does not offer the flag, and cannot spell it a third
//! way. `cli_flag_conformance` in `conformance/flags.rs` fails the build if
//! one tries.
//!
//! These types are deliberately bin-private (declared by `main.rs`, absent from
//! `lib.rs`). Nothing outside the binary consumes clap types.

use clap::{Args, Subcommand};

use facelock_cli::commands::pam::{PamAction, PamRequest};
use facelock_cli::commands::setup::{
    EncryptionChoice, ExecutionProviderChoice, ModelPreset, SetupArgs,
};

/// The user a command operates on.
#[derive(Args)]
pub struct UserArg {
    /// Username (default: current user)
    #[arg(short = 'u', long)]
    pub user: Option<String>,
}

/// Confirmation bypass for commands whose prompt is an ordinary confirmation.
/// It never grants a separate authorization such as `--allow-sensitive`.
///
/// `--no-confirm` shipped on `setup` only; it is carried as a hidden alias on
/// every site so a wrapper written against either spelling keeps working.
#[derive(Args)]
pub struct ConfirmArg {
    /// Skip confirmation prompts (also: --no-confirm)
    #[arg(short = 'y', long, alias = "no-confirm")]
    pub yes: bool,
}

/// Machine-readable output. The payload goes to stdout and nothing else does —
/// see `docs/contracts.md`, "CLI Output Streams".
#[derive(Args)]
pub struct JsonArg {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Declared ahead of its first consumer so the spelling is fixed once rather
/// than invented per command: the destructive commands flatten it in gap G3
/// (#168). No `#[allow(dead_code)]` is needed in the meantime — the derived
/// `FromArgMatches` reads the field, so it does not read as dead.
#[derive(Args)]
pub struct DryRunArg {
    /// Report what would change, and change nothing
    #[arg(long)]
    pub dry_run: bool,
}

/// `facelock setup`'s command line.
///
/// Its only job is to become a [`SetupArgs`]. Naming the 17 fields once means
/// `main` and the tests reach the resolver through the same conversion; when
/// they were two hand-written destructures, a field added to one could be
/// silently missing from the other.
#[derive(Args)]
pub struct SetupCli {
    /// Run in non-interactive mode (skip wizard)
    #[arg(long)]
    pub non_interactive: bool,
    #[command(flatten)]
    pub confirm: ConfirmArg,

    // -- Action pairs. A later flag wins over an earlier one, so a wrapper
    //    script can append an override to a command it did not construct.
    /// Install or manage PAM module configuration
    #[arg(long, overrides_with = "no_pam")]
    pub pam: bool,
    /// Do not touch PAM configuration at all (no prompt, no write)
    #[arg(long = "no-pam", overrides_with = "pam")]
    pub no_pam: bool,
    /// Install and enable systemd units
    #[arg(long, overrides_with = "no_systemd")]
    pub systemd: bool,
    /// Do not install or enable systemd units
    #[arg(long = "no-systemd", overrides_with = "systemd")]
    pub no_systemd: bool,
    /// Enroll a face during setup
    #[arg(long, overrides_with = "no_enroll")]
    pub enroll: bool,
    /// Do not enroll a face during setup
    #[arg(long = "no-enroll", overrides_with = "enroll")]
    pub no_enroll: bool,

    // -- Action modifiers
    /// Used with --systemd: disable and stop systemd units instead
    #[arg(long, requires = "systemd")]
    pub disable: bool,
    /// Used with --pam: target PAM service (default: sudo)
    #[arg(long, requires = "pam")]
    pub service: Option<String>,
    /// Used with --pam: remove the PAM line instead of adding it
    #[arg(long, requires = "pam")]
    pub remove: bool,
    /// Used with --pam: treat an absent service file as success
    #[arg(long = "if-present", requires = "pam")]
    pub if_present: bool,
    /// Used with --pam: permit sensitive PAM services
    #[arg(long = "allow-sensitive", requires = "pam", conflicts_with = "remove")]
    pub allow_sensitive: bool,

    // -- Choice flags. Supplying a value answers the question, and so skips
    //    the matching wizard step.
    /// Camera device path, or `auto` to re-detect from hardware
    #[arg(long)]
    pub camera: Option<String>,
    /// Model quality preset
    #[arg(long, value_enum)]
    pub models: Option<ModelPreset>,
    /// ONNX Runtime execution provider
    #[arg(long, value_enum)]
    pub execution_provider: Option<ExecutionProviderChoice>,
    /// Embedding encryption method
    #[arg(long, value_enum)]
    pub encryption: Option<EncryptionChoice>,
}

impl From<SetupCli> for SetupArgs {
    fn from(cli: SetupCli) -> Self {
        let SetupCli {
            non_interactive,
            confirm,
            pam,
            no_pam,
            systemd,
            no_systemd,
            enroll,
            no_enroll,
            disable,
            service,
            remove,
            if_present,
            allow_sensitive,
            camera,
            models,
            execution_provider,
            encryption,
        } = cli;
        SetupArgs {
            non_interactive,
            yes: confirm.yes,
            pam,
            no_pam,
            systemd,
            no_systemd,
            enroll,
            no_enroll,
            disable,
            service,
            remove,
            if_present,
            allow_sensitive,
            camera,
            models,
            execution_provider,
            encryption,
        }
    }
}

/// The services a `facelock pam` verb acts on.
///
/// Repeatable, which is the point: the old `setup --pam --service X` took one
/// `Option<String>`, so configuring three services meant three processes, three
/// root checks and no atomicity across them. Empty means `sudo`, preserving
/// what bare `--pam` has always meant.
#[derive(Args)]
pub struct PamServiceArg {
    /// PAM service to act on; repeat for several (default: sudo)
    #[arg(long, num_args = 1, action = clap::ArgAction::Append, value_name = "SERVICE")]
    pub service: Vec<String>,
}

/// `facelock pam add | remove | status`.
///
/// Becomes a [`PamRequest`], the same way [`SetupCli`] becomes a [`SetupArgs`]:
/// clap types stay in the binary, and the library sees plain data it can also
/// construct in a test.
#[derive(Subcommand)]
pub enum PamCli {
    /// Add the facelock line to one or more /etc/pam.d service files
    #[command(after_help = "\
--yes/--no-confirm only skips the per-file confirmation. Editing one of the \
sensitive services common-auth, login, password-auth, password-auth-ac, sshd, \
system-auth, system-auth-ac or system-login additionally requires \
--allow-sensitive; neither flag implies the \
other. --json implies --no-confirm, since a prompt would block the pipeline \
reading the document. Every service is validated before any file is written, \
so a rejected service leaves the rest untouched. A service that exists only in \
a vendor directory (/usr/lib/pam.d, where polkit-1 ships on Arch) is copied to \
/etc/pam.d first and reported as 'overridden'; the package's own file is never \
modified.")]
    Add {
        #[command(flatten)]
        service: PamServiceArg,
        #[command(flatten)]
        confirm: ConfirmArg,
        /// Also permit the sensitive services: common-auth, login, password-auth, password-auth-ac, sshd, system-auth, system-auth-ac, system-login
        #[arg(long)]
        allow_sensitive: bool,
        /// Treat a missing service file as success instead of an error
        #[arg(long = "if-present")]
        if_present: bool,
        #[command(flatten)]
        dry_run: DryRunArg,
        #[command(flatten)]
        json: JsonArg,
    },
    /// Remove the facelock line from one or more /etc/pam.d service files
    #[command(after_help = "\
Removal is never gated by the sensitive-service list and never prompts — it can \
only take away a way to authenticate. Facelock-owned backups and legacy \
.facelock-backup files are cleaned by default; --keep-backup preserves them. \
--all ignores configured PAM directories, scans the compiled system roots, \
preflights and journals the complete recognized set, and rolls every earlier \
file back if a later replacement or final active-reference scan fails. \
--yes/--no-confirm is accepted for symmetry with `add`.")]
    Remove {
        #[command(flatten)]
        service: PamServiceArg,
        /// Remove every recognized Facelock-owned PAM edit under the system PAM roots
        #[arg(long, conflicts_with = "service")]
        all: bool,
        #[command(flatten)]
        confirm: ConfirmArg,
        /// Treat a missing service file as success instead of an error
        #[arg(long = "if-present")]
        if_present: bool,
        /// Preserve Facelock PAM backups instead of cleaning them up
        #[arg(long = "keep-backup")]
        keep_backup: bool,
        #[command(flatten)]
        dry_run: DryRunArg,
        #[command(flatten)]
        json: JsonArg,
    },
    /// Report whether services carry the facelock line (exit 0 = all do, 1 = one does not, 2 = error)
    #[command(after_help = "\
Reads only; needs no root. This is the probe to branch on instead of grepping \
/etc/pam.d yourself. --if-present has the same meaning it has on add and \
remove, so `pam add --if-present` for optional integrations can be verified \
with `pam status --if-present` rather than with exit 2. --all replaces the \
service list with every service in the resolved directories that carries the \
line, which is the question a bare `pam status` cannot answer: it reports only \
about names you give it, so a configured polkit-1 is invisible to it. --all \
exits 1 when nothing at all is configured and 2 when a directory could not be \
read, so 'not configured' and 'not checked' are never the same answer.")]
    Status {
        #[command(flatten)]
        service: PamServiceArg,
        /// Report every service in the resolved directories that carries the facelock line
        #[arg(long, conflicts_with = "service")]
        all: bool,
        /// Treat a missing service file as success instead of an error
        #[arg(long = "if-present")]
        if_present: bool,
        #[command(flatten)]
        json: JsonArg,
    },
}

impl From<PamCli> for PamRequest {
    fn from(cli: PamCli) -> Self {
        match cli {
            PamCli::Add {
                service,
                confirm,
                allow_sensitive,
                if_present,
                dry_run,
                json,
            } => PamRequest {
                action: PamAction::Add,
                services: service.service,
                all: false,
                // `--json` implies `--no-confirm` and never `--allow-sensitive`:
                // the prompt is on stderr while a parser waits on stdout, so
                // asking is a hang, but the gate is an authorization and a
                // machine caller has not given one.
                no_confirm: confirm.yes || json.json,
                allow_sensitive,
                if_present,
                dry_run: dry_run.dry_run,
                keep_backup: false,
                json: json.json,
            },
            PamCli::Remove {
                service,
                all,
                confirm,
                if_present,
                keep_backup,
                dry_run,
                json,
            } => PamRequest {
                action: PamAction::Remove,
                services: service.service,
                all,
                // Symmetry with `add`; `remove` has nothing to suppress today.
                no_confirm: confirm.yes || json.json,
                allow_sensitive: false,
                if_present,
                dry_run: dry_run.dry_run,
                keep_backup,
                json: json.json,
            },
            PamCli::Status {
                service,
                all,
                if_present,
                json,
            } => PamRequest {
                action: PamAction::Status,
                services: service.service,
                all,
                if_present,
                json: json.json,
                ..PamRequest::default()
            },
        }
    }
}
