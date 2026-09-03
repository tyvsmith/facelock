//! Smoke tests for the facelock CLI binary.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn facelock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_facelock"))
}

/// The uid/gid a root test runner drops the child to. `setuid`/`setgid` do
/// not consult `/etc/passwd`, so the account need not exist inside a CI
/// container; 65534 is `nobody` on the distributions where it does.
const NOBODY_UID: u32 = 65534;
const NOBODY_GID: u32 = 65534;

/// Make `command` run unprivileged whatever the test runner's own uid is:
/// under a root runner it drops to `nobody` before `exec`. Returns whether
/// the drop was applied, which only the failure hint needs.
///
/// Every row in this file that spawns a process goes through here, including
/// the two pty rows. Rows used to return early under a root runner instead,
/// which is how this contract ended up with no CI coverage at all — both CI
/// test jobs run `cargo test` as root in a container, so a skipping row was a
/// no-op that reported as a pass (issues #189, #303).
///
/// Dropping cannot regress the way skipping did: `setuid(2)` from root
/// replaces the saved uid as well, and the standard library clears root's
/// supplementary groups ahead of it, so a row that runs at all ran
/// unprivileged. No environment variable, workflow step, or runner user
/// decides that.
///
/// A pty survives the drop. Both ends are opened by the parent, before the
/// fork, so the kernel's permission check on the device is already done and
/// the child inherits open descriptors rather than reopening them by path.
/// `openpty` chowning the device to the invoking user therefore does not
/// keep the dropped child from reading its own queued input.
fn run_unprivileged(command: &mut Command) -> bool {
    if nix::unistd::Uid::effective().is_root() {
        command.uid(NOBODY_UID).gid(NOBODY_GID);
        return true;
    }
    false
}

/// What to add to a spawn failure when the drop was applied: at uid 65534 the
/// child must be able to reach and execute the binary it was pointed at.
fn spawn_hint(dropped_privileges: bool) -> String {
    if !dropped_privileges {
        return String::new();
    }
    format!(
        "\nA root test runner runs this row as uid {NOBODY_UID}, so the built \
         binary and every directory above it must be traversable and executable \
         by other users — mode 0755, not 0700."
    )
}

/// DEC-6/C6 contract: every root-required command must refuse *before*
/// emitting any prompt text or touching state, when invoked non-root with no
/// TTY attached. Closing stdin (rather than leaving it inherited) forces
/// `require_root`'s non-interactive branch: `isatty(0)` is false on a closed
/// pipe, so it hard-errors instead of offering to re-exec with sudo — which
/// would otherwise hang waiting for input that never arrives.
///
/// Closing stdin is also the limit of what these rows witness: they prove a
/// refusal preceded the output, not *which* escalation class produced it.
/// `require_root` and `require_root_scripted` take that same non-interactive
/// branch and are indistinguishable here. Pinning the prompt-versus-hard-error
/// split needs a row that allocates a real pty, which is a separate concern.
///
/// The child always runs unprivileged, whatever the test runner's own uid is:
/// see [`run_unprivileged`], which every spawning row in this file goes
/// through.
fn assert_refuses_before_output(args: &[&str], forbidden_substrings: &[&str]) {
    let mut command = facelock_bin();
    command.args(args).stdin(Stdio::null());
    let dropped_privileges = run_unprivileged(&mut command);

    let output = command.output().unwrap_or_else(|e| {
        panic!(
            "failed to execute facelock {args:?}: {e}{}",
            spawn_hint(dropped_privileges)
        );
    });

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("Root required"),
        "facelock {args:?} should refuse with 'Root required', got:\nstdout: {stdout}\nstderr: {stderr}"
    );

    for forbidden in forbidden_substrings {
        assert!(
            !stdout.contains(forbidden) && !stderr.contains(forbidden),
            "facelock {args:?} must refuse BEFORE emitting {forbidden:?} (C6 ordering), \
             got:\nstdout: {stdout}\nstderr: {stderr}"
        );
    }
}

#[test]
fn status_refuses_before_root_non_root() {
    assert_refuses_before_output(&["status"], &["facelock system status"]);
}

#[test]
fn devices_refuses_before_root_non_root() {
    assert_refuses_before_output(
        &["devices"],
        &["Available video devices", "No video devices found"],
    );
}

