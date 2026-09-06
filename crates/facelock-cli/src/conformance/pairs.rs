//! Six documents exist twice, and the copies drift (#211).
//!
//! `docs/` and `book/src/` carry a same-named page for `architecture`,
//! `compatibility`, `configuration`, `quickstart`, `security` and
//! `troubleshooting`. `contracts.md` used to be a seventh until #187 made the
//! book's copy a one-line `{{#include}}`; the other six are still two files
//! maintained by hand, and nothing compares them.
//!
//! **Byte equality would be the wrong check.** They are deliberately different
//! documents for different readers: `docs/security.md` is the normative
//! security review and `book/src/security.md` is the chapter someone reads
//! before installing. The book is allowed to say less, in different words, in a
//! different order.
//!
//! What it is not allowed to do is say something *else*. So the comparison is
//! scoped to headings both files carry, and within a shared heading to the
//! tokens that decide what a reader types or looks for:
//!
//! - **package names** must match exactly, in both directions. A GPU section
//!   that names one package in `docs/` and another in `book/` is #209 with an
//!   extra step, and neither copy is more authoritative than the other here.
//! - **command names and absolute paths** must be a subset on the book's side.
//!   The book may omit what `docs/` covers — that is the relationship between
//!   the two — but a command or a path it names under a shared heading and
//!   `docs/` does not is either invented or renamed.
//!
//! What this does **not** catch is stated plainly because it matters: a fact
//! that `docs/` gains and the book never does. That is the direction #211
//! actually found — `docs/security.md` documented TPM PCR binding at length
//! and the book's security page said nothing about it — and it cannot be
//! mechanical, because "the book is shallower" is the design. The only pin for
//! it is [`BOOK_MUST_COVER`], a curated list of facts a reader must not be able
//! to miss by reading the book instead of the docs.
//!
//! Also out of scope: prose, ordering, headings that exist on one side only,
//! and the `docs/cli.md` ↔ `book/src/cli-reference.md` pair, which is not
//! same-named and is already held to a stricter standard by [`super::docs`].

use std::collections::BTreeSet;

use clap::{CommandFactory, Parser};

use super::packages::{install_mentions, table_mentions};
use crate::Cli;

/// The pairs, as `(docs path, docs text, book path, book text)`.
///
/// Embedded rather than walked: these are named documents whose disappearance
/// should break the build, which is the same reasoning [`super::docs`] applies
/// to `docs/cli.md`. A seventh pair added to the tree and not to this list is
/// caught by [`every_same_named_page_is_paired`].
const PAIRS: &[(&str, &str, &str, &str)] = &[
    (
        "docs/architecture.md",
        include_str!("../../../../docs/architecture.md"),
        "book/src/architecture.md",
        include_str!("../../../../book/src/architecture.md"),
    ),
    (
        "docs/compatibility.md",
        include_str!("../../../../docs/compatibility.md"),
        "book/src/compatibility.md",
        include_str!("../../../../book/src/compatibility.md"),
    ),
    (
        "docs/configuration.md",
        include_str!("../../../../docs/configuration.md"),
        "book/src/configuration.md",
        include_str!("../../../../book/src/configuration.md"),
    ),
    (
        "docs/quickstart.md",
        include_str!("../../../../docs/quickstart.md"),
        "book/src/quickstart.md",
        include_str!("../../../../book/src/quickstart.md"),
    ),
    (
        "docs/security.md",
        include_str!("../../../../docs/security.md"),
        "book/src/security.md",
        include_str!("../../../../book/src/security.md"),
    ),
    (
        "docs/troubleshooting.md",
        include_str!("../../../../docs/troubleshooting.md"),
        "book/src/troubleshooting.md",
        include_str!("../../../../book/src/troubleshooting.md"),
    ),
];

/// Facelock-owned path roots. A path under one of these is a promise about
/// this machine; anything else in the prose is an example or a third party's
/// file and is none of this check's business.
const PATH_ROOTS: &[&str] = &[
    "/etc/facelock",
    "/var/lib/facelock",
    "/var/log/facelock",
    "/run/facelock",
    "/lib/security",
    "/usr/lib/security",
    "/usr/lib64/security",
    "/usr/bin/facelock",
    "/usr/share/dbus-1",
    "/usr/share/pam-configs",
    "/usr/lib/systemd",
    "/etc/dbus-1",
    "/etc/pam.d",
    "/usr/lib/pam.d",
];

/// Facts a reader of the book must not be able to miss.
///
/// The one direction the mechanical comparison cannot cover: `docs/` gaining
/// depth the book never gets. Every entry is a decision a reader makes
/// differently depending on whether they know it, not merely a detail `docs/`
/// happens to carry — which is why the list is short and why adding to it is a
/// judgement rather than a rule.
///
/// Every token here is checked to be real by
/// [`required_coverage_names_things_that_exist`], so this cannot pin the book
/// to a config key or a command that no longer ships.
const BOOK_MUST_COVER: &[(&str, &[&str])] = &[(
    "book/src/security.md",
    &[
        // `tpm.pcr_binding` decides whether a firmware or kernel update
        // silently costs the user their enrolled templates. The book's
        // security page described TPM sealing and stopped there, so a reader
        // who chose the `tpm` method from the book alone had no way to know
        // there was a reseal step, or a backup that makes it painless.
        "pcr_binding",
        "facelock tpm reseal",
        "encryption.key",
    ],
)];

