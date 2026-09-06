//! Distro package names (#209, #211): a name a document tells a reader to
//! install is one the project's own packaging declares.
//!
//! `onnxruntime-opt-openvino` reached five user-facing files and a shipped
//! config template. No Arch repository and no AUR package has ever carried
//! that name, so the Intel setup path stopped at a failed `pacman -S` — and
//! the person who found it was a contributor evaluating facelock for Omarchy,
//! not us. The same class had already shipped twice (#172, #187). Nothing in
//! the tree validated a package name anywhere.
//!
//! **Offline by construction.** `just check` and CI must not need the network,
//! so nothing here queries a repository. The known-good names are *derived*
//! instead: `dist/PKGBUILD*`, `debian/control` and `dist/facelock.spec`
//! already declare every package the project builds, depends on or suggests,
//! and those manifests are exercised against real repositories by the
//! packaging jobs on every release. A name a document hands a reader must
//! appear in the manifest for that document's distro. `onnxruntime-opt-openvino`
//! never appeared in `dist/PKGBUILD` — it was invented in prose, which is
//! exactly the shape this catches.
//!
//! What the derivation does *not* prove is that a third-party name still
//! exists upstream: the manifest could be as wrong as the prose was.
//! `just check-package-names-live` revalidates the derived set against Arch,
//! the AUR, Debian and Fedora over the network. It is opt-in and deliberately
//! outside `just check`.
//!
//! The corpus is walked at test time rather than `include_str!`d, unlike
//! [`super::docs`]. That module pins named facts in named documents, where a
//! deleted file should break the build; this one sweeps every document a
//! reader could take an instruction from, where a *new* file that nobody
//! remembered to register is the failure mode. A floor assertion stands in for
//! the compile-time guarantee: a walk that stops finding files fails loudly
//! rather than passing vacuously.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The packaging manifest allowed to vouch for a name.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Distro {
    Arch,
    Debian,
    Fedora,
    /// A package manager the project ships no manifest for. Every name
    /// reached through one of these fails: there is nothing to check it
    /// against, and an unverifiable instruction is the thing being stopped.
    Unmanifested,
}

impl Distro {
    /// Named in the failure message so the fix is obvious: add the dependency
    /// where it belongs, or stop telling readers to install it.
    fn manifest(self) -> &'static str {
        match self {
            Distro::Arch => "dist/PKGBUILD, dist/PKGBUILD-bin or dist/PKGBUILD-git",
            Distro::Debian => "debian/control",
            Distro::Fedora => "dist/facelock.spec",
            Distro::Unmanifested => "no packaging manifest in this tree",
        }
    }
}

/// Install verbs that take bare package names, and the manifest that has to
/// declare what follows them.
///
/// The trailing space is load-bearing: without it `pacman -S` also matches
/// `pacman -Ss` and `pacman -Sy`, and the guard would read a flag as a
/// package name.
const INSTALL_VERBS: &[(&str, Distro)] = &[
    ("pacman -S ", Distro::Arch),
    ("pacman -Syu ", Distro::Arch),
    ("yay -S ", Distro::Arch),
    ("paru -S ", Distro::Arch),
    ("apt-get install ", Distro::Debian),
    ("apt install ", Distro::Debian),
    ("dnf install ", Distro::Fedora),
    ("yum install ", Distro::Fedora),
    ("zypper install ", Distro::Unmanifested),
    ("apk add ", Distro::Unmanifested),
    ("emerge ", Distro::Unmanifested),
];

