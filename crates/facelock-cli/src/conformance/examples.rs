//! Consume the repository's classified shell inventory and parse only argv.
//! Invocations shown in documentation are never executed.
use std::sync::OnceLock;

use clap::Parser;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct Source {
    pub path: String,
    pub anchor: String,
    pub ordinal: usize,
    pub line: usize,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Segment {
    pub argv: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Occurrence {
    pub source: Source,
    pub raw: String,
    pub classification: String,
    pub segments: Vec<Segment>,
    pub reason: Option<String>,
    pub expected_error: Option<String>,
}

pub(super) fn occurrences() -> &'static [Occurrence] {
    static INVENTORY: OnceLock<Vec<Occurrence>> = OnceLock::new();
    INVENTORY.get_or_init(|| {
        #[derive(Deserialize)]
        struct Inventory {
            schema_version: u32,
            occurrences: Vec<Occurrence>,
            errors: Vec<String>,
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = std::process::Command::new("python3")
            .args(["test/docs-examples.py", "--json"])
            .current_dir(root)
            .output()
            .expect("run documentation inventory extractor (python3 required)");
        let inventory: Inventory = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid documentation inventory JSON: {error}; {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert!(
            output.status.success() && inventory.errors.is_empty(),
            "documentation inventory failed: {:?}; {}",
            inventory.errors,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(inventory.schema_version, 1, "unsupported inventory schema");
        assert!(
            inventory.occurrences.len() > 20,
            "inventory unexpectedly empty"
        );
        inventory.occurrences
    })
}

pub(super) fn executable_invocations() -> Vec<(&'static Source, &'static [String])> {
    occurrences()
        .iter()
        .filter(|entry| entry.classification == "executable")
        .flat_map(|entry| {
            entry
                .segments
                .iter()
                .filter(|segment| is_facelock(&segment.argv))
                .map(|segment| (&entry.source, segment.argv.as_slice()))
        })
        .collect()
}

fn is_facelock(argv: &[String]) -> bool {
    argv.first().is_some_and(|arg| {
        std::path::Path::new(arg)
            .file_name()
            .is_some_and(|name| name == "facelock")
    })
}

/// Inline syntax may omit required arguments, but every concrete command
/// name before the options/positionals must exist in the real command tree.
fn validate_command_prefix(argv: &[String], root: &clap::Command) -> Result<(), String> {
    validate_template_prefix(argv, root)?;
    if argv.iter().any(|token| syntax_template(token)) {
        return Ok(());
    }
    match root.clone().try_get_matches_from(argv) {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument
                    | clap::error::ErrorKind::MissingSubcommand
                    | clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(format!(
            "{:?}: {}",
            error.kind(),
            error
                .to_string()
                .lines()
                .next()
                .unwrap_or("parser rejected schematic syntax")
        )),
    }
}

fn syntax_template(text: &str) -> bool {
    text.contains(['<', '>', '[', ']', '|', '…'])
        || text.contains("...")
        || text.split_whitespace().any(|word| {
            word.chars().any(|c| c.is_ascii_uppercase())
                && word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
        })
}

fn validate_template_prefix(argv: &[String], root: &clap::Command) -> Result<(), String> {
    let mut command = root;
    let mut path = root.get_name().to_owned();
    let mut tokens = argv.iter().skip(1);
    while let Some(token) = tokens.next() {
        if command.get_subcommands().next().is_none() {
            break;
        }
        if token.starts_with('-') {
            let spelling = token.split('=').next().expect("option spelling");
            let option = command.get_arguments().find(|arg| {
                spelling.strip_prefix("--").is_some_and(|long| {
                    arg.get_long() == Some(long)
                        || arg.get_all_aliases().unwrap_or_default().contains(&long)
                }) || (!spelling.starts_with("--")
                    && spelling
                        .chars()
                        .nth(1)
                        .is_some_and(|short| arg.get_short() == Some(short)))
            });
            let Some(option) = option else {
                return Err(format!("{path}: unknown documented option {spelling}"));
            };
            if !token.contains('=') {
                for _ in 0..option
                    .get_num_args()
                    .map(|range| range.min_values())
                    .unwrap_or(0)
                {
                    tokens.next();
                }
            }
            continue;
        }
        if token.contains(['<', '>', '[', ']', '|', '/', '…']) || token == "..." {
            break; // Explicit syntax metavariables/alternatives, not concrete argv.
        }
        let Some(child) = command.find_subcommand(token) else {
            return Err(format!("{path}: unknown documented command {token:?}"));
        };
        path.push(' ');
        path.push_str(child.get_name());
        command = child;
    }
    Ok(())
}

#[test]
fn schematic_command_prefix_rejects_invented_nested_verbs() {
    let root = super::surface::command_tree();
    let argv = |words: &[&str]| {
        words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };
    assert!(validate_command_prefix(&argv(&["facelock", "nonexistent"]), &root).is_err());
    assert!(validate_command_prefix(&argv(&["facelock", "pam", "nonexistent"]), &root).is_err());
    assert!(
        validate_command_prefix(&argv(&["facelock", "--quiet", "nonexistent"]), &root).is_err()
    );
    assert!(validate_command_prefix(&argv(&["facelock", "--bogus", "status"]), &root).is_err());
    assert!(validate_command_prefix(&argv(&["facelock", "auth"]), &root).is_ok());
    assert!(validate_command_prefix(&argv(&["facelock", "remove", "<MODEL_ID>"]), &root).is_ok());
    assert!(
        validate_command_prefix(&argv(&["facelock", "tpm", "seal-key|unseal-key"]), &root).is_ok()
    );
}

#[test]
fn schematic_documentation_names_real_commands() {
    let root = super::surface::command_tree();
    let mut failures = Vec::new();
    for entry in occurrences()
        .iter()
        .filter(|entry| entry.classification == "schematic")
    {
        for segment in &entry.segments {
            if !is_facelock(&segment.argv) {
                continue;
            }
            let result = if syntax_template(&entry.raw) {
                validate_template_prefix(&segment.argv, &root)
            } else {
                validate_command_prefix(&segment.argv, &root)
            };
            if let Err(error) = result {
                failures.push(format!(
                    "{}:{}: {error}",
                    entry.source.path, entry.source.line
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "schematic command drift:\n{}",
        failures.join("\n")
    );
}

#[test]
fn classified_documentation_invocations_match_the_parser() {
    let mut parsed = 0;
    let mut failures = Vec::new();
    for entry in occurrences() {
        let source = &entry.source;
        assert!(
            source.line > 0 && source.ordinal > 0 && source.sha256.len() == 64,
            "invalid source identity: {source:?}"
        );
        match entry.classification.as_str() {
            "executable" => {}
            "negative" if entry.expected_error.is_some() => {}
            "schematic" | "negative" | "historical" | "manual" => {
                assert!(
                    entry
                        .reason
                        .as_ref()
                        .is_some_and(|reason| !reason.trim().is_empty()),
                    "{}:{}: skipped example needs a reason",
                    source.path,
                    source.line
                );
                continue;
            }
            other => panic!("unknown example classification {other:?}"),
        }
        for segment in &entry.segments {
            if !is_facelock(&segment.argv) {
                continue;
            }
            parsed += 1;
            let result = crate::Cli::try_parse_from(&segment.argv);
            let valid = match (&entry.expected_error, result) {
                (Some(expected), Err(error)) => format!("{:?}", error.kind()) == *expected,
                (None, Ok(_)) => true,
                (None, Err(error))
                    if matches!(
                        error.kind(),
                        clap::error::ErrorKind::DisplayHelp
                            | clap::error::ErrorKind::DisplayVersion
                    ) =>
                {
                    true
                }
                _ => false,
            };
            if !valid {
                failures.push(format!(
                    "{}:{} #{} occurrence {}: {:?} (expected {:?})",
                    source.path,
                    source.line,
                    source.anchor,
                    source.ordinal,
                    segment.argv,
                    entry.expected_error
                ));
            }
        }
    }
    assert!(
        parsed > 20,
        "too few facelock invocations reached clap: {parsed}"
    );
    assert!(
        failures.is_empty(),
        "documentation argv drift:\n{}",
        failures.join("\n")
    );
}
