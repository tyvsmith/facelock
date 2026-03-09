//! Smoke tests for the howdy CLI binary.

use std::process::Command;

fn howdy_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_howdy"))
}

#[test]
fn help_exits_successfully() {
    let output = howdy_bin()
        .arg("--help")
        .output()
        .expect("failed to execute howdy --help");

    assert!(
        output.status.success(),
        "howdy --help should exit 0, got: {}",
        output.status
    );
}

#[test]
fn version_exits_successfully() {
    let output = howdy_bin()
        .arg("--version")
        .output()
        .expect("failed to execute howdy --version");

    assert!(
        output.status.success(),
        "howdy --version should exit 0, got: {}",
        output.status
    );
}

#[test]
fn version_output_contains_package_name() {
    let output = howdy_bin()
        .arg("--version")
        .output()
        .expect("failed to execute howdy --version");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("howdy"),
        "version output should contain 'howdy', got: {stdout}"
    );
}

#[test]
fn help_output_contains_expected_subcommands() {
    let output = howdy_bin()
        .arg("--help")
        .output()
        .expect("failed to execute howdy --help");

    let stdout = String::from_utf8_lossy(&output.stdout);

    let expected_subcommands = [
        "setup", "enroll", "remove", "clear", "list", "test", "preview", "config", "status",
        "devices",
    ];

    for subcmd in &expected_subcommands {
        assert!(
            stdout.to_lowercase().contains(subcmd),
            "help output should mention subcommand '{subcmd}', got:\n{stdout}"
        );
    }
}

#[test]
fn no_args_shows_error_or_help() {
    let output = howdy_bin()
        .output()
        .expect("failed to execute howdy with no args");

    // clap with required subcommand exits non-zero when no subcommand is given
    assert!(
        !output.status.success(),
        "howdy with no args should exit non-zero"
    );

    // Should show some usage information on stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Usage") || stderr.contains("usage") || stderr.contains("howdy"),
        "stderr should contain usage info, got: {stderr}"
    );
}
