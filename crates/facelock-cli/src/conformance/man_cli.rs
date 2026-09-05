//! Command-scoped roff coverage for the public clap tree.
use super::surface::{classification, command_tree, missing_arguments};

const PAGE: &str = include_str!("../../../../man/facelock.1");

fn plain(page: &str) -> String {
    super::unescape_roff(page)
        .replace(r"\fB", "")
        .replace(r"\fI", "")
        .replace(r"\fR", "")
        .replace(r"\fP", "")
}

fn section<'a>(page: &'a str, path: &str) -> Option<&'a str> {
    let heading = format!(".SS \"{path}\"\n");
    let alternate = format!(".SS {path}\n");
    let start = page
        .find(&heading)
        .map(|n| n + heading.len())
        .or_else(|| page.find(&alternate).map(|n| n + alternate.len()))?;
    let body = &page[start..];
    let end = [body.find("\n.SS "), body.find("\n.SH ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(body.len());
    Some(&body[..end])
}

#[test]
fn man_cli_covers_every_public_command_and_local_argument() {
    let root = command_tree();
    let mut commands = Vec::new();
    super::walk(&root, "", &mut commands);
    let page = plain(PAGE);
    let mut failures = Vec::new();
    for (path, command) in commands {
        if path == "facelock" || classification(&path, command) != "public" {
            continue;
        }
        let Some(body) = section(&page, &path) else {
            failures.push(format!("{path}: missing .SS command section"));
            continue;
        };
        for missing in missing_arguments(command, body, false) {
            failures.push(format!("{path}: missing {missing}"));
        }
    }
    let globals = page
        .split_once(".SH GLOBAL OPTIONS\n")
        .expect("GLOBAL OPTIONS")
        .1
        .split("\n.SH ")
        .next()
        .expect("global body");
    for missing in missing_arguments(&root, globals, true) {
        failures.push(format!("global: missing {missing}"));
    }
    assert!(
        failures.is_empty(),
        "man/facelock.1:\n{}",
        failures.join("\n")
    );
}

#[test]
fn man_sections_do_not_borrow_neighboring_flags() {
    let page = ".SS \"facelock first\"\nfirst\n.SS \"facelock second\"\n--json\n";
    assert!(
        !section(page, "facelock first")
            .expect("first")
            .contains("--json")
    );
    assert!(section(page, "facelock missing").is_none());
}