/// A `##` or `###` heading and the body under it, ending at the next heading
/// of either level.
///
/// Fenced code blocks are skipped when looking for headings: a `### ` inside a
/// shell example is a comment, not a section.
fn sections(doc: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut heading = String::from("(preamble)");
    let mut body = String::new();
    let mut fenced = false;

    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let is_heading = !fenced
            && (line.starts_with("## ") || line.starts_with("### "))
            && !line.contains('\r');
        if is_heading {
            sections.push((heading, std::mem::take(&mut body)));
            heading = line.trim_end().to_string();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    sections.push((heading, body));
    sections
}

/// Package names an install command or a package table hands a reader.
fn packages(body: &str) -> BTreeSet<String> {
    install_mentions(body)
        .into_iter()
        .chain(table_mentions(body))
        .map(|(_, name)| name)
        .collect()
}

/// `facelock <verb>` mentions, filtered to verbs the binary actually has.
///
/// The filter is what makes this usable: without it "facelock is a PAM module"
/// contributes a command called `is`. With it, a heading can only contribute a
/// name the clap tree agrees exists — so the check compares documentation of
/// real commands rather than parts of speech.
fn commands(body: &str, known: &BTreeSet<String>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = body;
    while let Some(at) = rest.find("facelock ") {
        rest = &rest[at + "facelock ".len()..];
        let verb: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || *c == '-')
            .collect();
        if known.contains(&verb) {
            found.insert(verb);
        }
    }
    found
}

/// Absolute paths under a facelock-owned root.
fn paths(body: &str) -> BTreeSet<String> {
    const TRAILING: [char; 6] = ['/', '.', ',', ')', '`', ':'];

    let mut found = BTreeSet::new();
    for token in body.split(|c: char| c.is_whitespace() || c == '`' || c == '(' || c == '"') {
        let Some(start) = token.find('/') else {
            continue;
        };
        let token = &token[start..];
        if !PATH_ROOTS.iter().any(|root| token.starts_with(root)) {
            continue;
        }
        let token = token.trim_end_matches(TRAILING);
        if !token.is_empty() {
            found.insert(token.to_string());
        }
    }
    found
}

/// A parent directory cannot vouch for a managed backup filename or a
/// sibling with the same prefix. Permit only concrete canonical refinements.
fn path_is_covered(path: &str, known: &BTreeSet<String>) -> bool {
    if known.contains(path) {
        return true;
    }
    const REFINEMENTS: &[(&str, &str)] = &[
        ("/var/lib/facelock", "/var/lib/facelock/facelock.db"),
        ("/var/lib/facelock", "/var/lib/facelock/models"),
        ("/etc/facelock", "/etc/facelock/config.toml"),
        (
            "/usr/share/dbus-1/system.d",
            "/usr/share/dbus-1/system.d/org.facelock.Daemon.conf",
        ),
    ];
    REFINEMENTS
        .iter()
        .any(|(parent, child)| path == *child && known.contains(*parent))
}

#[test]
fn managed_path_coverage_rejects_stale_adjacent_backups_and_prefixes() {
    let known = [
        "/etc/pam.d".to_owned(),
        "/etc/pam.d/sudo".to_owned(),
        "/var/lib/facelock".to_owned(),
    ]
    .into_iter()
    .collect();
    assert!(!path_is_covered("/etc/pam.d/sudo.facelock-backup", &known));
    assert!(!path_is_covered(
        "/var/lib/facelock-old/facelock.db",
        &known
    ));
    assert!(path_is_covered("/var/lib/facelock/facelock.db", &known));
}

/// Every top-level and nested verb the binary offers.
fn known_commands() -> BTreeSet<String> {
    let root = Cli::command();
    let mut names = BTreeSet::new();
    for command in root.get_subcommands() {
        if command.get_name() == "help" {
            continue;
        }
        names.insert(command.get_name().to_string());
    }
    names
}

/// Under a heading both copies carry, both name the same packages.
///
/// Exact, and in both directions: neither copy is the authority on which
/// package a reader installs, so a disagreement is a defect wherever it sits.
#[test]
fn paired_sections_name_the_same_packages() {
    for (docs_path, docs, book_path, book) in PAIRS {
        let book_sections = sections(book);
        for (heading, docs_body) in sections(docs) {
            let Some((_, book_body)) = book_sections.iter().find(|(h, _)| *h == heading) else {
                continue;
            };
            let in_docs = packages(&docs_body);
            let in_book = packages(book_body);
            assert_eq!(
                in_docs, in_book,
                "`{heading}` names different packages in {docs_path} ({in_docs:?}) \
                 and {book_path} ({in_book:?}); a reader following one of them \
                 installs something the other never mentions"
            );
        }
    }
}

