//! Flag spelling, the command set, and what argv must keep parsing.
//!
//! Two kinds of check share this file because they share a subject — the
//! surface a caller types. The registries (`TOP_LEVEL_COMMANDS`,
//! `SHORT_REGISTRY`, `JSON_COMMANDS`) pin what may exist; the `plan()` rows
//! and `legacy_invocations_still_parse` pin what a real command line resolves
//! to.

use clap::{CommandFactory, Parser};

use facelock_cli::commands;
use facelock_cli::commands::setup::{
    BaseMode, CameraChoice, EncryptionChoice, ExecutionProviderChoice, ModelPreset, PamPref,
    SetupArgs, SetupPlan, SystemdPref, resolve_setup_plan,
};

use super::walk;
use crate::{Cli, Commands};

/// Parse a `facelock setup ...` command line and resolve it, exercising the
/// exact path `main` takes. Pure: no root, no camera, no network.
fn plan(args: &[&str]) -> SetupPlan {
    let argv: Vec<&str> = ["facelock", "setup"].iter().chain(args).copied().collect();
    let cli = Cli::try_parse_from(argv).expect("expected these args to parse");
    let Commands::Setup(setup) = cli.command else {
        panic!("expected the Setup variant");
    };
    resolve_setup_plan(SetupArgs::from(setup))
}

fn parse_error(args: &[&str]) -> clap::Error {
    let argv: Vec<&str> = ["facelock", "setup"].iter().chain(args).copied().collect();
    Cli::try_parse_from(argv)
        .err()
        .expect("expected a parse error")
}

fn install(service: Option<&str>) -> PamPref {
    PamPref::Install {
        service: service.map(str::to_string),
        if_present: false,
    }
}

// -----------------------------------------------------------------------
// §2.4 compatibility matrix — one test per row.
// -----------------------------------------------------------------------

#[test]
fn matrix_row_bare_setup_is_the_full_wizard() {
    assert_eq!(plan(&[]), SetupPlan::default());
    // Spelled out, since `default()` is what every other row is diffed against.
    let p = plan(&[]);
    assert_eq!(p.base, Some(BaseMode::Wizard));
    assert_eq!(p.systemd, SystemdPref::Ask);
    assert_eq!(p.pam, PamPref::Ask);
    assert_eq!(p.enroll, None);
    assert_eq!(p.camera, None);
    assert_eq!(p.models, None);
    assert_eq!(p.execution_provider, None);
    assert_eq!(p.encryption, None);
    assert!(!p.yes);
    assert!(!p.allow_sensitive);
}

#[test]
fn matrix_row_non_interactive_skips_every_action() {
    let p = plan(&["--non-interactive"]);
    assert_eq!(p.base, Some(BaseMode::NonInteractive));
    // Ask under a non-interactive base means "do nothing", as today.
    assert_eq!(p.systemd, SystemdPref::Ask);
    assert_eq!(p.pam, PamPref::Ask);
    assert_eq!(p.enroll, None);
}

#[test]
fn matrix_row_pam_with_service_is_standalone() {
    let p = plan(&["--pam", "--service", "sudo"]);
    assert_eq!(p.base, None);
    assert_eq!(p.pam, install(Some("sudo")));
    assert_eq!(p.systemd, SystemdPref::Ask);
}

/// Standalone `--pam` / `--systemd` must NOT go through the interactive
/// root pre-check: it prompts and re-execs under sudo on a TTY, which those
/// invocations never did. They bail from their own root checks instead.
/// A base setup, which always did prompt, still must.
#[test]
fn only_a_base_setup_takes_the_interactive_root_precheck() {
    for args in [
        vec!["--pam"],
        vec!["--pam", "--service", "sudo"],
        vec!["--pam", "--remove"],
        vec!["--systemd"],
        vec!["--systemd", "--disable"],
        vec!["--systemd", "--pam"],
    ] {
        let p = plan(&args);
        assert_eq!(p.base, None, "{args:?} must stay standalone");
        assert!(
            !commands::setup::needs_root_precheck(&p),
            "{args:?} must not trigger the sudo re-exec prompt"
        );
    }

    for args in [
        vec![],
        vec!["--non-interactive"],
        vec!["--no-pam"],
        vec!["--non-interactive", "--pam"],
        vec!["--camera", "auto"],
    ] {
        let p = plan(&args);
        assert!(
            commands::setup::needs_root_precheck(&p),
            "{args:?} runs a base setup and must keep the root pre-check"
        );
    }
}

#[test]
fn matrix_row_systemd_is_standalone() {
    let p = plan(&["--systemd"]);
    assert_eq!(p.base, None);
    assert_eq!(p.systemd, SystemdPref::Install);
    assert_eq!(p.pam, PamPref::Ask);
}

#[test]
fn matrix_row_systemd_and_pam_runs_both() {
    // Today `--pam` is silently dropped here.
    let p = plan(&["--systemd", "--pam"]);
    assert_eq!(p.base, None);
    assert_eq!(p.systemd, SystemdPref::Install);
    assert_eq!(p.pam, install(None));
}