#[test]
fn preview_refuses_before_root_non_root() {
    assert_refuses_before_output(
        &["preview", "--text-only"],
        &["Graphical preview", "text-only mode"],
    );
}

#[test]
fn test_cmd_refuses_before_root_non_root() {
    assert_refuses_before_output(
        &["test", "--user", "nonexistent-test-user"],
        &["Testing face recognition", "No face models enrolled"],
    );
}

#[test]
fn audit_refuses_before_root_non_root() {
    // audit is scripted/hard-error only (never an interactive prompt), but
    // the non-interactive refusal path is identical either way.
    assert_refuses_before_output(&["audit"], &["Audit logging"]);
}

#[test]
fn bench_refuses_before_root_non_root() {
    assert_refuses_before_output(&["bench", "cold-auth"], &["cold auth", "Cold Auth"]);
}

#[test]
fn config_edit_refuses_before_root_non_root() {
    assert_refuses_before_output(&["config", "edit"], &["Config file:", "Config saved"]);
}

// The four commands ADR 009 moved under `daemon` and `tpm`. None of them had a
// C6 row under its old top-level spelling — a pre-existing gap, closed here
// because the rename is the moment their argv changes anyway. Each asserts the
// same thing as the rows above: the root check runs ahead of the command's
// first byte of output, not after it.

#[test]
fn daemon_restart_refuses_before_root_non_root() {
    assert_refuses_before_output(
        &["daemon", "restart"],
        &["Daemon restarted.", "Daemon shutdown requested"],
    );
}

#[test]
fn tpm_encrypt_refuses_before_root_non_root() {
    assert_refuses_before_output(
        &["tpm", "encrypt"],
        &[
            "Proceeding to encrypt embeddings",
            "All embeddings are already encrypted",
            "Encrypting ",
        ],
    );
}

#[test]
fn tpm_decrypt_refuses_before_root_non_root() {
    assert_refuses_before_output(
        &["tpm", "decrypt"],
        &["No encrypted embeddings found", "Decrypting "],
    );
}

#[test]
fn tpm_reseal_refuses_before_root_non_root() {
    // The method check is the first thing `run_reseal` does (the root check
    // runs in main's dispatch gate, before the config parse), so its refusal
    // text is the tightest ordering witness available.
    assert_refuses_before_output(
        &["tpm", "reseal"],
        &[
            "only applies when encryption.method",
            "no sealed key found at",
            "Unsealed the current key",
        ],
    );
}

#[test]
fn daemon_run_refuses_before_root_non_root() {
    // The tracing line `run` emits right after the root check is the tightest
    // ordering witness available: the subscriber is not even installed until
    // the check has passed.
    assert_refuses_before_output(&["daemon", "run"], &["facelock daemon starting"]);
}

