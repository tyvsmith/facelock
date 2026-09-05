//! Reference-doc coverage (#172): the docs describe the binary that shipped.
//!
//! Reference text is embedded; executable examples come from the shared
//! classified inventory, which also reaches instructional pages outside it.

use clap::{CommandFactory, Parser};

use facelock_cli::commands::setup::{PamPref, SetupArgs, resolve_setup_plan};

use super::walk;
use crate::{Cli, Commands};

/// `docs/cli.md` is the user-facing CLI reference. Reached the way
/// `commands/capabilities.rs` reaches `docs/contracts.md` and `setup.rs`
/// reaches the systemd unit: embedded, so a doc that stops describing the
/// binary breaks the build rather than the reader.
const CLI_DOC: &str = include_str!("../../../../docs/cli.md");

/// Resolve the book's canonical include without maintaining a second copy.
const BOOK_CLI_DOC: &str = include_str!("../../../../book/src/cli-reference.md");

/// The man page. Matched only through [`unescaped_man_page`].
const MAN_PAGE: &str = include_str!("../../../../man/facelock.1");

/// The contract document. Not a CLI reference — it is the normative copy of
/// the rules the references paraphrase, so a list that decides behaviour
/// (which PAM services the sensitive gate covers) is checked against it
/// rather than against prose that is allowed to summarize.
const CONTRACTS_DOC: &str = include_str!("../../../../docs/contracts.md");

/// Both prose copies of the CLI reference, by repository path so a failure
/// names the file that is missing the entry rather than just the entry.
fn markdown_references() -> Vec<(&'static str, &'static str)> {
    let book = if BOOK_CLI_DOC.trim() == "{{#include ../../docs/cli.md}}" {
        CLI_DOC
    } else {
        BOOK_CLI_DOC
    };
    vec![
        ("docs/cli.md", CLI_DOC),
        ("book/src/cli-reference.md", book),
    ]
}

fn command_section<'a>(doc: &'a str, path: &str) -> Option<&'a str> {
    let mut offset = 0;
    let mut start = None;
    let mut depth = 0;
    for line in doc.split_inclusive('\n') {
        if line.starts_with('#') {
            let title = line.trim_start_matches('#').trim();
            let current_depth = line.len() - line.trim_start_matches('#').len();
            if let Some(start) = start
                && (current_depth <= depth || title.starts_with("facelock "))
            {
                return Some(&doc[start..offset]);
            }
            if title == path {
                start = Some(offset + line.len());
                depth = current_depth;
            }
        }
        offset += line.len();
    }
    start.map(|start| &doc[start..])
}

#[test]
fn docs_cli_covers_every_public_command_and_local_argument() {
    let root = super::surface::command_tree();
    let mut commands = Vec::new();
    walk(&root, "", &mut commands);
    let mut failures = Vec::new();
    for (doc_path, doc) in markdown_references() {
        for (path, command) in &commands {
            if path == "facelock" || super::surface::classification(path, command) != "public" {
                continue;
            }
            let Some(body) = command_section(doc, path) else {
                failures.push(format!("{doc_path}: {path}: missing command heading"));
                continue;
            };
            for missing in super::surface::missing_arguments(command, body, false) {
                failures.push(format!("{doc_path}: {path}: missing {missing}"));
            }
        }
        for missing in
            super::surface::missing_arguments(&root, section_of(doc, "\n## Global flags\n"), true)
        {
            failures.push(format!("{doc_path}: global: missing {missing}"));
        }
    }
    assert!(
        failures.is_empty(),
        "CLI reference coverage:\n{}",
        failures.join("\n")
    );
}

#[test]
fn command_sections_do_not_borrow_neighboring_flags() {
    let doc =
        "## facelock first\nfirst\n### facelock first child\n--json\n## facelock second\n--user\n";
    assert!(
        !command_section(doc, "facelock first")
            .expect("first")
            .contains("--json")
    );
    assert!(
        !command_section(doc, "facelock first child")
            .expect("child")
            .contains("--user")
    );
}

/// The man page with its roff hyphen escapes undone. Shared with
/// [`super::man_pam`], which asks the same question of the other page.
fn unescaped_man_page() -> String {
    super::unescape_roff(MAN_PAGE)
}