/// Names that are real but that no manifest in this tree declares.
///
/// Keep this narrow: source-development tools, repository setup tools, and
/// concrete providers of virtual dependencies need not be runtime package
/// dependencies. Each exception records its purpose and dated repository
/// evidence. Never add a fabricated name merely to make documentation pass.
const KNOWN_EXTERNAL: &[(Distro, &str, &str)] = &[
    // Official Arch JSON: https://archlinux.org/packages/search/json/?name=NAME
    // All checked 2026-09-05; package manifests retain their virtual dependencies.
    (
        Distro::Arch,
        "base-devel",
        "source build tools; implicit makepkg prerequisite, official Core",
    ),
    (
        Distro::Arch,
        "just",
        "source checkout recipe runner, official Extra",
    ),
    (
        Distro::Arch,
        "pkgconf",
        "concrete source pkg-config implementation, official Core",
    ),
    (
        Distro::Arch,
        "v4l-utils",
        "source V4L2 development tools, official Extra",
    ),
    (
        Distro::Arch,
        "onnxruntime-cpu",
        "concrete provider of onnxruntime, official Extra",
    ),
    // Debian 13 and Ubuntu 26.04 package metadata checked in disposable containers
    // 2026-09-05; https://packages.debian.org/trixie/{build-essential,just}
    (
        Distro::Debian,
        "build-essential",
        "implicit Debian build tools, explicit source checkout prerequisite",
    ),
    (
        Distro::Debian,
        "just",
        "source checkout recipe runner, not a binary-package dependency",
    ),
    (
        Distro::Debian,
        "git",
        "source checkout retrieval, not needed by the release-archive package build",
    ),
    // Fedora 43 repoquery checked 2026-09-05; official package descriptions:
    // https://packages.fedoraproject.org/pkgs/just/just/
    // https://packages.fedoraproject.org/pkgs/pkgconf/pkgconf-pkg-config/
    // https://packages.fedoraproject.org/pkgs/dnf5/dnf5-plugins/
    (Distro::Fedora, "just", "source checkout recipe runner"),
    (
        Distro::Fedora,
        "git",
        "source checkout retrieval, not needed by the release-archive package build",
    ),
    (
        Distro::Fedora,
        "pkgconf-pkg-config",
        "source pkg-config command provider",
    ),
    (
        Distro::Fedora,
        "dnf5-plugins",
        "repository setup: provides dnf copr, not a Facelock runtime dependency",
    ),
];

/// Repository root, from the compile-time manifest directory so the walk does
/// not depend on the working directory `cargo test` happens to use.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("the crate sits two levels below the repository root")
}

/// Every document a reader could take an install instruction from.
///
/// `docs/` is walked recursively so `docs/adr/` is covered; `book/src/` is
/// flat. `config/facelock.toml` is here because it ships to
/// `/etc/facelock/config.toml` — its comments are instructions a user reads
/// on their own machine, and they carried the #209 defect in prose form.
/// `website/index.html` is here because it is where an evaluator lands first.
fn instruction_corpus() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = vec![
        root.join("README.md"),
        root.join("AGENTS.md"),
        root.join("config/facelock.toml"),
        root.join("website/index.html"),
    ];
    collect_markdown(&root.join("docs"), &mut files);
    collect_markdown(&root.join("book/src"), &mut files);
    files.sort();

    let corpus: Vec<(String, String)> = files
        .iter()
        .map(|path| {
            let text = fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string();
            let text = if relative.ends_with(".html") {
                strip_markup(&text)
            } else {
                text
            };
            (relative, text)
        })
        .collect();

    assert!(
        corpus.len() > 25,
        "walked only {} documents — the walk is broken, not the tree",
        corpus.len()
    );
    corpus
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", dir.display()))
        .filter_map(Result::ok);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// Flatten HTML to the text a reader sees, so a command wrapped in