#[test]
fn matrix_row_non_interactive_and_pam_runs_base_and_pam() {
    // Today `--non-interactive` is silently dropped here.
    let p = plan(&["--non-interactive", "--pam"]);
    assert_eq!(p.base, Some(BaseMode::NonInteractive));
    assert_eq!(p.pam, install(None));
}

#[test]
fn matrix_row_no_pam_suppresses_step_nine() {
    let p = plan(&["--no-pam"]);
    assert_eq!(p.base, Some(BaseMode::Wizard));
    assert_eq!(p.pam, PamPref::Skip);
}

#[test]
fn matrix_row_yes_with_execution_provider() {
    let p = plan(&["-y", "--execution-provider=cuda"]);
    assert_eq!(p.base, Some(BaseMode::Wizard));
    assert!(p.yes);
    assert_eq!(p.execution_provider, Some(ExecutionProviderChoice::Cuda));
}

// -----------------------------------------------------------------------
// Action modifiers
// -----------------------------------------------------------------------

#[test]
fn pam_remove_with_explicit_service() {
    let p = plan(&["--pam", "--remove", "--service", "sudo"]);
    assert_eq!(p.base, None);
    assert_eq!(
        p.pam,
        PamPref::Remove {
            service: "sudo".to_string(),
            if_present: false,
        }
    );
}

#[test]
fn pam_remove_defaults_to_sudo() {
    // Removal needs a concrete service, so the default is applied eagerly.
    assert_eq!(
        plan(&["--pam", "--remove"]).pam,
        PamPref::Remove {
            service: "sudo".to_string(),
            if_present: false,
        }
    );
}

#[test]
fn pam_remove_if_present_reaches_the_resolved_plan() {
    assert_eq!(
        plan(&[
            "--pam",
            "--service",
            "omarchy-lock-face",
            "--remove",
            "--if-present",
        ])
        .pam,
        PamPref::Remove {
            service: "omarchy-lock-face".to_string(),
            if_present: true,
        }
    );
}

/// The add side of the same flag.
///
/// `setup --pam --if-present` used to be a parse error, and the alias handed
/// the writer a hard-coded `false` even so — two independent places for the
/// bool to stop, which is why the row is here as well as in
/// `commands::setup`'s step-9 tests. What a caller wants out of it is one
/// thing: configure this set of optional integrations, skipping the services
/// this machine does not have.
#[test]
fn pam_add_if_present_reaches_the_resolved_plan() {
    assert_eq!(
        plan(&["--pam", "--service", "omarchy-lock-face", "--if-present"]).pam,
        PamPref::Install {
            service: Some("omarchy-lock-face".to_string()),
            if_present: true,
        }
    );
    // Bare `--pam --if-present` too: the service default is applied later, and
    // the flag must survive that.
    assert_eq!(
        plan(&["--pam", "--if-present"]).pam,
        PamPref::Install {
            service: None,
            if_present: true,
        }
    );
}

#[test]
fn systemd_disable_is_standalone() {
    let p = plan(&["--systemd", "--disable"]);
    assert_eq!(p.base, None);
    assert_eq!(p.systemd, SystemdPref::Disable);
}

// -----------------------------------------------------------------------
// `overrides_with`: the later flag wins for every action pair.
// -----------------------------------------------------------------------

#[test]
fn later_pam_flag_wins() {
    assert_eq!(plan(&["--pam", "--no-pam"]).pam, PamPref::Skip);
    assert_eq!(plan(&["--no-pam", "--pam"]).pam, install(None));
}

#[test]
fn later_systemd_flag_wins() {
    assert_eq!(
        plan(&["--systemd", "--no-systemd"]).systemd,
        SystemdPref::Skip
    );
    assert_eq!(
        plan(&["--no-systemd", "--systemd"]).systemd,
        SystemdPref::Install
    );
}

#[test]
fn later_enroll_flag_wins() {
    assert_eq!(plan(&["--enroll", "--no-enroll"]).enroll, Some(false));
    assert_eq!(plan(&["--no-enroll", "--enroll"]).enroll, Some(true));
}

// -----------------------------------------------------------------------
// Choice flags
// -----------------------------------------------------------------------

#[test]
fn choice_flag_forces_the_base_setup_to_run() {
    // The regression guard: `--camera` must not be silently dropped in
    // favour of PAM-only mode the way the old `else if` chain did.
    let p = plan(&["--camera=/dev/video2", "--pam"]);
    assert_eq!(p.base, Some(BaseMode::Wizard));
    assert_eq!(p.pam, install(None));
    assert_eq!(
        p.camera,
        Some(CameraChoice::Path("/dev/video2".to_string()))
    );
}

