# PAM Remove `--if-present` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in `--if-present` mode to `facelock setup --pam --remove` that succeeds only when the target PAM service file is absent, while leaving the existing default and every other error fatal.

**Architecture:** Carry one boolean from Clap through `SetupArgs` and `PamPref::Remove` to the PAM removal edge. Mirror the existing installation seam with `pam_remove_in(base, service, if_present)`, read the target directly, and special-case only `io::ErrorKind::NotFound`; all tests use a temporary PAM directory and never touch `/etc/pam.d`.

**Tech Stack:** Rust 2024, Clap derive, `anyhow`, the existing typed `PamMessage`/gettext seam, `tempfile`, Cargo test/clippy/fmt.

---

## File Map

- Modify `crates/facelock-cli/src/main.rs`: define the flag, thread it through both `Commands::Setup` destructuring sites, and test parser/plan/help behavior.
- Modify `crates/facelock-cli/src/commands/setup.rs`: extend `SetupArgs`/`PamPref`, dispatch the policy, add `pam_remove_in`, and add temp-directory behavior tests.
- Modify `crates/facelock-cli/src/message/pam.rs`: add and exhaustively enumerate `PamServiceAbsent`.
- Modify `crates/facelock-cli/src/message/mod.rs`: pin the absent-service English fallback text.
- Modify `docs/contracts.md`: document flag composition and the NotFound-only guarantee.
- Modify `CHANGELOG.md`: add the issue #148 user-visible change under `[Unreleased]`.

The enum/data-flow ripple and removal behavior are one implementation unit: splitting them into separate commits would leave an uncompilable intermediate state or a parsed flag that is silently dropped. Documentation and final verification form a second independently reviewable task.

### Task 1: Implement the flag, typed message, and temp-directory removal behavior with TDD

**Files:**
- Modify: `crates/facelock-cli/src/main.rs:31-84,277-309,369-429,475-577,665-695`
- Modify: `crates/facelock-cli/src/commands/setup.rs:100-130,172-221,337-350,1838-1855,2779-2974,3570-3590,3865-3945`
- Modify: `crates/facelock-cli/src/message/pam.rs:16-75,77-178,187-237`
- Modify: `crates/facelock-cli/src/message/mod.rs:330-415`
- Test: inline `#[cfg(test)]` modules in the four files above

- [ ] **Step 1: Confirm the isolated worktree is clean before editing**

Run:

```bash
pwd
git status --short --branch
git diff --exit-code
git diff --cached --exit-code
```

Expected: `pwd` is `/home/ty/Code/facelock/.worktrees/issue-148-if-present`; the branch is `fix/148-pam-remove-if-present`; both diff commands exit 0; no dirty paths are listed. Do not stash, clean, stage, or edit the primary checkout.

- [ ] **Step 2: Add the final parser, plan, help, removal, and message tests before production changes**

In `crates/facelock-cli/src/main.rs`, update the two existing `PamPref::Remove` expectations to include `if_present: false`, then add these tests beside the action-modifier tests:

```rust
#[test]
fn pam_remove_if_present_reaches_the_resolved_plan() {
    let p = plan(&[
        "--pam",
        "--service",
        "omarchy-lock-face",
        "--remove",
        "--if-present",
    ]);
    assert_eq!(p.base, None);
    assert_eq!(
        p.pam,
        PamPref::Remove {
            service: "omarchy-lock-face".to_string(),
            if_present: true,
        }
    );
}

#[test]
fn if_present_requires_remove_and_pam() {
    assert_eq!(
        parse_error(&["--if-present"]).kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    assert_eq!(
        parse_error(&["--pam", "--if-present"]).kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    assert_eq!(
        parse_error(&["--remove", "--if-present"]).kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
}

#[test]
fn setup_help_documents_if_present() {
    let mut command = Cli::command();
    let setup = command
        .find_subcommand_mut("setup")
        .expect("setup subcommand must exist");
    let help = setup.render_long_help().to_string();
    assert!(help.contains("--if-present"), "{help}");
    assert!(
        help.contains("succeed quietly if the service file is absent"),
        "{help}"
    );
}
```