/// `<span class="command">` is still one command line.
///
/// Deliberately naive — the only job is to stop tags and entities from
/// splitting or joining tokens. Anything it gets wrong shows up as a package
/// name that fails to resolve, which is a loud failure and not a silent pass.
fn strip_markup(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut depth = 0usize;
    for ch in html.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// One package name a document hands a reader, with enough context to name
/// the offender.
#[derive(Debug)]
struct Mention {
    distro: Distro,
    name: String,
    source: String,
}

/// Package names reached through an install verb.
///
/// Scanning is per line and per verb, so `yay -S facelock  # or paru -S
/// facelock` yields both. A ` #` ends the argument list except where the
/// comment itself carries a second verb, which is why the search runs from
/// each verb rather than from the start of the line.
pub(super) fn install_mentions(text: &str) -> Vec<(Distro, String)> {
    let mut found = Vec::new();
    for line in text.lines() {
        for (verb, distro) in INSTALL_VERBS {
            let mut from = 0usize;
            while let Some(at) = line[from..].find(verb) {
                let start = from + at + verb.len();
                for name in package_tokens(&line[start..]) {
                    found.push((*distro, name));
                }
                from = start;
            }
        }
    }
    found
}

/// The bare names in an argument list, stopping where the command does.
///
/// Stops at a comment, a shell operator, or anything that cannot be a package
/// name — a placeholder, a redirect, a quoted string. Flags are skipped rather
/// than ending the list, since `--needed` and `-y` sit among the names.
///
/// Trailing markup (a closing backtick, a sentence's period) is trimmed and
/// then *ends* the list: a name cited inline in prose is still a name a reader
/// will type, but the words after the closing backtick are prose, not more
/// packages.
fn package_tokens(rest: &str) -> Vec<String> {
    const CLOSERS: [char; 7] = ['`', '.', ',', ')', '"', '\'', ';'];

    let mut names = Vec::new();
    let mut tokens = rest.split_whitespace();
    while let Some(raw) = tokens.next() {
        if raw.starts_with('#') {
            break;
        }
        if raw.starts_with('-') {
            if matches!(raw, "-t" | "--target-release") {
                tokens.next();
            }
            continue;
        }
        let token = raw.trim_end_matches(CLOSERS);
        if token.is_empty() || !is_package_name(token) {
            break;
        }
        names.push(token.to_string());
        if token.len() != raw.len() {
            break;
        }
    }
    names
}

/// The shape every distro this project ships to agrees on: lowercase, digits,
/// and `. _ + -`, opening on an alphanumeric.
fn is_package_name(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._+-".contains(c))
}

/// Package names given in a table rather than in a command.
///
/// The GPU tables in `README.md` and `book/src/gpu.md` name the package to
/// install in a column and leave the `pacman -S` to a code block further
/// down — which is how `onnxruntime-opt-openvino` survived in two files after
/// the command that installed it was fixed. A cell counts only when it is
/// *exactly* one backticked token and the column header names a distro, so a
/// prose cell (`none packaged`) and a mixed cell (`` `facelock`, TPM
/// enabled ``) are left alone.
pub(super) fn table_mentions(text: &str) -> Vec<(Distro, String)> {
    let mut found = Vec::new();
    let mut header: Option<Vec<String>> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            header = None;
            continue;
        }
        let cells: Vec<String> = trimmed
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();

        // The `|---|---|` rule line neither is a header nor carries data.
        if cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
        {
            continue;
        }
        let Some(columns) = &header else {
            header = Some(cells);
            continue;
        };

        for (column, cell) in columns.iter().zip(&cells) {
            if !column.contains("Package") {
                continue;
            }
            // A column headed only "Package" says nothing about which
            // repository the name lives in, so there is no manifest to check
            // it against.
            let distro = if column.contains("Arch") {
                Distro::Arch
            } else if column.contains("Debian") || column.contains("Ubuntu") {
                Distro::Debian
            } else if column.contains("Fedora") {
                Distro::Fedora
            } else {
                continue;
            };
            let Some(name) = cell.strip_prefix('`').and_then(|c| c.strip_suffix('`')) else {
                continue;
            };
            if is_package_name(name) {
                found.push((distro, name.to_string()));
            }
        }
    }
    found
}