/// The body of one `##` section, so a check scoped to a section cannot be
/// satisfied by matching text somewhere else in the file.
fn section_of<'a>(doc: &'a str, heading: &str) -> &'a str {
    let start = doc
        .find(heading)
        .unwrap_or_else(|| panic!("`{}` is missing from the document", heading.trim()));
    let body = &doc[start + heading.len()..];
    match body.find("\n## ") {
        Some(end) => &body[..end],
        None => body,
    }
}

/// The body of one `###` subsection, ending at the next heading of the same
/// level or higher.
///
/// [`section_of`] stops only at a `##`, which is right for `docs/cli.md` and
/// wrong here: `### facelock pam Semantics` would run on through
/// `### facelock capabilities` and two more subsections, and a check scoped
/// to it could be satisfied by text describing a different command.
fn subsection_of<'a>(doc: &'a str, heading: &str) -> &'a str {
    let start = doc
        .find(heading)
        .unwrap_or_else(|| panic!("`{}` is missing from the document", heading.trim()));
    let body = &doc[start + heading.len()..];
    let end = [body.find("\n## "), body.find("\n### ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(body.len());
    &body[..end]
}

/// Every subcommand the binary offers is written down.
///
/// The rot this exists to stop was real: `is-enrolled`, `hyprlock`,
/// `reseal`, `pam` and `capabilities` all shipped without ever reaching
/// the reference, and `is-enrolled` is the one command Omarchy's
/// integration depends on.
///
/// **Top-level commands get a `## facelock <name>` heading of their own.**
/// A nested verb (`bench camera-reopen`, `tpm unseal-check`, `pam status`,
/// `hyprlock enable`) does *not*: it is documented under its parent's
/// section, which is how the file already reads and how a reader looks one
/// up. So the assertion for a nested verb is weaker on purpose — the full
/// invocation must appear somewhere in the file — but it is still enough
/// to catch a whole verb going undocumented, which is what happened to
/// `tpm unseal-check`.
///
/// Clap's built-in `help` pseudo-command is skipped at every level.
///
/// Both prose copies are checked. The book's is a hand-maintained near-copy
/// rather than a generated one, so holding only `docs/cli.md` to this would
/// leave the copy that rots more freely unpinned.
#[test]
fn docs_cli_documents_every_subcommand() {
    let root = Cli::command();

    for (doc_path, doc) in markdown_references() {
        for command in root.get_subcommands() {
            let name = command.get_name();
            if name == "help" || command.is_hide_set() {
                continue;
            }
            let heading = format!("\n## facelock {name}\n");
            assert!(
                doc.contains(&heading),
                "`facelock {name}` ships but `{doc_path}` has no \
                 `## facelock {name}` heading"
            );

            for nested in command.get_subcommands() {
                let nested_name = nested.get_name();
                if nested_name == "help" || nested.is_hide_set() {
                    continue;
                }
                let invocation = format!("facelock {name} {nested_name}");
                assert!(
                    doc.contains(&invocation),
                    "`{invocation}` ships but is not mentioned anywhere in \
                     `{doc_path}`; document it under the `## facelock {name}` section"
                );
            }
        }
    }
}

/// Every `setup` flag is named in the section a capability points at.
///
/// `setup-no-pam`, `setup-systemd` and `setup-if-present` are capability
/// names: a wrapper reads one, then opens `docs/cli.md` to find out what the
/// flag it has just proved exists actually does. That section documented none
/// of the choice or action flags for the whole cycle they shipped in — the man
/// page and the book carried them and `docs/cli.md` did not. It is the rot
/// [`docs_cli_documents_every_subcommand`] catches, one level down.
///
/// Scoped to `setup` on purpose. Holding every command's flags to this would
/// demand a flag table on commands that read better as prose, which is a
/// decision about what the reference contains rather than a check that it is
/// accurate. `setup` is the one section a capability name sends a reader to
/// for a flag.
///
/// A global flag is skipped. `--config` and `--quiet` are documented once
/// under `## Global flags`, which is what `global = true` is for;
/// `Cli::command()` does not propagate them into the subcommand today, so this
/// is a forward guard rather than a filter that fires.
///
/// A flag counts as documented when its long name is a backticked token of its
/// own — ``` `--no-pam` ``` — or opens one carrying a metavariable —
/// ``` `--camera <PATH|auto>` ```. Both spellings are in the file. What
/// neither accepts is the name appearing only in running prose, where a reader
/// scanning the tables never reaches it.
#[test]
fn docs_cli_setup_documents_every_flag() {
    let root = Cli::command();
    let setup = root
        .get_subcommands()
        .find(|command| command.get_name() == "setup")
        .expect("`facelock setup` ships");

    let section = section_of(CLI_DOC, "\n## facelock setup\n");

    for arg in setup.get_arguments() {
        if arg.is_global_set() {
            continue;
        }
        let Some(long) = arg.get_long() else {
            continue;
        };
        if long == "help" || long == "version" {
            continue;
        }
        assert!(
            section.contains(&format!("`--{long}`")) || section.contains(&format!("`--{long} <")),
            "`facelock setup --{long}` ships but the `## facelock setup` \
             section of docs/cli.md never names it"
        );
    }
}

/// The `## Machine-readable output` section names every command that
/// offers `--json`.
///
/// That section is a third copy of a list the binary already holds twice —
/// the `JSON_COMMANDS` registry above, and the table in
/// `docs/contracts.md`. Prose is the copy that rots silently, because
/// nothing fails when a newly added `--json` never reaches it.
/// `JSON_COMMANDS` is pinned against the clap tree and not against this
/// text, so the two checks do not prop each other up: the walk below is the
/// clap tree, and a command satisfies it only by being written down.
///
/// One direction only. Asserting that nothing *else* is named there would
/// mean parsing prose for command names, and the reverse direction is
/// already covered — a command named here that binds no `--json` fails
/// `cli_flag_conformance` against `JSON_COMMANDS` first.
///
/// `docs/cli.md` only, unlike the coverage and example checks either side
/// of it. The book's copy has no `## Machine-readable output` section at
/// all, so holding it to this would not find rot — it would demand a
/// section that has never existed, which is a decision about what the book
/// contains rather than a check that it is accurate.
#[test]
fn docs_cli_machine_output_section_names_every_json_command() {
    let root = Cli::command();
    let mut commands = Vec::new();
    walk(&root, "", &mut commands);

    let section = section_of(CLI_DOC, "\n## Machine-readable output\n");

    for (path, command) in &commands {
        if !command.get_arguments().any(|arg| arg.get_id() == "json") {
            continue;
        }
        assert!(
            section.contains(path.as_str()),
            "`{path}` offers `--json` but the `## Machine-readable output` \
             section of docs/cli.md does not name it"
        );
    }
}

/// Every `facelock …` line the reference shows must actually parse.
///
/// A reference example is a promise that the command line works, and the
/// cheapest way to break that promise is to document a flag that was
/// renamed or never existed. This does not prove an example *succeeds* —
/// most need root, a camera or a TPM — only that clap accepts it, which is
/// the half a unit test can own.
///
/// The shared extractor owns quoting, wrappers, shell operators, source
/// locations and classification. Schematic and manual records stay visible
/// in its report without being passed off as executable parser evidence.
#[test]
fn docs_cli_examples_all_parse() {
    for (source, argv) in super::examples::executable_invocations() {
        match Cli::try_parse_from(argv) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ) => {}
            Err(error) => panic!(
                "`{}` in {}:{} must parse: {error}",
                argv.join(" "),
                source.path,
                source.line
            ),
        }
    }
}

