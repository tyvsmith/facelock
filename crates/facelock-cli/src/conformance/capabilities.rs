//! `facelock capabilities` (#165): every emitted name is backed by a surface.

use clap::{CommandFactory, Parser};

use super::{arg, assert_long, sub};
use crate::{Cli, Commands};

/// Every emitted capability name is backed by the clap surface it names.
///
/// This is what makes the list a probe rather than documentation: a name
/// that nothing declares is a lie a consumer would act on. It lives in the
/// binary's test module because only the binary can call `Cli::command()`.
///
/// Each predicate proves the **surface** exists — the subcommand, the
/// argument, its long spelling. What a surface *means* is pinned by the
/// section of `docs/contracts.md` that owns it and by that command's own
/// tests; duplicating semantics here would only make both places drift.
///
/// `&str` patterns cannot be exhaustive, so the wildcard arm **is** the
/// exhaustiveness check: it must panic, so that adding a name to
/// `CAPABILITIES` without adding a predicate fails the build.
#[test]
fn capability_names_are_all_implemented() {
    use facelock_cli::commands::capabilities::CAPABILITIES;

    let root = Cli::command();
    let pam = sub(&root, "pam");
    let setup = sub(&root, "setup");
    let tpm = sub(&root, "tpm");

    for name in CAPABILITIES {
        match *name {
            "capabilities" => {
                sub(&root, "capabilities");
            }
            // ADR 009 renamed five invocations. A consumer cannot tell a
            // build that offers `daemon restart` from one that still wants
            // `restart` by any means except asking, so each new spelling
            // is its own name. One name, one promise: that the subcommand
            // at this path exists — not what it does, which the
            // docs/contracts.md row it is checked against owns.
            "config-edit" => {
                sub(sub(&root, "config"), "edit");
            }
            "daemon-restart" => {
                sub(sub(&root, "daemon"), "restart");
            }
            "tpm-decrypt" => {
                sub(tpm, "decrypt");
            }
            "tpm-encrypt" => {
                sub(tpm, "encrypt");
            }
            "tpm-reseal" => {
                sub(tpm, "reseal");
            }
            "devices-json" => assert_long(sub(&root, "devices"), "json", "json"),
            "is-enrolled" => {
                sub(&root, "is-enrolled");
            }
            "is-enrolled-json" => assert_long(sub(&root, "is-enrolled"), "json", "json"),
            "pam-allow-sensitive" => {
                assert_long(sub(pam, "add"), "allow_sensitive", "allow-sensitive");
                // The half that is not a spelling: `remove` must *not*
                // offer it, because removal is never gated.
                assert!(
                    !sub(pam, "remove")
                        .get_arguments()
                        .any(|a| a.get_id() == "allow_sensitive"),
                    "`pam remove` must not offer --allow-sensitive"
                );
            }
            "pam-dry-run" => {
                for verb in ["add", "remove"] {
                    assert_long(sub(pam, verb), "dry_run", "dry-run");
                }
            }
            "pam-if-present" => {
                for verb in ["add", "remove", "status"] {
                    assert_long(sub(pam, verb), "if_present", "if-present");
                }
            }
            "pam-json" => {
                for verb in ["add", "remove", "status"] {
                    assert_long(sub(pam, verb), "json", "json");
                }
            }
            "pam-multi-service" => {
                for verb in ["add", "remove", "status"] {
                    let verb_command = sub(pam, verb);
                    assert_long(verb_command, "service", "service");
                    // "multi" is the whole promise: a single `Option` here
                    // is what forced one process per service.
                    assert!(
                        matches!(
                            arg(verb_command, "service").get_action(),
                            clap::ArgAction::Append
                        ),
                        "`pam {verb} --service` must be repeatable"
                    );
                }
            }
            "pam-status" => {
                sub(pam, "status");
            }
            "pam-remove-all" => {
                assert_long(sub(pam, "remove"), "all", "all");
                assert!(
                    !sub(pam, "add").get_arguments().any(|a| a.get_id() == "all"),
                    "`pam add` must not offer machine-wide mutation"
                );
            }
            // The status enumerator is backed independently from destructive
            // cleanup so callers can branch on the exact operation they need.
            "pam-status-all" => {
                assert_long(sub(pam, "status"), "all", "all");
            }
            "quiet" => {
                assert_long(&root, "quiet", "quiet");
                assert!(
                    arg(&root, "quiet").is_global_set(),
                    "`--quiet` must be global — every command honours it"
                );
            }
            "setup-allow-sensitive" => assert_long(setup, "allow_sensitive", "allow-sensitive"),
            "setup-if-present" => assert_long(setup, "if_present", "if-present"),
            "setup-no-pam" => assert_long(setup, "no_pam", "no-pam"),
            "setup-systemd" => assert_long(setup, "systemd", "systemd"),
            "status-json" => assert_long(sub(&root, "status"), "json", "json"),
            other => {
                panic!("capability `{other}` has no predicate: a name nothing backs is a lie")
            }
        }
    }
}

/// Nothing legacy invokes this command, so these are their own table
/// rather than rows in `legacy_invocations_still_parse`.
#[test]
fn capabilities_invocations_parse() {
    for argv in [
        &["facelock", "capabilities"][..],
        &["facelock", "capabilities", "--json"],
        &["facelock", "--quiet", "capabilities", "--json"],
        &["facelock", "capabilities", "--quiet"],
    ] {
        let cli = Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("`{}` must parse: {e}", argv.join(" ")));
        assert!(
            matches!(cli.command, Commands::Capabilities { .. }),
            "`{}` must reach the Capabilities variant",
            argv.join(" ")
        );
        // The global flag on either side of the subcommand name reaches
        // the same field — the `CLI Flag Spelling` invariant.
        assert_eq!(
            cli.quiet,
            argv.contains(&"--quiet"),
            "`{}`: global --quiet must land on Cli::quiet wherever it sits",
            argv.join(" ")
        );
    }
}