/// Under a heading both copies carry, the book names no command the docs do
/// not.
///
/// One direction on purpose. The book is the shallower of the two, so a
/// command it omits is editorial; a command it introduces under a heading its
/// canonical copy does not mention is a rename that only landed in one place,
/// or a command that never existed.
#[test]
fn paired_sections_introduce_no_command_the_docs_lack() {
    let known = known_commands();

    for (docs_path, docs, book_path, book) in PAIRS {
        let book_sections = sections(book);
        for (heading, docs_body) in sections(docs) {
            let Some((_, book_body)) = book_sections.iter().find(|(h, _)| *h == heading) else {
                continue;
            };
            let in_docs = commands(&docs_body, &known);
            for verb in commands(book_body, &known) {
                assert!(
                    in_docs.contains(&verb),
                    "`{heading}` in {book_path} documents `facelock {verb}`, but \
                     the same section of {docs_path} never mentions it"
                );
            }
        }
    }
}

/// Under a heading both copies carry, the book names no facelock path the docs
/// do not.
///
/// A book path that refines a directory the docs name is fine — naming
/// `/var/lib/facelock/facelock.db` where the docs said `/var/lib/facelock` is
/// more specific, not different. A path with no such ancestor is a location
/// only one copy believes in.
#[test]
fn paired_sections_introduce_no_path_the_docs_lack() {
    let mut failures = Vec::new();
    for (docs_path, docs, book_path, book) in PAIRS {
        let book_sections = sections(book);
        for (heading, docs_body) in sections(docs) {
            let Some((_, book_body)) = book_sections.iter().find(|(h, _)| *h == heading) else {
                continue;
            };
            let in_docs = paths(&docs_body);
            for path in paths(book_body) {
                if !path_is_covered(&path, &in_docs) {
                    failures.push(format!("`{heading}` in {book_path} names `{path}`, which the same section of {docs_path} never mentions"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The book carries the facts a reader would otherwise only find in `docs/`.
#[test]
fn book_pages_cover_the_facts_that_change_a_decision() {
    for (path, required) in BOOK_MUST_COVER {
        let (_, _, _, book) = PAIRS
            .iter()
            .find(|(_, _, book_path, _)| book_path == path)
            .unwrap_or_else(|| panic!("`{path}` is not one of the paired documents"));
        for fact in *required {
            assert!(
                book.contains(fact),
                "`{path}` never mentions `{fact}`. Its canonical copy in docs/ \
                 covers it, and a reader who reaches the book instead is missing \
                 the tradeoff, not just the detail"
            );
        }
    }
}

/// The required-coverage list names things that exist.
///
/// A curated list is the one part of this suite that is asserted rather than
/// derived, so each entry is held to the tree: a command must parse, and a
/// config key must be in the shipped template. Without this the list could go
/// on pinning the book to a key that was renamed two releases ago.
#[test]
fn required_coverage_names_things_that_exist() {
    const CONFIG_TEMPLATE: &str = include_str!("../../../../config/facelock.toml");

    for (_, required) in BOOK_MUST_COVER {
        for fact in *required {
            if let Some(command) = fact.strip_prefix("facelock ") {
                let argv: Vec<&str> = std::iter::once("facelock")
                    .chain(command.split_whitespace())
                    .collect();
                Cli::try_parse_from(&argv).unwrap_or_else(|e| {
                    panic!("BOOK_MUST_COVER pins `{fact}`, which does not parse: {e}")
                });
                continue;
            }
            assert!(
                CONFIG_TEMPLATE.contains(fact),
                "BOOK_MUST_COVER pins `{fact}`, which config/facelock.toml does \
                 not mention — either it was renamed or the pin was never real"
            );
        }
    }
}

/// Every page that exists under both names is in [`PAIRS`].
///
/// The list is embedded, so a seventh pair added to the tree would simply not
/// be checked. This walks the two directories and says so.
#[test]
fn every_same_named_page_is_paired() {
    use std::fs;
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let names = |dir: &Path| -> BTreeSet<String> {
        fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{} must be readable: {e}", dir.display()))
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension()? == "md")
                    .then(|| path.file_name()?.to_str().map(str::to_string))?
            })
            .collect()
    };

    let shared: BTreeSet<String> = names(&root.join("docs"))
        .intersection(&names(&root.join("book/src")))
        .cloned()
        .collect();
    assert!(
        shared.len() >= 6,
        "found only {} same-named pages — the walk is broken, not the tree",
        shared.len()
    );

    for name in shared {
        // Exact canonical includes cannot drift; validate the actual target
        // rather than exempting filenames from coverage.
        let book =
            fs::read_to_string(root.join("book/src").join(&name)).expect("read paired book page");
        if book.trim() == format!("{{{{#include ../../docs/{name}}}}}") {
            continue;
        }
        let book_path = format!("book/src/{name}");
        assert!(
            PAIRS.iter().any(|(_, _, path, _)| *path == book_path),
            "`docs/{name}` and `{book_path}` are a same-named pair that PAIRS \
             does not list, so nothing compares them"
        );
    }
}