/// Every name the packaging manifests declare, by distro.
fn declared_names() -> BTreeMap<Distro, BTreeSet<String>> {
    let root = repo_root();
    let read = |relative: &str| {
        fs::read_to_string(root.join(relative))
            .unwrap_or_else(|e| panic!("{relative} must be readable: {e}"))
    };

    let mut arch = BTreeSet::new();
    for pkgbuild in ["dist/PKGBUILD", "dist/PKGBUILD-bin", "dist/PKGBUILD-git"] {
        arch.extend(pkgbuild_names(&read(pkgbuild)));
    }

    BTreeMap::from([
        (Distro::Arch, arch),
        (Distro::Debian, control_names(&read("debian/control"))),
        (Distro::Fedora, spec_names(&read("dist/facelock.spec"))),
        (Distro::Unmanifested, BTreeSet::new()),
    ])
}

/// `pkgname` plus every dependency array.
///
/// `optdepends` entries are `'name: description'`, and every array entry can
/// carry a version constraint, so both are trimmed off.
fn pkgbuild_names(pkgbuild: &str) -> BTreeSet<String> {
    const ARRAYS: &[&str] = &["depends", "makedepends", "optdepends", "checkdepends"];

    let mut names = BTreeSet::new();
    let mut in_array = false;

    for line in pkgbuild.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("pkgname=") {
            names.insert(strip_constraint(name.trim_matches(['\'', '"'])));
            continue;
        }
        let opens = ARRAYS.iter().any(|array| {
            trimmed
                .strip_prefix(array)
                .is_some_and(|rest| rest.starts_with("=("))
        });
        if opens {
            in_array = true;
        }
        if !in_array {
            continue;
        }
        for entry in quoted_entries(trimmed) {
            let name = entry.split(':').next().unwrap_or(&entry);
            names.insert(strip_constraint(name.trim()));
        }
        if trimmed.ends_with(')') {
            in_array = false;
        }
    }
    names.remove("");
    names
}

/// The `'…'`-quoted entries on one line of a bash array.
fn quoted_entries(line: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find(['\'', '"']) {
        let quote = rest.as_bytes()[open] as char;
        let after = &rest[open + 1..];
        let Some(close) = after.find(quote) else {
            break;
        };
        entries.push(after[..close].to_string());
        rest = &after[close + 1..];
    }
    entries
}

/// `Package:` plus every dependency field.
///
/// Alternatives (`a | b`) count as two names; substvars (`${shlibs:Depends}`)
/// are not names at all.
fn control_names(control: &str) -> BTreeSet<String> {
    const FIELDS: &[&str] = &[
        "Package:",
        "Depends:",
        "Pre-Depends:",
        "Build-Depends:",
        "Recommends:",
        "Suggests:",
    ];

    let mut names = BTreeSet::new();
    let mut in_field = false;

    for line in control.lines() {
        let starts_field = FIELDS.iter().any(|field| line.starts_with(field));
        if starts_field {
            in_field = true;
        } else if !line.starts_with([' ', '\t']) {
            in_field = false;
        }
        if !in_field {
            continue;
        }
        let value = line.split_once(':').map_or(line, |(_, rest)| rest);
        for entry in value.split([',', '|']) {
            let entry = entry.trim();
            if entry.is_empty() || entry.starts_with('$') {
                continue;
            }
            let name = entry.split_whitespace().next().unwrap_or(entry);
            if is_package_name(name) {
                names.insert(name.to_string());
            }
        }
    }
    names
}

/// `Name:` plus every dependency tag. Macro-valued dependencies
/// (`%{name}-libs`) are skipped: nothing here expands macros.
fn spec_names(spec: &str) -> BTreeSet<String> {
    const TAGS: &[&str] = &[
        "Name:",
        "Requires:",
        "BuildRequires:",
        "Recommends:",
        "Suggests:",
    ];

    let mut names = BTreeSet::new();
    for line in spec.lines() {
        if !TAGS.iter().any(|tag| line.starts_with(tag)) {
            continue;
        }
        let Some((_, value)) = line.split_once(':') else {
            continue;
        };
        let Some(name) = value.split_whitespace().next() else {
            continue;
        };
        if is_package_name(name) {
            names.insert(name.to_string());
        }
    }
    names
}