/// Every gated service name is written down in all three references.
///
/// Its own test rather than a preamble to
/// [`docs_cli_examples_never_install_into_a_gated_service`]: this one reads
/// prose and that one resolves invocations, so a shared name told a reader the
/// wrong thing about which of the two had broken.
#[test]
fn docs_references_name_every_gated_service() {
    use facelock_cli::commands::pam::SENSITIVE_SERVICES;

    // Which services are gated is enumerated in prose in every reference,
    // and a reader who cannot look the list up has to discover it by being
    // refused. `SENSITIVE_SERVICES` is the only copy that decides anything,
    // so every other copy is checked against it.
    //
    // Containment is a deliberately loose test: `login` would also match
    // "login managers". It still catches the failure that matters, which is
    // a name added to `SENSITIVE_SERVICES` and written down nowhere.
    let man = unescaped_man_page();
    let mut references = markdown_references();
    references.push(("man/facelock.1", &man));
    for (doc_path, doc) in &references {
        for service in SENSITIVE_SERVICES {
            assert!(
                doc.contains(service),
                "`{doc_path}` never names the gated service `{service}`"
            );
        }
    }
}

/// The contract's own list of gated services is the whole list.
///
/// `docs/contracts.md` is where an operator looks up which service names
/// need `--allow-sensitive`, and it enumerates them rather than pointing at
/// the code — so a name missing from the sentence reads as a name that does
/// not need the flag. The list has already grown once past what the prose
/// said: `system-auth-ac` and `password-auth-ac` joined
/// [`SENSITIVE_SERVICES`] for the files RHEL's older `authconfig` leaves
/// behind, and the section went on claiming six for both.
///
/// Stricter than [`docs_references_name_every_gated_service`] in the two
/// ways that matter for a normative document: the name must appear inside
/// `### facelock pam Semantics` rather than anywhere in the file, and it
/// must be marked up as code, so "logins are audited" cannot stand in for
/// `login`.
#[test]
fn contracts_name_every_gated_service() {
    use facelock_cli::commands::pam::SENSITIVE_SERVICES;

    let section = subsection_of(CONTRACTS_DOC, "\n### facelock pam Semantics\n");

    for service in SENSITIVE_SERVICES {
        assert!(
            section.contains(&format!("`{service}`")),
            "`{service}` is gated by SENSITIVE_SERVICES but the \
             `### facelock pam Semantics` section of docs/contracts.md never \
             names it"
        );
    }

    // The names alone would not have caught the rot this test was written
    // for. `system-auth-ac` and `password-auth-ac` were already named a few
    // paragraphs up, in the symlink rule that explains why they are on the
    // list, while the sentence that enumerates the gate went on saying six.
    // So the size is pinned too, in the one spelling the section uses.
    const COUNTS: &[&str] = &[
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    let count = COUNTS
        .get(SENSITIVE_SERVICES.len())
        .expect("SENSITIVE_SERVICES outgrew the numerals this check spells");
    assert!(
        section.contains(&format!("of the {count}")),
        "SENSITIVE_SERVICES holds {} services but the `### facelock pam \
         Semantics` section of docs/contracts.md never says \"of the {count}\" \
         — the count in the prose is stale",
        SENSITIVE_SERVICES.len()
    );
}

/// No example installs into a gated PAM service without the flag that
/// unlocks it.
///
/// This is the bug that motivated the gap: `docs/cli.md` documented
/// `facelock setup --pam --service login`, and `login` is in
/// `SENSITIVE_SERVICES`, so the command as written **fails**. Parsing
/// cannot catch it — the gate is a runtime refusal, not a parse error —
/// so the check has to resolve the invocation the way `main` does and ask
/// the writer's own question.
///
/// Only the add direction is gated. `pam remove --service login` is a
/// documented example precisely because removal is never gated.
#[test]
fn docs_cli_examples_never_install_into_a_gated_service() {
    use facelock_cli::commands::pam::{PamAction, SENSITIVE_SERVICES};

    let gated = |services: &[String]| -> Option<String> {
        services
            .iter()
            .find(|s| SENSITIVE_SERVICES.contains(&s.as_str()))
            .cloned()
    };

    for (source, argv) in super::examples::executable_invocations() {
        let doc_path = &source.path;
        let rendered = argv.join(" ");
        let cli = match Cli::try_parse_from(argv) {
            Ok(cli) => cli,
            Err(error)
                if matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ) =>
            {
                continue;
            }
            Err(error) => panic!("{doc_path}: {rendered}: {error}"),
        };

        match cli.command {
            Commands::Pam { command } => {
                let request: facelock_cli::commands::pam::PamRequest = command.into();
                if request.action != PamAction::Add || request.allow_sensitive {
                    continue;
                }
                if let Some(service) = gated(&request.services) {
                    panic!(
                        "`{rendered}` in {doc_path} installs into the gated service \
                             `{service}` without --allow-sensitive, so it fails as written"
                    );
                }
            }
            Commands::Setup(setup) => {
                let plan = resolve_setup_plan(SetupArgs::from(setup));
                if plan.allow_sensitive {
                    continue;
                }
                let PamPref::Install {
                    service: Some(service),
                    ..
                } = &plan.pam
                else {
                    continue;
                };
                if let Some(service) = gated(std::slice::from_ref(service)) {
                    panic!(
                        "`{rendered}` in {doc_path} installs into the gated service \
                             `{service}` without --allow-sensitive, so it fails as written"
                    );
                }
            }
            _ => {}
        }
    }
}