#[test]
fn camera_auto_is_distinct_from_a_path() {
    assert_eq!(plan(&["--camera", "auto"]).camera, Some(CameraChoice::Auto));
    assert_eq!(
        plan(&["--camera", "/dev/video2"]).camera,
        Some(CameraChoice::Path("/dev/video2".to_string()))
    );
}

#[test]
fn models_preset_parses() {
    assert_eq!(plan(&["--models", "high"]).models, Some(ModelPreset::High));
    assert_eq!(
        plan(&["--models", "balanced"]).models,
        Some(ModelPreset::Balanced)
    );
    assert_eq!(
        plan(&["--models", "standard"]).models,
        Some(ModelPreset::Standard)
    );
}

#[test]
fn encryption_choice_parses() {
    assert_eq!(
        plan(&["--encryption", "tpm"]).encryption,
        Some(EncryptionChoice::Tpm)
    );
    assert_eq!(
        plan(&["--encryption", "auto"]).encryption,
        Some(EncryptionChoice::Auto)
    );
    assert_eq!(
        plan(&["--encryption", "none"]).encryption,
        Some(EncryptionChoice::None)
    );
}

// -----------------------------------------------------------------------
// Action modifiers require their action. Today these are silently dropped.
// -----------------------------------------------------------------------

#[test]
fn remove_without_pam_is_a_parse_error() {
    assert_eq!(
        parse_error(&["--remove"]).kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
}

/// `--if-present` requires `--pam`, and nothing else.
///
/// Was `if_present_requires_remove_and_pam`, back when the flag was gated to
/// removal. The two cases that must keep failing are the same two it held
/// then, kept here rather than dropped: `--if-present` on its own is still a
/// parse error naming the action it modifies, and `--remove --if-present`
/// still fails on `--remove`'s own `requires = "pam"`. Only the middle case
/// moved, from a parse error to a resolved plan.
#[test]
fn if_present_requires_pam() {
    for args in [&["--if-present"][..], &["--remove", "--if-present"]] {
        assert_eq!(
            parse_error(args).kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "unexpected error kind for {args:?}"
        );
    }
    // ...and the case that used to be here as a third failure.
    assert_eq!(
        plan(&["--pam", "--if-present"]).pam,
        PamPref::Install {
            service: None,
            if_present: true,
        }
    );
}

#[test]
fn setup_help_documents_if_present() {
    let mut command = Cli::command();
    let help = command
        .find_subcommand_mut("setup")
        .expect("setup subcommand")
        .render_long_help()
        .to_string();

    assert!(help.contains("--if-present"));
    assert!(help.contains("treat an absent service file as success"));
}

#[test]
fn service_without_pam_is_a_parse_error() {
    assert_eq!(
        parse_error(&["--service", "sudo"]).kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
}

#[test]
fn disable_without_systemd_is_a_parse_error() {
    assert_eq!(
        parse_error(&["--disable"]).kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
}

// -----------------------------------------------------------------------
// is-enrolled
// -----------------------------------------------------------------------

/// Clap derives the subcommand name from the variant, so `IsEnrolled` must
/// spell itself `is-enrolled` on the command line.
#[test]
fn is_enrolled_flags_parse() {
    let cli = Cli::try_parse_from([
        "facelock",
        "is-enrolled",
        "--user",
        "alice",
        "--json",
        "--quiet",
    ])
    .expect("is-enrolled args should parse");
    // `--quiet` is global now, so it is read off `Cli`, not the variant.
    assert!(cli.quiet);
    let Commands::IsEnrolled { user, json } = cli.command else {
        panic!("expected the IsEnrolled variant");
    };
    assert_eq!(user.user.as_deref(), Some("alice"));
    assert!(json.json);
}

// -----------------------------------------------------------------------
// Flag spelling (#167)
// -----------------------------------------------------------------------

/// The top-level command set, in `--help` order.
///
/// A top-level name is the spelling every wrapper script, unit file and
/// lock screen hard-codes, so gaining or losing one is a decision, not a
/// side effect of adding a variant. The rule that decides whether a new
/// command belongs here or inside a noun group is written down in
/// `docs/contracts.md` §CLI Subcommands; ADR 009 adopted it.
///
/// Checked in both directions against `Cli::command()`, like
/// `JSON_COMMANDS`: a name here that the binary does not offer fails, and
/// a command the binary offers that is not here fails too. Nested verbs
/// (`daemon restart`, `tpm encrypt`, `pam status`) are deliberately absent
/// — they are their group's business, and moving one is not a change to
/// this surface.
const TOP_LEVEL_COMMANDS: &[&str] = &[
    "setup",
    "is-enrolled",
    "capabilities",
    "enroll",
    "remove",
    "clear",
    "list",
    "test",
    "preview",
    "config",
    "status",
    "devices",
    "daemon",
    "auth",
    "bench",
    "tpm",
    "pam",
    "hyprlock",
    "audit",
];

#[test]
fn top_level_commands_match_the_registry() {
    let root = Cli::command();
    let actual: Vec<&str> = root
        .get_subcommands()
        .map(|c| c.get_name())
        .filter(|name| *name != "help")
        .collect();

    for name in TOP_LEVEL_COMMANDS {
        assert!(
            actual.contains(name),
            "`facelock {name}` is in TOP_LEVEL_COMMANDS but the binary does not offer it"
        );
    }
    for name in &actual {
        assert!(
            TOP_LEVEL_COMMANDS.contains(name),
            "`facelock {name}` is a top-level command with no row in \
             TOP_LEVEL_COMMANDS; add one only if it names a user task that \
             fits no existing noun group (docs/contracts.md §CLI Subcommands)"
        );
    }
}

/// `-v` on every side of every command, and what it counts to.
///
/// Its own table rather than rows in `legacy_invocations_still_parse`: that
/// one pins what parsed *before* the flag existed, and this flag is new.
///
/// The row a reader would expect to be a contradiction is the one that must
/// not be: `--quiet -v` is a legitimate pair. They are two knobs on two
/// streams — `--quiet` silences the stdout report, `-v` raises the stderr
/// diagnostics — so neither cancels the other (docs/contracts.md, "CLI Output
/// Streams").
#[test]
fn verbose_invocations_parse_and_count_repeats() {
    for (argv, expected) in [
        (&["facelock", "status"][..], 0u8),
        (&["facelock", "-v", "status"], 1),
        (&["facelock", "status", "-v"], 1),
        (&["facelock", "--verbose", "status"], 1),
        (&["facelock", "-vv", "status"], 2),
        (&["facelock", "status", "-vvv"], 3),
        (&["facelock", "--verbose", "--verbose", "status"], 2),
        // The three init sites, each reached with the flag: the shared CLI
        // path, the one-shot PAM helper, and the daemon.
        (&["facelock", "-v", "setup", "--non-interactive"], 1),
        (&["facelock", "auth", "--user", "alice", "-vv"], 2),
        (&["facelock", "daemon", "run", "-v"], 1),
    ] {
        let cli = Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("`{}` must parse: {e}", argv.join(" ")));
        assert_eq!(
            cli.verbose,
            expected,
            "`{}`: -v must count its repeats",
            argv.join(" ")
        );
    }

    let cli = Cli::try_parse_from(["facelock", "--quiet", "-v", "list", "--json"])
        .expect("`--quiet -v` must parse: quiet stdout and loud stderr are not a contradiction");
    assert!(cli.quiet);
    assert_eq!(cli.verbose, 1);
}

/// The short-letter registry.
///
/// Short letters are a single namespace shared by every subcommand: once
/// `-l` means `--label` on one command, spending it on something else
/// elsewhere makes both a trap. Each row is a letter and the long names
/// allowed to bind it. `cli_flag_conformance` fails on a letter that is not
/// listed, and on a listed letter bound to a name outside its row, so
/// widening the namespace is a deliberate edit here rather than a side
/// effect of adding a flag.
///
/// `l` maps to two names because `enroll --label` and `audit --lines` both
/// ship today and both must keep working. `v` was held here as a reservation
/// before `--verbose` existed, so that the letter could not be spent on
/// something else first; the global flag now claims it.
const SHORT_REGISTRY: &[(char, &[&str])] = &[
    ('u', &["user"]),
    ('y', &["yes"]),
    ('c', &["config"]),
    ('q', &["quiet"]),
    ('l', &["label", "lines"]),
    ('f', &["follow"]),
    ('v', &["verbose"]),
];

/// Every command that offers `--json`, by full invocation path.
///
/// The list is the contract, in both directions: a command here without a
/// `json` arg fails, and a `json` arg on a command not here fails too. So
/// `--json` cannot appear because a matrix looked incomplete — adding a
/// row is the moment someone states who parses the output, which is the
/// rule `docs/contracts.md` records under "CLI Machine Output".
///
/// `preview` is on the list because it always emitted JSON; it just spelled
/// the flag `--text-only`, which survives as a hidden alias.
const JSON_COMMANDS: &[&str] = &[
    "facelock is-enrolled",
    "facelock capabilities",
    "facelock list",
    "facelock devices",
    "facelock preview",
    "facelock status",
    "facelock pam add",
    "facelock pam remove",
    "facelock pam status",
];

/// Pins flag spelling across the whole command tree.
///
/// This is the test that outlives the refactor that produced it. The shared
/// arg structs in `args.rs` stop drift at the sites that use them; this
/// stops it at the sites that do not — a hand-rolled `--user` on a new
/// command, a second `-c`, a `--json` that grew a short letter.
///
/// **Recorded deviation from the G1 plan.** The plan called for a single
/// `UserArg` (an `Option<String>`) on every user-scoped command including
/// `auth`. `auth --user` is required today and `pam_facelock.so` spawns
/// `facelock auth --user <name>`; making it optional would let the subject
/// default to the process owner, which is an auth-semantics change, not a
/// spelling one. So `auth` keeps a required `String` and only gains `-u`,
/// and the requiredness rule below is asserted per command rather than
/// assumed uniform.
#[test]
fn cli_flag_conformance() {
    let root = Cli::command();
    let mut commands = Vec::new();
    walk(&root, "", &mut commands);

    for (path, command) in &commands {
        let about = command
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(
            !about.trim().is_empty(),
            "`{path}` has no about text, so it renders blank in `--help`"
        );

        for arg in command.get_arguments() {
            let id = arg.get_id().as_str();
            // clap owns `-h`/`-V`; the registry governs our own flags only.
            if id == "help" || id == "version" {
                continue;
            }
            let long = arg.get_long();

            match id {
                "user" => {
                    assert_eq!(
                        arg.get_short(),
                        Some('u'),
                        "`{path} --user` must also spell `-u`"
                    );
                    assert_eq!(long, Some("user"), "`{path}`: user arg spelled oddly");
                    if path == "facelock auth" {
                        assert!(
                            arg.is_required_set(),
                            "`auth --user` must stay required — PAM names the subject"
                        );
                    } else {
                        assert!(
                            !arg.is_required_set(),
                            "`{path} --user` must stay optional (defaults to current user)"
                        );
                    }
                }
                "yes" => {
                    assert_eq!(arg.get_short(), Some('y'), "`{path} --yes` must spell `-y`");
                    assert_eq!(long, Some("yes"));
                    assert!(
                        arg.get_all_aliases()
                            .unwrap_or_default()
                            .contains(&"no-confirm"),
                        "`{path} --yes` must accept the historical `--no-confirm`"
                    );
                }
                // `--all` widens what a command reports rather than how it is
                // spelled, and it is the sort of flag a later command would
                // reach for too, so the letter it must not spend is pinned
                // here beside `--json`'s rather than left to SHORT_REGISTRY's
                // default refusal.
                "json" | "dry_run" | "all" => {
                    assert_eq!(
                        arg.get_short(),
                        None,
                        "`{path} --{id}` must not claim a short letter"
                    );
                    // The long spelling is the whole contract for these,
                    // so assert it too: without this a `--json-output`
                    // would satisfy the short-letter rule vacuously.
                    let expected = id.replace('_', "-");
                    assert_eq!(
                        long,
                        Some(expected.as_str()),
                        "`{path}`: the `{id}` arg must spell `--{expected}`"
                    );
                }
                "quiet" => {
                    assert_eq!(
                        arg.get_short(),
                        Some('q'),
                        "`{path} --quiet` must spell `-q`"
                    );
                    assert_eq!(long, Some("quiet"), "`{path}`: quiet arg spelled oddly");
                }
                "verbose" => {
                    assert_eq!(
                        arg.get_short(),
                        Some('v'),
                        "`{path} --verbose` must spell `-v`"
                    );
                    assert_eq!(long, Some("verbose"), "`{path}`: verbose arg spelled oddly");
                    // Repeatable or the rungs above info are unreachable: a
                    // `SetTrue` here would leave `-vv` a parse error and
                    // debug/trace with no spelling at all.
                    assert!(
                        matches!(arg.get_action(), clap::ArgAction::Count),
                        "`{path} --verbose` must count repeats"
                    );
                }
                _ => {}
            }

            let mut shorts: Vec<char> = arg.get_short().into_iter().collect();
            shorts.extend(arg.get_all_short_aliases().unwrap_or_default());
            for short in shorts {
                let Some((_, allowed)) = SHORT_REGISTRY.iter().find(|(letter, _)| *letter == short)
                else {
                    panic!(
                        "`{path}` binds -{short} (--{}), which is not in SHORT_REGISTRY; \
                         add a row there if the letter is really meant to be spent",
                        long.unwrap_or(id)
                    );
                };
                let name = long.unwrap_or(id);
                assert!(
                    allowed.contains(&name),
                    "`{path}` binds -{short} to --{name}; the registry reserves \
                     that letter for {allowed:?}"
                );
            }
        }
    }

    // -- `--json` coverage (#169) ------------------------------------
    //
    // Spelling is pinned above; these pin *where* the flag appears, and
    // that nothing else spells machine output a second way.

    let offers_json = |path: &str| {
        commands
            .iter()
            .find(|(p, _)| p == path)
            .unwrap_or_else(|| panic!("`{path}` is in JSON_COMMANDS but not in the tree"))
            .1
            .get_arguments()
            .any(|arg| arg.get_id() == "json")
    };
    for path in JSON_COMMANDS {
        assert!(
            offers_json(path),
            "`{path}` is in JSON_COMMANDS but binds no `--json`"
        );
    }
    for (path, command) in &commands {
        if command.get_arguments().any(|arg| arg.get_id() == "json") {
            assert!(
                JSON_COMMANDS.contains(&path.as_str()),
                "`{path}` offers `--json` without a row in JSON_COMMANDS; add one \
                 naming the consumer that asked for it"
            );
        }
    }

    // A flag that advertises JSON is named `json`. Without this, a later
    // `--output json` or a second `--text-only` would satisfy every rule
    // above by never being called `json` in the first place. Clap aliases
    // are not `Arg`s and carry no help, so a hidden historical spelling is
    // exempt by construction.
    //
    // Everything a user could read on the flag counts, concatenated
    // rather than one-or-the-other: short help, long help, and the
    // possible values with their own help. A `--format <FORMAT>` over
    // `{json, text}` documented as "Output format" says JSON nowhere in
    // its help text and only the value list gives it away.
    //
    // This is deliberately over-broad. A future *input* flag such as
    // `--from-json <path>` would trip it despite emitting nothing. Rename
    // that arg rather than relaxing the rule: the tripwire is worth more
    // than the one name it costs.
    for (path, command) in &commands {
        for arg in command.get_arguments() {
            let mut advertised = String::new();
            for text in [arg.get_help(), arg.get_long_help()].into_iter().flatten() {
                advertised.push_str(&text.to_string());
                advertised.push(' ');
            }
            for value in arg.get_possible_values() {
                advertised.push_str(value.get_name());
                advertised.push(' ');
                if let Some(text) = value.get_help() {
                    advertised.push_str(&text.to_string());
                    advertised.push(' ');
                }
            }
            if advertised.to_lowercase().contains("json") {
                assert_eq!(
                    arg.get_id().as_str(),
                    "json",
                    "`{path} --{}` advertises JSON but is not the shared \
                     `json` arg; machine output has one spelling",
                    arg.get_long().unwrap_or_else(|| arg.get_id().as_str())
                );
            }
        }
    }
}

/// Every invocation that parsed before the shared arg structs landed must
/// still parse. The refactor is additive or it is a regression, and only a
/// table of real argv can tell those apart.
#[test]
fn legacy_invocations_still_parse() {
    for argv in [
        &["facelock", "setup", "--pam", "--service", "sudo", "--yes"][..],
        &[
            "facelock",
            "setup",
            "--pam",
            "--service",
            "sudo",
            "--remove",
            "--yes",
        ],
        &[
            "facelock",
            "setup",
            "--pam",
            "--service",
            "sudo",
            "--remove",
            "--yes",
            "--if-present",
        ],
        &["facelock", "setup", "--no-pam", "--systemd", "--enroll"],
        &["facelock", "setup", "--pam", "--no-confirm"],
        &["facelock", "preview", "--text-only"],
        &["facelock", "preview", "--json"],
        &["facelock", "remove", "1", "-y"],
        &["facelock", "clear", "-u", "alice", "--yes"],
        &["facelock", "enroll", "-u", "alice", "-l", "laptop"],
        &["facelock", "audit", "-f", "-l", "5"],
        &["facelock", "list", "-u", "alice", "--json"],
        &["facelock", "devices", "--json"],
        // `facelock pam` (#174). The new verb is additive: every row above
        // is what `setup --pam` accepted before it existed, and both
        // spellings keep working.
        &["facelock", "pam", "add"],
        &["facelock", "pam", "add", "--service", "sudo"],
        &[
            "facelock",
            "pam",
            "add",
            "--service",
            "sudo",
            "--service",
            "polkit-1",
            "--no-confirm",
            "--dry-run",
            "--json",
        ],
        &[
            "facelock",
            "pam",
            "add",
            "--service",
            "system-auth",
            "--allow-sensitive",
            "-y",
        ],
        &["facelock", "pam", "add", "--service", "x", "--if-present"],
        &["facelock", "pam", "remove"],
        &["facelock", "pam", "remove", "--all"],
        &[
            "facelock",
            "pam",
            "remove",
            "--service",
            "sudo",
            "--if-present",
            "--no-confirm",
        ],
        &["facelock", "pam", "status"],
        &["facelock", "pam", "status", "--service", "sudo", "--json"],
        &["facelock", "--quiet", "pam", "status", "--json"],
        // P3's `--all`. Additive in the same way: a bare `pam status` still
        // means `sudo`, so no invocation above changes meaning.
        &["facelock", "pam", "status", "--all"],
        &["facelock", "pam", "status", "--all", "--json"],
        &["facelock", "pam", "status", "--all", "--if-present"],
        &["facelock", "--quiet", "pam", "status", "--all", "--json"],
        // P6's `status --json`. `status` took no flag of its own before it,
        // so the bare form is the one that must not have changed meaning.
        &["facelock", "status"],
        &["facelock", "status", "--json"],
        &["facelock", "--quiet", "status", "--json"],
        // ADR 009. The bare forms are what five init-system units and
        // `commands::setup::run_systemd`'s `ExecStart` marker invoke, and
        // what a reader types; the explicit forms are the new spellings.
        // The four deleted top-level spellings get no rows here: this
        // table pins what must keep parsing, and ADR 009 decided they
        // must not. They are pinned as parse errors further down.
        &["facelock", "daemon"],
        &["facelock", "daemon", "run"],
        &["facelock", "daemon", "restart"],
        &["facelock", "config"],
        &["facelock", "config", "show"],
        &["facelock", "config", "edit"],
        &["facelock", "tpm", "encrypt"],
        &["facelock", "tpm", "encrypt", "--generate-key"],
        &["facelock", "tpm", "decrypt"],
        &["facelock", "tpm", "reseal"],
    ] {
        Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("`{}` must still parse: {e}", argv.join(" ")));
    }

    // The PAM module spawns exactly this argv (crates/pam-facelock/src/lib.rs).
    // `auth` no longer declares its own `--config`; this parses only because
    // the one on `Cli` is `global = true` and is therefore still accepted
    // after the subcommand name. Drop `global` and PAM breaks silently.
    let cli = Cli::try_parse_from([
        "facelock",
        "auth",
        "--user",
        "alice",
        "--config",
        "/etc/facelock/config.toml",
    ])
    .expect("the argv pam_facelock.so spawns must parse");
    assert_eq!(cli.config.as_deref(), Some("/etc/facelock/config.toml"));
    let Commands::Auth { user } = cli.command else {
        panic!("expected the Auth variant");
    };
    assert_eq!(user, "alice");

    // `daemon -c X` kept its spelling when the per-command flag was deleted:
    // the global one gained `-c`. Both sides of the subcommand work, and
    // the bare form still reaches the daemon under `Option<DaemonCommand>`
    // (ADR 009): were `None` ever to mean `Restart`, every service unit
    // would restart the daemon at boot instead of starting it.
    for argv in [
        &["facelock", "daemon", "-c", "/tmp/x.toml"][..],
        &["facelock", "-c", "/tmp/x.toml", "daemon"],
    ] {
        let cli = Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("`{}` must parse: {e}", argv.join(" ")));
        assert_eq!(cli.config.as_deref(), Some("/tmp/x.toml"));
        assert!(matches!(cli.command, Commands::Daemon { command: None }));
    }

    // The `None` arms are the compatibility surface: bare `daemon` runs
    // the daemon and bare `config` shows the file. A future `Option` that
    // defaulted to anything else would be a silent behaviour change.
    assert!(matches!(
        Cli::try_parse_from(["facelock", "daemon"])
            .expect("bare `facelock daemon` must parse")
            .command,
        Commands::Daemon { command: None }
    ));
    assert!(matches!(
        Cli::try_parse_from(["facelock", "config"])
            .expect("bare `facelock config` must parse")
            .command,
        Commands::Config { command: None }
    ));

    // Deleted, not hidden (ADR 009): the old spellings must not parse.
    for argv in [
        &["facelock", "restart"][..],
        &["facelock", "encrypt"],
        &["facelock", "encrypt", "--generate-key"],
        &["facelock", "decrypt"],
        &["facelock", "reseal"],
        &["facelock", "config", "--edit"],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "`{}` was renamed and must no longer parse",
            argv.join(" ")
        );
    }

    // `--quiet` moved off `is-enrolled` onto the root, so both positions
    // must reach the same field.
    for argv in [
        &["facelock", "is-enrolled", "--quiet"][..],
        &["facelock", "--quiet", "is-enrolled"],
        &["facelock", "is-enrolled", "-q"],
    ] {
        let cli = Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("`{}` must parse: {e}", argv.join(" ")));
        assert!(cli.quiet, "`{}` must set the global quiet", argv.join(" "));
    }

    // `--no-confirm` was `setup`-only; it is the alias everywhere now.
    let cli = Cli::try_parse_from(["facelock", "clear", "--no-confirm"])
        .expect("`clear --no-confirm` must parse");
    let Commands::Clear { confirm, .. } = cli.command else {
        panic!("expected the Clear variant");
    };
    assert!(confirm.yes);
}