/// Trim a version constraint from a dependency entry.
fn strip_constraint(entry: &str) -> String {
    let end = entry.find(['>', '<', '=']).unwrap_or(entry.len());
    entry[..end].trim().to_string()
}

/// Every package name a document hands a reader, from commands and tables
/// alike.
fn documented_packages() -> Vec<Mention> {
    let mut mentions = Vec::new();
    for (path, text) in instruction_corpus() {
        for (distro, name) in install_mentions(&text) {
            mentions.push(Mention {
                distro,
                name,
                source: path.clone(),
            });
        }
        for (distro, name) in table_mentions(&text) {
            mentions.push(Mention {
                distro,
                name,
                source: path.clone(),
            });
        }
    }
    assert!(
        mentions.len() > 10,
        "found only {} package mentions — the extractor is broken, not the docs",
        mentions.len()
    );
    mentions
}

/// Every documented package name is declared by its distro's packaging
/// manifest or the narrow, repository-verified external-tool allowlist.
///
/// An undeclared runtime/build dependency belongs in the packaging manifest.
/// Checkout and repository setup tools can be reviewed external exceptions;
/// fabricated package names must never be accepted merely to silence the guard.
#[test]
fn documented_packages_are_declared_by_packaging() {
    let declared = declared_names();

    for mention in documented_packages() {
        if KNOWN_EXTERNAL
            .iter()
            .any(|(distro, name, _)| *distro == mention.distro && *name == mention.name)
        {
            continue;
        }
        let known = declared
            .get(&mention.distro)
            .expect("every Distro variant has a declared set");
        assert!(
            known.contains(&mention.name),
            "`{}` tells a reader to install `{}`, but {} never declares it. \
             Correct nonexistent names (#209); declare package dependencies \
             in {}, or document a narrowly verified external-tool exception.",
            mention.source,
            mention.name,
            mention.distro.manifest(),
            mention.distro.manifest()
        );
    }
}

/// The manifests parse to something, so the guard above cannot pass by
/// finding nothing to compare against.
///
/// A parser that silently returns an empty set turns
/// [`documented_packages_are_declared_by_packaging`] into an assertion that
/// every documented name is missing — which fails, loudly, and would be
/// mistaken for a docs bug. Naming the failure here says which half broke.
#[test]
fn packaging_manifests_declare_the_names_they_are_read_for() {
    let declared = declared_names();

    for (distro, floor, must_hold) in [
        (Distro::Arch, 12usize, "facelock"),
        (Distro::Debian, 6, "facelock"),
        (Distro::Fedora, 6, "facelock"),
    ] {
        let names = &declared[&distro];
        assert!(
            names.len() >= floor,
            "{} parsed to only {} names ({names:?}) — the manifest parser is \
             broken, not the manifest",
            distro.manifest(),
            names.len()
        );
        assert!(
            names.contains(must_hold),
            "{} must declare `{must_hold}`",
            distro.manifest()
        );
    }

    let arch = &declared[&Distro::Arch];
    for gpu in ["onnxruntime-opt-cuda", "onnxruntime-opt-rocm"] {
        assert!(
            arch.contains(gpu),
            "dist/PKGBUILD lists `{gpu}` in optdepends; the optdepends parser \
             must reach it, or every GPU instruction fails this suite"
        );
    }
}