/// Open a pty pair, returning `(controller, device)` — the ends historically
/// called master and slave.
fn open_pty() -> (std::fs::File, std::fs::File) {
    let mut controller = -1;
    let mut device = -1;
    // SAFETY: both fds are out-params written before `openpty` returns 0, and
    // the null `name`/`termp`/`winp` arguments each mean "default" per
    // openpty(3).
    let rc = unsafe {
        libc::openpty(
            &mut controller,
            &mut device,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());

    // SAFETY: `openpty` returned 0, so both are fresh owned fds and nothing
    // else holds them.
    unsafe {
        (
            std::os::fd::FromRawFd::from_raw_fd(controller),
            std::os::fd::FromRawFd::from_raw_fd(device),
        )
    }
}

/// DEC-6: `daemon run` is the **hard error only** escalation class — it never
/// offers `Re-run with sudo? [Y/n]`, even with a terminal attached, because
/// every shipped service unit invokes it and a unit has nobody to answer
/// (issue #188).
///
/// This needs a real pty. The `assert_refuses_before_output` rows above close
/// stdin, which drives `require_root` down its *non-interactive* branch too —
/// so they pass under either escalation class and cannot tell them apart. A
/// regression to `require_root` here does not fail fast: it blocks forever on
/// a prompt nobody will answer, which is why the wait is bounded and a
/// timeout is the assertion.
///
/// Like every other row here it drops to `nobody` under a root runner rather
/// than returning early. As root `daemon run` would start the daemon instead
/// of refusing, so skipping was the only alternative — and skipping is what
/// left this row with no CI coverage (issue #303).
#[test]
fn daemon_run_never_prompts_with_a_tty_attached() {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let (mut controller, device) = open_pty();
    let mut command = facelock_bin();
    command
        .args(["daemon", "run"])
        .stdin(Stdio::from(device.try_clone().expect("dup pty device")))
        .stdout(Stdio::from(device.try_clone().expect("dup pty device")))
        .stderr(Stdio::from(device));
    let dropped_privileges = run_unprivileged(&mut command);

    let spawned = command.spawn();
    // Drop the parent's copies of the device fds now that the child holds
    // them. `Command` owns its `Stdio` handles until it is itself dropped, so
    // a `Command` that outlives the spawn keeps a write end of the device
    // open here and `read_to_end` on the controller below never sees EOF.
    drop(command);
    let mut child = spawned.unwrap_or_else(|e| {
        panic!(
            "failed to spawn facelock daemon run: {e}{}",
            spawn_hint(dropped_privileges)
        )
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait().expect("try_wait on facelock daemon run") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`facelock daemon run` never exited as a non-root user with a TTY \
                     attached: it is blocked on the interactive sudo prompt, which the \
                     hard-error class forbids (expected require_root_scripted)"
                );
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    // Reading the controller ends in EIO once the last device fd closes;
    // bytes already read are kept, so the error is the end of the stream.
    let mut raw = Vec::new();
    let _ = controller.read_to_end(&mut raw);
    let out = String::from_utf8_lossy(&raw);

    assert!(!status.success(), "should refuse non-root, got: {status}");
    assert!(out.contains("Root required"), "got: {out}");
    // Asserted without the preceding newline: the pty translates it to CRLF.
    assert!(out.contains("Run: sudo facelock daemon run"), "got: {out}");
    assert!(
        !out.contains("Re-run with sudo"),
        "`daemon run` must not offer the interactive re-exec, got: {out}"
    );
    assert!(
        !out.contains("facelock daemon starting"),
        "the root check must run before anything else, got: {out}"
    );
}

#[test]
fn enroll_refuses_before_setup_prompt_non_root() {
    // C6: `enroll` was the one gated spelling with no row at all — its
    // ordering rested entirely on the gate's exhaustive match in `main`, so
    // nothing failed if the arm went missing (issue #288).
    //
    // `--config` at a path that does not exist is what makes this row bite,
    // and it is the one difference from the rows above. `enroll::run` now
    // re-checks root itself, as a backstop for the callers inside `setup`
    // that never reach the gate — so with a readable config on the machine,
    // a deleted gate arm would still produce "Root required", just one parse
    // too late. Pointing at nothing means only the gate can answer first:
    // reaching the parse renders a config error instead (issue #191).
    //
    // Which branch `enroll` opens with depends on `/etc/facelock/.setup-complete`,
    // a property of the machine running the test, so the forbidden list
    // covers both: the marker-absent branch prints "Setup has not been
    // completed." and asks to run setup, the marker-present one goes to the
    // encryption posture check and the model probe.
    assert_refuses_before_output(
        &[
            "--config",
            "/nonexistent/facelock-enroll-c6.toml",
            "enroll",
            "--user",
            "nonexistent-test-user",
        ],
        &[
            "Setup has not been completed",
            "Run setup now?",
            "WARNING: encryption.method",
            "Face recognition models not found",
            "Enrolling face for user",
        ],
    );
}

#[test]
fn setup_systemd_under_config_override_refuses_root_first_non_root() {
    // #314: `--systemd` under a non-default `--config` is refused, because
    // the packaged unit reads only the default file. That refusal sits
    // *behind* the root gate, so an unprivileged caller hears "Root
    // required" and nothing else: the identity diagnosis, the base flow's
    // first line and the unit narration are all forbidden here. The refusal
    // itself is witnessed as root by tier 3a (test/run-camera-free-tests.sh)
    // and by the setup unit tests.
    assert_refuses_before_output(
        &[
            "--config",
            "/nonexistent/facelock-setup-override-c6.toml",
            "setup",
            "--systemd",
            "--non-interactive",
        ],
        &[
            "--systemd is not supported with --config",
            "facelock setup: preparing system",
            "Validating installed facelock-daemon",
        ],
    );
}

/// DEC-6: `enroll` is the **interactive** escalation class — with a terminal
/// attached it offers `Re-run with sudo? [Y/n]` before it refuses, the
/// opposite of `daemon run` above.
///
/// The stdin-closed rows cannot witness that. `require_root` and
/// `require_root_scripted` share the non-interactive branch, so silently
/// downgrading `enroll`'s arm to the hard-error class passes every one of
/// them. Only a real pty separates the two (issue #288).
///
/// `--skip-setup-check` pins the hint: without it `sudo_hint` names `setup`
/// or `enroll` depending on whether the setup marker exists on the machine
/// running the test, and the assertion below would be reading the host
/// rather than the code.
///
/// `--config` points at nothing for the same reason the row above does: the
/// backstop inside `enroll::run` must not be able to answer for the gate.
///
/// Under a root runner it drops to `nobody` like every other row rather than
/// returning early: as root `enroll` opens the camera instead of refusing,
/// and skipping is what left this row with no CI coverage (issue #303).
///
/// The child is given an empty `PATH` so this row can never escalate for
/// real. The answer written to the pty is "n", but if the parser ever read
/// that as consent, `Command::new("sudo")` fails to spawn instead of
/// enrolling a face as root — and the assertions below fail loudly rather
/// than the row passing by accident.
#[test]
fn enroll_offers_the_sudo_re_exec_with_a_tty_attached() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    let (mut controller, device) = open_pty();
    // Queued in the tty's input buffer before the child exists, so the prompt
    // cannot race ahead of the answer and read EOF — which
    // `confirm_default_yes` would take as the default *yes*.
    controller.write_all(b"n\n").expect("write answer to pty");

    let mut command = facelock_bin();
    command
        .args([
            "--config",
            "/nonexistent/facelock-enroll-c6.toml",
            "enroll",
            "--user",
            "nonexistent-test-user",
            "--skip-setup-check",
        ])
        .env("PATH", "")
        .stdin(Stdio::from(device.try_clone().expect("dup pty device")))
        .stdout(Stdio::from(device.try_clone().expect("dup pty device")))
        .stderr(Stdio::from(device));
    let dropped_privileges = run_unprivileged(&mut command);

    let spawned = command.spawn();
    // Drop the parent's copies of the device fds now that the child holds
    // them. `Command` owns its `Stdio` handles until it is itself dropped, so
    // a `Command` that outlives the spawn keeps a write end of the device
    // open here and `read_to_end` on the controller below never sees EOF.
    drop(command);
    let mut child = spawned.unwrap_or_else(|e| {
        panic!(
            "failed to spawn facelock enroll: {e}{}",
            spawn_hint(dropped_privileges)
        )
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait().expect("try_wait on facelock enroll") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`facelock enroll` never exited as a non-root user with a TTY attached");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let mut raw = Vec::new();
    let _ = controller.read_to_end(&mut raw);
    let out = String::from_utf8_lossy(&raw);

    assert!(!status.success(), "should refuse non-root, got: {status}");
    assert!(
        out.contains("Re-run with sudo"),
        "`enroll` must offer the interactive re-exec on a TTY (expected \
         require_root, not require_root_scripted), got: {out}"
    );
    // Asserted without the preceding newline: the pty translates it to CRLF.
    assert!(out.contains("Run: sudo facelock enroll"), "got: {out}");
    assert!(
        !out.contains("failed to execute sudo"),
        "declining the prompt must refuse, never escalate, got: {out}"
    );
    assert!(
        !out.contains("Enrolling face for user"),
        "the root check must run before anything else, got: {out}"
    );
}

#[test]
fn list_refuses_before_root_non_root() {
    assert_refuses_before_output(
        &["list", "--user", "nonexistent-test-user"],
        &["Face models for user", "No face models enrolled"],
    );
}

#[test]
fn remove_refuses_before_confirmation_prompt_non_root() {
    // C6: historically remove.rs asked for Y/N confirmation *before* checking
    // root. Pin that the root check now runs first — the confirmation prompt
    // text must never appear.
    assert_refuses_before_output(
        &["remove", "1", "--user", "nonexistent-test-user"],
        &["Remove face model #1"],
    );
}

#[test]
fn clear_refuses_before_confirmation_prompt_non_root() {
    assert_refuses_before_output(
        &["clear", "--user", "nonexistent-test-user"],
        &["Remove ALL face models", "No face models enrolled"],
    );
}

#[test]
fn pam_add_refuses_before_touching_pam_d_non_root() {
    // C6: the root check is the first statement in `commands::pam::run`, ahead
    // of the module-presence check, the plan, and `--dry-run`. A dry run that
    // succeeded unprivileged would be a preview of a command that cannot run.
    assert_refuses_before_output(
        &["pam", "add", "--service", "sudo", "--dry-run"],
        &[
            "About to modify",
            "Would add the facelock PAM line",
            "==> facelock PAM line",
        ],
    );
}

#[test]
fn pam_remove_refuses_before_touching_pam_d_non_root() {
    assert_refuses_before_output(
        &["pam", "remove", "--service", "sudo", "--dry-run"],
        &[
            "Would remove the facelock PAM line",
            "Removed facelock PAM line",
        ],
    );
}

#[test]
fn tpm_status_refuses_before_opening_database_non_root() {
    // `tpm status` had no root check at all, so a non-root caller reached
    // `FaceStore::open_readonly` on the root-only database and got a sqlite
    // "unable to open database file" error instead of the refusal. The
    // forbidden list includes that error text: reaching it means the check
    // is gone again, not merely late.
    assert_refuses_before_output(
        &["tpm", "status"],
        &["TPM Status", "TPM device", "failed to open face database"],
    );
}

#[test]
fn tpm_pcr_baseline_refuses_before_output_non_root() {
    // `tpm pcr-baseline` also had no root check: it printed its header and
    // the PCR values and exited 0 for any user. It now refuses and exits 1.
    assert_refuses_before_output(
        &["tpm", "pcr-baseline"],
        &["PCR Baseline", "TPM support not compiled in"],
    );
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

/// `capabilities` reads no file at all, so a `--config` pointing at nothing
/// must not change its answer.
///
/// The D7 dispatch order is what makes that true — `capabilities` is handled
/// ahead of `ConfigLoad::read()` — and only a spawned process can witness it.
/// `capability_names_are_all_implemented` proves each name is backed by a
/// surface, but it calls `Cli::command()` in-process and would keep passing if
/// the command started loading a config and exiting 1 on a missing one, which
/// is the failure a probing wrapper actually hits.
///
/// Asserted against `CAPABILITIES` rather than a hand-picked name or two: an
/// empty stdout also exits 0, and would tell that wrapper this build can do
/// nothing.
#[test]
fn capabilities_ignores_a_missing_config_file() {
    let output = facelock_bin()
        .args([
            "--config",
            "/nonexistent/facelock-smoke.toml",
            "capabilities",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("failed to execute facelock capabilities");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "`facelock --config /nonexistent/... capabilities` must exit 0, got {}:\n\
         stdout: {stdout}\nstderr: {stderr}",
        output.status
    );

    for name in facelock_cli::commands::capabilities::CAPABILITIES {
        assert!(
            stdout.lines().any(|line| line == *name),
            "capabilities must print `{name}` on a line of its own, got:\n{stdout}"
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

#[test]
fn data_purge_refuses_before_confirmation_prompt_non_root() {
    // C6: the root check is the first statement in `commands::data::run`,
    // ahead of the destruction authorization, the confirmation prompt, and
    // the lifecycle lease. A non-root caller must never reach the question
    // "this permanently destroys ..." for an operation that will be refused,
    // and must never cause the daemon to be stopped on the way to finding
    // out. The forbidden list therefore covers the prompt, the authorization
    // refusal, and the report headings.
    assert_refuses_before_output(
        &["data", "purge", "--allow-destruction", "--yes"],
        &[
            "This permanently destroys",
            "Refusing to destroy biometric data",
            "Removed",
            "were retained",
            "Removing a name is not erasure",
        ],
    );
}

#[test]
fn data_purge_dry_run_refuses_before_reporting_non_root() {
    // The root check runs ahead of `--dry-run` too, the way `pam add`'s
    // does: a dry run that succeeded unprivileged would be a preview of a
    // command that cannot run, and it reads root-only state to build it.
    assert_refuses_before_output(
        &["data", "purge", "--dry-run"],
        &["Dry run", "Removing a name is not erasure", "were retained"],
    );
}