// -----------------------------------------------------------------------
// `facelock pam` (#174)
// -----------------------------------------------------------------------

fn pam_request(args: &[&str]) -> facelock_cli::commands::pam::PamRequest {
    let argv: Vec<&str> = ["facelock", "pam"].iter().chain(args).copied().collect();
    let cli = Cli::try_parse_from(argv).expect("expected these args to parse");
    let Commands::Pam { command } = cli.command else {
        panic!("expected the Pam variant");
    };
    command.into()
}

/// The defect the verb exists to fix: one process, several services. The
/// old surface took a single `Option<String>`, so a wrapper wanting three
/// services ran three of them.
#[test]
fn pam_service_is_repeatable_and_ordered() {
    assert_eq!(
        pam_request(&["add", "--service", "sudo", "--service", "polkit-1"]).services,
        ["sudo", "polkit-1"]
    );
    // Empty is not "no services" — it is `sudo`, resolved in the command.
    assert!(pam_request(&["add"]).services.is_empty());
}

/// **`--no-confirm` must never imply `--allow-sensitive`.** They are
/// separate authorizations: "do not ask me" and "yes, edit system-auth".
/// The primary PAM verb and the setup alias both enforce that separation.
#[test]
fn no_confirm_and_allow_sensitive_are_independent() {
    for skip_prompts in [
        &["add", "--no-confirm"][..],
        &["add", "--yes"],
        &["add", "-y"],
    ] {
        let request = pam_request(skip_prompts);
        assert!(request.no_confirm, "{skip_prompts:?}");
        assert!(
            !request.allow_sensitive,
            "{skip_prompts:?} must not unlock the sensitive services"
        );
    }

    let request = pam_request(&["add", "--allow-sensitive"]);
    assert!(request.allow_sensitive);
    assert!(
        !request.no_confirm,
        "--allow-sensitive accepts a risk; it does not skip the question"
    );

    // The setup alias must keep the same separation.
    let setup = plan(&["--pam", "--yes"]);
    assert!(setup.yes);
    assert!(!setup.allow_sensitive);
}