The two existing expected values become:

```rust
PamPref::Remove {
    service: "sudo".to_string(),
    if_present: false,
}
```

In `crates/facelock-cli/src/commands/setup.rs`, add these tests inside `mod action_tests`, where `hash_dir` already detects both byte changes and unexpected new files:

```rust
#[test]
fn pam_remove_if_present_missing_service_is_a_noop() {
    let dir = tempfile::TempDir::new().unwrap();
    let before = hash_dir(dir.path());

    pam_remove_in(dir.path(), "omarchy-lock-face", true).unwrap();

    assert_eq!(before, hash_dir(dir.path()));
    assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn pam_remove_missing_service_without_if_present_still_errors() {
    let dir = tempfile::TempDir::new().unwrap();

    let error = pam_remove_in(dir.path(), "omarchy-lock-face", false)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("PAM service file not found:"),
        "{error}"
    );
    assert!(error.contains("omarchy-lock-face"), "{error}");
    assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn pam_remove_if_present_removes_an_existing_facelock_line() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("omarchy-lock-face");
    fs::write(
        &path,
        format!("#%PAM-1.0\n{PAM_LINE}\nauth include system-auth\n"),
    )
    .unwrap();

    pam_remove_in(dir.path(), "omarchy-lock-face", true).unwrap();

    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "#%PAM-1.0\nauth include system-auth\n"
    );
}

#[test]
fn pam_remove_if_present_preserves_a_file_without_a_facelock_line() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("omarchy-lock-face");
    let original = b"#%PAM-1.0\nauth include system-auth\n".to_vec();
    fs::write(&path, &original).unwrap();

    pam_remove_in(dir.path(), "omarchy-lock-face", true).unwrap();

    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn pam_remove_if_present_does_not_suppress_other_read_errors() {
    let dir = tempfile::TempDir::new().unwrap();
    let before = hash_dir(dir.path());

    let error = pam_remove_in(dir.path(), ".", true)
        .unwrap_err()
        .to_string();

    assert!(error.contains("failed to read"), "{error}");
    assert_eq!(before, hash_dir(dir.path()));
}
```

Update the existing deferred-step fixture to carry the new policy explicitly:

```rust
let plan = pam_plan(PamPref::Remove {
    service: "sudo".to_string(),
    if_present: true,
});
```

In `crates/facelock-cli/src/message/mod.rs`, extend `english_fallback_is_byte_identical` with the skipped-case rendering assertion:

```rust
assert_eq!(
    PamMessage::PamServiceAbsent {
        path: "/etc/pam.d/omarchy-lock-face".into()
    }
    .localized(),
    "PAM service file absent: /etc/pam.d/omarchy-lock-face. Nothing to remove."
);
```

The existing no-facelock-line temp-dir test exercises the `PamNoLineFound` branch; the existing message implementation and sample sweep continue to pin that event. The new exact rendering assertion pins the additional absent-service event.

- [ ] **Step 3: Run the focused package tests and verify RED**

Run:

```bash
cargo test -p facelock-cli
```

Expected: compilation fails because `PamPref::Remove` has no `if_present` field, `pam_remove_in` is undefined, and `PamMessage::PamServiceAbsent` is undefined. These failures prove the tests require the new end-to-end behavior rather than passing on the old implementation.

- [ ] **Step 4: Add the Clap flag and thread it through both main.rs paths**

In the `Commands::Setup` action modifiers, insert the flag immediately after `remove`:

```rust
/// Used with --pam --remove: succeed quietly if the service file is absent
#[arg(long = "if-present", requires = "remove")]
if_present: bool,
```

In both `Commands::Setup` patterns—the runtime dispatch around line 277 and the test helper around line 385—insert `if_present` immediately after `remove`. In both corresponding `SetupArgs` initializers, insert the same field:

```rust
service,
remove,
if_present,
camera,
```

Keep `requires = "remove"`; do not duplicate a `requires = "pam"` constraint unless `Cli::command().debug_assert()` or the parser tests demonstrate that Clap does not compose `if-present -> remove -> pam`.

- [ ] **Step 5: Carry the policy through setup resolution and dispatch**

In `crates/facelock-cli/src/commands/setup.rs`, change the removal variant and raw arguments to:

```rust
pub enum PamPref {
    Ask,
    Install { service: Option<String> },
    Remove { service: String, if_present: bool },
    Skip,
}

pub struct SetupArgs {
    pub non_interactive: bool,
    pub yes: bool,
    pub pam: bool,
    pub no_pam: bool,
    pub systemd: bool,
    pub no_systemd: bool,
    pub enroll: bool,
    pub no_enroll: bool,
    pub disable: bool,
    pub service: Option<String>,
    pub remove: bool,
    pub if_present: bool,
    pub camera: Option<String>,
    pub models: Option<ModelPreset>,
    pub execution_provider: Option<ExecutionProviderChoice>,
    pub encryption: Option<EncryptionChoice>,
}
```

Construct the resolved removal policy without changing service defaulting:

```rust
PamPref::Remove {
    service: args
        .service
        .clone()
        .unwrap_or_else(|| DEFAULT_PAM_SERVICE.to_string()),
    if_present: args.if_present,
}
```

Dispatch the policy in `run_with_plan`:

```rust
PamPref::Remove {
    service,
    if_present,
} => run_pam(service, true, *if_present, plan.yes)?,
```

Keep `pam_step_for` structurally deferred:

```rust
PamPref::Remove { .. } => PamStep::Deferred,
```

Change `run_pam` and its removal call to:

```rust
pub fn run_pam(
    service: &str,
    remove: bool,
    if_present: bool,
    yes: bool,
) -> anyhow::Result<()> {
    if !nix::unistd::Uid::current().is_root() {
        bail!("PAM configuration requires root. Run with sudo.");
    }

    if remove {
        pam_remove(service, if_present)
    } else {
        pam_install(service, yes, false)?;
        print_pam_extension_hint();
        Ok(())
    }
}
```

There is only one current `run_pam` caller. Do not add the flag to `pam_install`, and do not weaken either installation precondition.

- [ ] **Step 6: Add the typed absent-service message and exhaustive sample link**

In `crates/facelock-cli/src/message/pam.rs`, insert this enum variant between `PamNoLineFound` and `PamBackupExists`:

```rust
PamServiceAbsent {
    path: String,
},
```

Insert the corresponding `localized()` arm between the existing no-line and backup arms:

```rust
PamServiceAbsent { path } => fill(
    translate("PAM service file absent: {path}. Nothing to remove."),
    &[("path", path.clone())],
),
```

Link the variant into the exhaustive sample chain:

```rust
PamRemoved { .. } => PamNoLineFound { path: s("/p") },
PamNoLineFound { .. } => PamServiceAbsent { path: s("/p") },
PamServiceAbsent { .. } => PamBackupExists {
    path: s("/p"),
    backup: s("/b"),
},
PamBackupExists { .. } => return None,
```

Do not use any authentication-wire language in this setup event. In particular, do not alter or introduce the frozen substrings `rate limited`, `IR camera required`, or exact `cancelled`.

- [ ] **Step 7: Replace the hardcoded removal function with the real-path wrapper and direct-read seam**

In `crates/facelock-cli/src/commands/setup.rs`, replace the complete current `pam_remove` function with:

```rust
fn pam_remove(service: &str, if_present: bool) -> anyhow::Result<()> {
    pam_remove_in(Path::new(PAM_DIR), service, if_present)
}

fn pam_remove_in(base: &Path, service: &str, if_present: bool) -> anyhow::Result<()> {
    let pam_file = base.join(service);
    let pam_path = pam_file.display().to_string();

    let content = match fs::read_to_string(&pam_file) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if if_present {
                Terminal.info(&PamMessage::PamServiceAbsent {
                    path: pam_path.clone(),
                });
                return Ok(());
            }
            bail!("PAM service file not found: {pam_path}");
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {pam_path}"));
        }
    };

    let original_count = content.lines().count();
    let new_lines: Vec<&str> = content
        .lines()
        .filter(|line| !is_facelock_pam_line(line))
        .collect();

    if new_lines.len() == original_count {
        Terminal.info(&PamMessage::PamNoLineFound {
            path: pam_path.clone(),
        });
    } else {
        let mut output = new_lines.join("\n");
        if content.ends_with('\n') {
            output.push('\n');
        }

        fs::write(&pam_file, &output)
            .with_context(|| format!("failed to write {pam_path}"))?;
        Terminal.info(&PamMessage::PamRemoved {
            path: pam_path.clone(),
        });
    }

    let backup_path = format!("{pam_path}.facelock-backup");
    if Path::new(&backup_path).exists() {
        Terminal.info(&PamMessage::PamBackupExists {
            path: pam_path,
            backup: backup_path,
        });
    }

    Ok(())
}
```

The read must remain direct. Do not reintroduce `pam_file.exists()`: it would collapse permission and metadata failures into the same branch as absence, violating the issue's “file-not-found only” requirement. Return from the permitted NotFound branch before checking for backups so the no-op cannot read or write anything else under the base directory.

- [ ] **Step 8: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p facelock-cli --bin facelock pam_remove -- --nocapture
cargo test -p facelock-cli --lib pam_remove -- --nocapture
cargo test -p facelock-cli --lib message::tests::english_fallback_is_byte_identical
cargo test -p facelock-cli --lib message::tests::no_unfilled_placeholders
cargo test -p facelock-cli
```

Expected: every command exits 0. The `--nocapture` output includes the absent-service and no-line informational messages; all temp-directory assertions pass; no command requests root or accesses `/etc/pam.d`.

- [ ] **Step 9: Format, inspect scope, and commit the coherent implementation**

Run:

```bash
cargo fmt --all
git diff --check
git status --short
git diff -- crates/facelock-cli/src/main.rs crates/facelock-cli/src/commands/setup.rs crates/facelock-cli/src/message/pam.rs crates/facelock-cli/src/message/mod.rs
```

Expected: only the four listed Rust files are modified; the diff contains no installation changes, PAM service deletion, host-path test writes, or unrelated formatting.

Stage only those files—never `git add -A`—and commit:

```bash
git add crates/facelock-cli/src/main.rs
git add crates/facelock-cli/src/commands/setup.rs
git add crates/facelock-cli/src/message/pam.rs
git add crates/facelock-cli/src/message/mod.rs
git diff --cached --name-status
git commit --no-gpg-sign -m "feat(setup): tolerate absent PAM service on request"
```

Expected staged paths: exactly the four Rust files above.

### Task 2: Document the contract, run the temp-directory manual check, and complete release verification

**Files:**
- Modify: `docs/contracts.md:35-65` (`facelock setup Flag Composition`)
- Modify: `CHANGELOG.md:9-30` (`[Unreleased]` → `Added`)
- Verify: the entire Cargo workspace and final Git diff

- [ ] **Step 1: Add the setup flag contract**

In `docs/contracts.md`, immediately after the paragraph stating that `--remove` and `--service` require `--pam`, add:

```markdown
`--if-present` requires `--remove` (and therefore `--pam`). It changes only a
missing target service file from an error into a successful no-op; read, parse
and write failures remain fatal, and `--remove` without the flag retains its
historical missing-file error.
```

This documents the compatibility boundary; do not change any PAM authentication outcome or frozen protocol section.

- [ ] **Step 2: Add the Unreleased changelog entry with issue reference**

Under `## [Unreleased]` → `### Added` in `CHANGELOG.md`, add:

```markdown
- **Optional idempotent PAM-line removal** (#148): `facelock setup --pam
  --remove --if-present` now succeeds when the requested PAM service file is
  absent, so teardown scripts can iterate optional integrations without their
  own existence guards. The behavior is opt-in: omitting `--if-present` keeps
  the historical missing-file error, and all non-NotFound I/O failures remain
  fatal. Existing service files are never deleted.
```

- [ ] **Step 3: Run the required manual check against a temporary directory**

Run the focused removal tests with human-visible output:

```bash
cargo test -p facelock-cli --lib commands::setup::action_tests::pam_remove_ -- --nocapture
```

Expected: all matching tests pass. Output demonstrates the absent-service no-op, unchanged `PamNoLineFound` path, and ordinary removal path. The tests' `TempDir`/`hash_dir` assertions prove no missing target, backup, or sibling file is created. Do not run `sudo facelock setup --pam --remove` and do not inspect or modify `/etc/pam.d` for this check.

- [ ] **Step 4: Verify parser dependencies and user-facing help manually**

Run:

```bash
cargo run --bin facelock -- setup --help
```

Expected: exit 0 and help includes:

```text
--if-present
    Used with --pam --remove: succeed quietly if the service file is absent
```

Then run the parser regression tests directly:

```bash
cargo test -p facelock-cli --bin facelock if_present -- --nocapture
```

Expected: the accepted `--pam --remove --if-present` plan and all rejected incomplete combinations pass.

- [ ] **Step 5: Run the full definition-of-done verification**

Run each command separately and require exit 0 before continuing:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all workspace tests pass, Clippy emits no warnings, and rustfmt reports no diff. Do not claim completion from a previous baseline run; these commands must run on the final implementation and documentation tree.

- [ ] **Step 6: Review the final change for scope and contract compliance**

Run:

```bash
git diff --check
git status --short
git diff --name-only origin/main
rg -n "pam_remove_in|if_present|PamServiceAbsent" crates/facelock-cli/src/main.rs crates/facelock-cli/src/commands/setup.rs crates/facelock-cli/src/message/pam.rs crates/facelock-cli/src/message/mod.rs
rg -n "rate limited|IR camera required|cancelled" docs/contracts.md crates/pam-facelock/src
git diff --exit-code origin/main -- crates/pam-facelock/src
```

Expected: only `CHANGELOG.md` and `docs/contracts.md` remain uncommitted after Task 1; implementation symbols appear only in the intended CLI files; frozen authentication strings remain unchanged. Inspect `git diff` and confirm:

- missing without the flag still errors;
- only direct-read `ErrorKind::NotFound` is suppressed;
- `pam_install` is unchanged;
- no service-file deletion API exists;
- all removal tests use `TempDir`;
- no unrelated user or agent changes are staged.

- [ ] **Step 7: Commit only the documentation changes**

Run:

```bash
git add CHANGELOG.md
git add docs/contracts.md
git diff --cached --name-status
git commit --no-gpg-sign -m "docs: document idempotent PAM removal flag"
git status --short --branch
```

Expected staged paths before commit: exactly `CHANGELOG.md` and `docs/contracts.md`. Expected final status: clean `fix/148-pam-remove-if-present` branch ahead of `origin/main`; no untracked files.

- [ ] **Step 8: Prepare the PR handoff with issue #148 closure**

Use the repository's branch-finishing/publishing workflow. The PR title should be:

```text
feat(setup): add --if-present to PAM removal
```

The PR body must include this exact issue reference:

```markdown
Closes #148
```

Also summarize the opt-in compatibility behavior, direct-read NotFound-only handling, temp-directory test seam, and the final verification commands. Before publishing, verify the branch diff from the clean base:

```bash
git diff --stat origin/main...HEAD
git log --oneline origin/main..HEAD
git status --short --branch
```

Expected: the diff contains only the committed design/plan documents, the four intended CLI Rust files, `CHANGELOG.md`, and `docs/contracts.md`; the worktree is clean.
