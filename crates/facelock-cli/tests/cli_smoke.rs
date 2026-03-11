//! Smoke tests for the facelock CLI binary.

use std::process::Command;

fn facelock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_facelock"))
}

#[test]
fn help_exits_successfully() {
    let output = facelock_bin()
        .arg("--help")
        .output()
        .expect("failed to execute facelock --help");

    assert!(
        output.status.success(),
        "facelock --help should exit 0, got: {}",
        output.status
    );
}

#[test]
fn version_exits_successfully() {
    let output = facelock_bin()
        .arg("--version")
        .output()
        .expect("failed to execute facelock --version");

    assert!(
        output.status.success(),
        "facelock --version should exit 0, got: {}",
        output.status
    );
}

#[test]
fn version_output_contains_package_name() {
    let output = facelock_bin()
        .arg("--version")
        .output()
        .expect("failed to execute facelock --version");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("facelock"),
        "version output should contain 'facelock', got: {stdout}"
    );
}

#[test]
fn help_output_contains_expected_subcommands() {
    let output = facelock_bin()
        .arg("--help")
        .output()
        .expect("failed to execute facelock --help");

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
    let output = facelock_bin()
        .output()
        .expect("failed to execute facelock with no args");

    // clap with required subcommand exits non-zero when no subcommand is given
    assert!(
        !output.status.success(),
        "facelock with no args should exit non-zero"
    );

    // Should show some usage information on stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Usage") || stderr.contains("usage") || stderr.contains("facelock"),
        "stderr should contain usage info, got: {stderr}"
    );
}
