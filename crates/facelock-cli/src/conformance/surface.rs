//! Test-only parser inventory. No command dispatch or hardware access occurs here.
use clap::CommandFactory;
use serde_json::{Value, json};

pub(super) fn command_tree() -> clap::Command {
    let mut root = crate::Cli::command();
    root.build();
    root
}

pub(super) fn classification(path: &str, command: &clap::Command) -> &'static str {
    if path.split_whitespace().any(|part| part == "help") {
        "help"
    } else if command.is_hide_set() {
        "internal"
    } else {
        "public"
    }
}

fn inventory(root: &clap::Command) -> Value {
    let mut commands = Vec::new();
    super::walk(root, "", &mut commands);
    json!({
        "schema_version": 1,
        "binary": root.get_name(),
        "commands": commands.into_iter().map(|(path, command)| {
            json!({
                "path": path.split_whitespace().collect::<Vec<_>>(),
                "classification": classification(&path, command),
                "aliases": command.get_all_aliases().collect::<Vec<_>>(),
                "subcommand_required": command.is_subcommand_required_set(),
                "arguments": command.get_arguments().map(|arg| json!({
                    "id": arg.get_id().as_str(),
                    "long": arg.get_long(),
                    "short": arg.get_short(),
                    "aliases": arg.get_all_aliases().unwrap_or_default(),
                    "short_aliases": arg.get_all_short_aliases().unwrap_or_default(),
                    "required": arg.is_required_set(),
                    "global": arg.is_global_set(),
                    "hidden": arg.is_hide_set(),
                    "action": format!("{:?}", arg.get_action()),
                    "defaults": arg.get_default_values().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>(),
                    "value_names": arg.get_value_names().map(|names| names.iter().map(|name| name.as_str()).collect::<Vec<_>>()).unwrap_or_default(),
                    "possible_values": arg.get_value_parser().possible_values().map(|values| values.map(|value| value.get_name().to_owned()).collect::<Vec<_>>()).unwrap_or_default(),
                    "num_args": arg.get_num_args().map(|range| json!({"min":range.min_values(),"max":range.max_values()})),
                    "conflicts": command.get_arg_conflicts_with(arg).iter().map(|arg| arg.get_id().as_str()).collect::<Vec<_>>(),
                })).collect::<Vec<_>>()
            })
        }).collect::<Vec<_>>()
    })
}

#[test]
fn export_cli_surface_if_requested() {
    let surface = inventory(&command_tree());
    assert!(surface["commands"].as_array().expect("command array").len() > 40);
    if let Some(path) = std::env::var_os("FACELOCK_DOCS_SURFACE_OUTPUT") {
        let path = std::path::Path::new(&path);
        assert!(
            path.is_absolute(),
            "FACELOCK_DOCS_SURFACE_OUTPUT must be an explicit absolute file path"
        );
        // The caller supplies an artifact location; normal test runs write nothing.
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&surface).expect("serialize inventory"),
        )
        .expect("write FACELOCK_DOCS_SURFACE_OUTPUT");
    }
}

#[test]
fn inventory_preserves_nested_constraints_and_aliases() {
    let surface = inventory(&command_tree());
    let commands = surface["commands"].as_array().expect("commands");
    let find = |path: &[&str]| {
        commands
            .iter()
            .find(|command| command["path"] == json!(path))
            .expect("command exists")
    };
    let setup = find(&["facelock", "setup"]);
    let yes = setup["arguments"]
        .as_array()
        .expect("arguments")
        .iter()
        .find(|arg| arg["id"] == "yes")
        .expect("yes flag");
    assert!(
        yes["aliases"]
            .as_array()
            .expect("aliases")
            .contains(&json!("no-confirm"))
    );
    let auth = find(&["facelock", "auth"]);
    assert!(
        auth["arguments"]
            .as_array()
            .expect("arguments")
            .iter()
            .any(|arg| arg["id"] == "user" && arg["required"] == true)
    );
    assert_eq!(find(&["facelock", "help"])["classification"], "help");
    assert!(
        commands
            .iter()
            .any(|command| command["classification"] == "internal")
    );
}

/// Tokens, not substrings: `--user-name` cannot document `--user`.
pub(super) fn contains_token(text: &str, token: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .any(|word| word == token || (!token.starts_with('-') && word.eq_ignore_ascii_case(token)))
}

pub(super) fn missing_arguments(command: &clap::Command, body: &str, globals: bool) -> Vec<String> {
    let mut missing = Vec::new();
    for alias in command.get_visible_aliases() {
        if !contains_token(body, alias) {
            missing.push(format!("command alias {alias}"));
        }
    }
    for arg in command.get_arguments() {
        if arg.is_hide_set()
            || matches!(arg.get_id().as_str(), "help" | "version")
            || (arg.is_global_set() && !globals)
        {
            continue;
        }
        let mut names = Vec::new();
        if let Some(long) = arg.get_long() {
            names.push(format!("--{long}"));
        }
        if let Some(short) = arg.get_short() {
            names.push(format!("-{short}"));
        }
        for alias in arg.get_visible_aliases().unwrap_or_default() {
            names.push(format!("--{alias}"));
        }
        for alias in arg.get_visible_short_aliases().unwrap_or_default() {
            names.push(format!("-{alias}"));
        }
        if arg.get_long().is_none() && arg.get_short().is_none() {
            names.extend(
                arg.get_value_names()
                    .map(|names| names.iter().map(ToString::to_string).collect())
                    .unwrap_or_else(|| vec![arg.get_id().to_string()]),
            );
        }
        missing.extend(names.into_iter().filter(|name| !contains_token(body, name)));
    }
    missing
}

#[test]
fn flag_coverage_does_not_accept_another_flag_prefix() {
    assert!(!contains_token("`--user-name`", "--user"));
    assert!(!contains_token("`--USER`", "--user"));
    assert!(contains_token("`--user <NAME>`", "--user"));
}

#[test]
fn inventory_walks_beyond_two_levels_and_retains_constraints() {
    let mut root = clap::Command::new("fixture").subcommand(
        clap::Command::new("one").subcommand(
            clap::Command::new("two").subcommand(
                clap::Command::new("three")
                    .arg(
                        clap::Arg::new("choice")
                            .long("choice")
                            .required(true)
                            .value_parser(["a", "b"]),
                    )
                    .arg(clap::Arg::new("left").long("left").conflicts_with("right"))
                    .arg(clap::Arg::new("right").long("right")),
            ),
        ),
    );
    root.build();
    let surface = inventory(&root);
    let leaf = surface["commands"]
        .as_array()
        .expect("commands")
        .iter()
        .find(|command| command["path"] == json!(["fixture", "one", "two", "three"]))
        .expect("deeply nested command");
    let args = leaf["arguments"].as_array().expect("arguments");
    assert!(args.iter().any(|arg| arg["id"] == "choice"
        && arg["possible_values"] == json!(["a", "b"])
        && arg["required"] == true));
    assert!(
        args.iter()
            .any(|arg| arg["id"] == "left" && arg["conflicts"] == json!(["right"]))
    );
}