/// `setup --pam` exposes the same explicit sensitive authorization as the
/// primary `pam add` surface, and the modifier is meaningless without PAM.
#[test]
fn setup_accepts_explicit_sensitive_authorization_only_with_pam() {
    let authorized = [
        "facelock",
        "setup",
        "--pam",
        "--service",
        "system-auth",
        "--yes",
        "--allow-sensitive",
    ];
    assert!(
        Cli::try_parse_from(authorized).is_ok(),
        "`{}` must parse",
        authorized.join(" ")
    );
    assert!(
        Cli::try_parse_from(["facelock", "setup", "--allow-sensitive"]).is_err(),
        "--allow-sensitive without --pam must be rejected"
    );
    assert!(
        Cli::try_parse_from([
            "facelock",
            "setup",
            "--pam",
            "--remove",
            "--allow-sensitive",
        ])
        .is_err(),
        "removal is never sensitive-gated, so it must not accept a meaningless authorization"
    );

    let resolved = plan(&["--pam", "--allow-sensitive"]);
    assert!(resolved.allow_sensitive);
    assert!(
        !resolved.yes,
        "sensitive authorization must not suppress the prompt"
    );
}

/// `--allow-sensitive` is an `add`-only flag: removal can only take away a
/// way to authenticate, so there is nothing to gate.
#[test]
fn remove_and_status_do_not_offer_allow_sensitive() {
    for argv in [
        &["facelock", "pam", "remove", "--allow-sensitive"][..],
        &["facelock", "pam", "status", "--allow-sensitive"],
        &["facelock", "pam", "status", "--dry-run"],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "`{}` must not parse",
            argv.join(" ")
        );
    }
}