/// No unmanifested package manager reaches a reader.
///
/// Separated from the main guard so the message is the right one: a
/// `zypper install` instruction is not a wrong package name, it is a name
/// this tree has no way to check. Adding openSUSE packaging, or an entry in
/// [`KNOWN_EXTERNAL`], is the fix — not a looser check.
#[test]
fn no_documented_install_uses_an_unverifiable_package_manager() {
    for mention in documented_packages() {
        assert!(
            mention.distro != Distro::Unmanifested
                || KNOWN_EXTERNAL
                    .iter()
                    .any(|(distro, name, _)| *distro == mention.distro && *name == mention.name),
            "`{}` tells a reader to install `{}` with a package manager this \
             tree ships no manifest for, so nothing can confirm the name exists",
            mention.source,
            mention.name
        );
    }
}

#[test]
fn install_verbs_do_not_match_a_longer_flag() {
    assert!(install_mentions("pacman -Ss onnxruntime").is_empty());
    assert!(install_mentions("pacman -Qkk facelock").is_empty());
}

#[test]
fn source_prerequisite_install_forms_keep_every_package() {
    assert_eq!(
        install_mentions("sudo pacman -Syu --needed base-devel onnxruntime-cpu"),
        vec![
            (Distro::Arch, "base-devel".into()),
            (Distro::Arch, "onnxruntime-cpu".into()),
        ]
    );
}

#[test]
fn apt_target_suite_is_not_a_package() {
    assert_eq!(
        install_mentions("sudo apt install -t trixie-backports rustc cargo"),
        vec![
            (Distro::Debian, "rustc".into()),
            (Distro::Debian, "cargo".into()),
        ]
    );
}

#[test]
fn a_comment_can_carry_a_second_verb() {
    let found = install_mentions("yay -S facelock           # or paru -S facelock");
    assert_eq!(
        found,
        vec![
            (Distro::Arch, "facelock".to_string()),
            (Distro::Arch, "facelock".to_string()),
        ]
    );
}

#[test]
fn flags_are_skipped_and_comments_end_the_list() {
    let found = install_mentions("sudo pacman -S --needed onnxruntime-opt-cuda  # NVIDIA");
    assert_eq!(found, vec![(Distro::Arch, "onnxruntime-opt-cuda".into())]);
}

#[test]
fn a_chained_update_does_not_swallow_the_install() {
    let found = install_mentions("sudo apt update && sudo apt install facelock");
    assert_eq!(found, vec![(Distro::Debian, "facelock".into())]);
}

#[test]
fn an_inline_citation_keeps_its_name() {
    let found = install_mentions("Run `sudo dnf install facelock` to get it.");
    assert_eq!(found, vec![(Distro::Fedora, "facelock".into())]);
}

#[test]
fn table_cells_need_a_distro_column_and_a_bare_backticked_name() {
    let table = "\
| GPU Vendor | Package (Arch) | Config value |
|---|---|---|
| NVIDIA | `onnxruntime-opt-cuda` | `\"cuda\"` |
| Intel | none packaged | `\"openvino\"` |
";
    assert_eq!(
        table_mentions(table),
        vec![(Distro::Arch, "onnxruntime-opt-cuda".into())]
    );

    let unlabelled = "\
| Suite | Package |
|---|---|
| trixie | `facelock`, TPM enabled |
";
    assert!(table_mentions(unlabelled).is_empty());
}

#[test]
fn optdepends_descriptions_and_version_constraints_are_trimmed() {
    let pkgbuild = "\
pkgname=facelock
depends=('glibc' 'onnxruntime>=1.16')
optdepends=(
'onnxruntime-opt-cuda: NVIDIA GPU acceleration (replaces onnxruntime)'
)
";
    let names = pkgbuild_names(pkgbuild);
    assert!(names.contains("facelock"));
    assert!(names.contains("onnxruntime"));
    assert!(names.contains("onnxruntime-opt-cuda"));
}

#[test]
fn control_substvars_are_not_package_names() {
    let control = "\
Package: facelock
Depends: ${shlibs:Depends}, ${misc:Depends}, dbus, libpam-runtime
";
    let names = control_names(control);
    assert!(names.contains("facelock"));
    assert!(names.contains("libpam-runtime"));
    assert!(!names.iter().any(|n| n.contains('$')));
}
