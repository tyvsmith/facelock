# `facelock setup --pam --remove --if-present` Design

## Purpose

Issue #148 asks for an opt-in idempotent removal mode for PAM service files
that may not exist. Today, `facelock setup --pam --remove` exits successfully
when the service file exists but contains no Facelock line, yet errors when the
service file itself is absent. `--if-present` makes only that absent-file case a
successful no-op while preserving the current default and all other failures.

## Scope

- Add `--if-present` to `facelock setup`, valid only with `--remove` (and thus
  transitively with `--pam`).
- Carry the flag through the existing setup argument, plan, and PAM dispatch
  types without changing unrelated setup precedence.
- Add a temp-directory seam for PAM removal, mirroring `pam_install_in`.
- Report the skipped absent-service case through the typed PAM message system.
- Add CLI, plan-resolution, removal-behavior, message-enumeration, help,
  changelog, and contract coverage.

## Non-goals

- Do not make `--remove` unconditionally idempotent. Without `--if-present`, a
  missing service file remains an error for compatibility with existing
  callers.
- Do not change `pam_install` or its missing-service-file refusal.
- Do not delete PAM service files or their owning project's content. Facelock
  removes only active lines that reference `pam_facelock.so`.
- Do not broaden privilege behavior, modify PAM authentication semantics, or
  touch the `pam-facelock` crate and its dependency boundary.
- Do not test against or modify the host's `/etc/pam.d`.

## CLI and Data Flow

`Commands::Setup` gains an `if_present: bool` field next to `remove`, exposed as
`--if-present` with `requires = "remove"`. Clap must also continue enforcing
`remove`'s existing `requires = "pam"`; parser tests and `Cli::debug_assert()`
will verify the composed relationship. The setup help text will describe the
flag as an absent-service no-op used with `--pam --remove`.

Both `Commands::Setup` destructuring sites in `main.rs` pass the field into
`SetupArgs`, which continues to mirror the Clap variant field for field.
`resolve_setup_plan` carries the value only when constructing:

```text
PamPref::Remove { service, if_present }
```

`PamPref::Install`, `Ask`, and `Skip` are unchanged. Wizard step 9 continues to
classify every removal plan as deferred; `run_with_plan` later dispatches the
service and flag through `run_pam`. The resulting flow is:

```text
Commands::Setup
  -> SetupArgs
  -> resolve_setup_plan
  -> PamPref::Remove { service, if_present }
  -> run_with_plan
  -> run_pam
  -> pam_remove
  -> pam_remove_in
```

The existing default service remains `sudo`, and `--if-present` does not cause
the base wizard to run or otherwise change standalone `--pam` behavior.

## Removal and Error Semantics

`pam_remove(service, if_present)` remains the real-system wrapper and delegates
to `pam_remove_in(Path::new(PAM_DIR), service, if_present)`. The parameterized
function joins `base` and `service`, allowing all behavior to be exercised in a
temporary directory without root privileges.

The function reads the service file directly with `fs::read_to_string`; it must
not use `Path::exists()`. `exists()` maps metadata failures to `false`, which
could incorrectly turn permission or other filesystem errors into a successful
skip. The direct read is matched as follows:

- `ErrorKind::NotFound` with `if_present == true`: emit the absent-service PAM
  message and return `Ok(())` before any backup lookup or write.
- `ErrorKind::NotFound` with `if_present == false`: preserve the current fatal
  `PAM service file not found: <path>` behavior.
- Any other read error: remain fatal with the existing path context.
- Successful read: run the existing line-removal behavior unchanged.

For an existing file with no active Facelock line, return success, leave the
file byte-identical, and continue reporting `PamNoLineFound`. For an existing
file with a Facelock line, remove the line while preserving the current
trailing-newline behavior and report `PamRemoved`. Existing backup-restoration
hints remain unchanged. Write failures remain fatal.

The absent path must create no service file, backup, directory, or other entry
under the supplied base directory. The guarantee applies only to the observed
absent-file no-op; existing-file editing retains its current implementation and
semantics.

## Message Behavior

Add `PamMessage::PamServiceAbsent { path: String }`. It is an informational
stdout event, for example:

```text
PAM service file absent: /etc/pam.d/omarchy-lock-face. Nothing to remove.
```

The new variant receives an exhaustive `localized()` arm and is inserted into
the `Samples::next_sample` chain so placeholder and enumeration coverage cannot
silently omit it. It does not contain or alter the frozen PAM wire strings
`rate limited`, `IR camera required`, or exact `cancelled`.

Although the CLI documentation describes the operation as succeeding
"quietly," this message is deliberate: it means no warning or error and no
interactive prompt, while still making the no-op visible and translatable.

## Test Plan

Removal tests call `pam_remove_in` against `tempfile::TempDir`:

1. Missing service plus `if_present = true` returns success and leaves the
   entire base directory unchanged and empty.
2. Missing service plus `if_present = false` returns the existing error,
   guarding the compatibility default.
3. Existing service containing an active Facelock line plus
   `if_present = true` removes the line exactly as ordinary removal does.
4. Existing service without a Facelock line plus `if_present = true` returns
   success, leaves the file byte-identical, and follows the unchanged
   `PamNoLineFound` branch.
5. A non-`NotFound` filesystem failure remains fatal, demonstrating that the
   flag does not suppress arbitrary I/O failures.

Plan and parser coverage will assert that `--pam --remove --if-present`
produces `PamPref::Remove { if_present: true, .. }`, that ordinary removal
produces `if_present: false`, and that `--if-present` without the required
modifiers is rejected. The existing deferred-step test will be updated for the
new enum field and continue proving wizard step 9 writes nothing.

The PAM message sample walk will cover `PamServiceAbsent`, and help output will
be checked for `--if-present` and its description. Final verification is:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check
cargo run --bin facelock -- setup --help
```

## Documentation and Manual Check

`CHANGELOG.md` receives an `[Unreleased]` entry describing the opt-in behavior
and referencing #148. `docs/contracts.md` receives a concise addition to setup
flag composition stating that `--if-present` requires removal and suppresses
only an absent PAM service file.

The manual check runs the focused removal tests with `--nocapture` and inspects
their temp-directory assertions/output. It must use `pam_remove_in` and a fresh
temporary directory; the real CLI removal wrapper is not invoked because it is
intentionally pinned to `/etc/pam.d` and requires root.

## Compatibility and Security Constraints

- The no-flag missing-file exit remains non-zero.
- The flag suppresses only `ErrorKind::NotFound`; permission, malformed UTF-8,
  read, and write failures remain fatal.
- Root checks on the real PAM path remain in place.
- `pam_install` still refuses to invent a missing service file.
- Facelock never deletes a PAM service file and never modifies an absent path.
- Existing line matching, commented-line handling, trailing-newline behavior,
  backup hints, and no-line success behavior remain unchanged.
- No PAM authentication outcome, D-Bus protocol, frozen message, config key,
  database schema, model path, or runtime inference behavior changes.
- All implementation and verification work stays in the isolated issue #148
  worktree, and only touched files are staged.