#[test]
fn pam_remove_keep_backup_is_an_explicit_opt_out() {
    let default = pam_request(&["remove"]);
    assert!(!default.keep_backup);

    let kept = pam_request(&["remove", "--keep-backup"]);
    assert!(kept.keep_backup);

    for argv in [
        &["facelock", "pam", "add", "--keep-backup"][..],
        &["facelock", "pam", "status", "--keep-backup"],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "--keep-backup is remove-only: {argv:?}"
        );
    }
}

/// `--all` is an enumerating flag for status and removal, takes no
/// `--service`, and leaves both bare forms alone.
///
/// The last clause is the compatibility one: `pam status` with no flags has
/// meant `sudo` since the verb shipped, and its exit code is 0/1/2 about that
/// one service. Enumerating instead would have changed an integrator's answer
/// without changing their command line, which is why `--all` is a flag rather
/// than a new default (docs/contracts.md, "facelock pam Semantics").
#[test]
fn all_enumerates_status_or_removal_and_leaves_bare_forms_alone() {
    assert!(pam_request(&["status", "--all"]).all);
    assert!(pam_request(&["status", "--all", "--json"]).json);
    let removal = pam_request(&["remove", "--all"]);
    assert!(removal.all);
    assert_eq!(
        removal.action,
        facelock_cli::commands::pam::PamAction::Remove
    );

    let bare = pam_request(&["status"]);
    assert!(!bare.all, "bare `pam status` must still mean one service");
    assert!(
        bare.services.is_empty(),
        "...resolved to sudo in the command"
    );
    let bare_remove = pam_request(&["remove"]);
    assert!(!bare_remove.all, "bare `pam remove` must still mean sudo");
    assert!(bare_remove.services.is_empty());

    // `--if-present` composes: the pair is documented as answering
    // "everything configured, and absence is not an error".
    assert!(pam_request(&["status", "--all", "--if-present"]).if_present);

    for argv in [
        // Enumerating and naming are two questions; a request that did both
        // would have to drop one silently.
        &["facelock", "pam", "status", "--all", "--service", "sudo"][..],
        &["facelock", "pam", "remove", "--all", "--service", "sudo"],
        // Addition is deliberately named: `add --all` would edit every PAM
        // service file on the machine rather than clean Facelock-owned work.
        &["facelock", "pam", "add", "--all"],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "`{}` must not parse",
            argv.join(" ")
        );
    }
}
