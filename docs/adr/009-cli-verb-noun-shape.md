# ADR 009: Verb/Noun Shape of the CLI Command Tree

## Status

Accepted

## Date

2026-08-17

## Decision

Rename the four wrong-shape command names now, delete the old spellings rather
than aliasing them, keep task verbs top-level, and freeze the rule that decides
where a future command goes.

| Today | After | Note |
|---|---|---|
| `facelock restart` | `facelock daemon restart` | |
| `facelock daemon` | `facelock daemon run` | bare `facelock daemon` still runs the daemon |
| `facelock encrypt` | `facelock tpm encrypt` | keeps `--generate-key` |
| `facelock decrypt` | `facelock tpm decrypt` | |
| `facelock reseal` | `facelock tpm reseal` | |
| `facelock config` | `facelock config show` | bare `facelock config` still shows |
| `facelock config --edit` | `facelock config edit` | |

`Daemon` and `Config` become `Option<…Command>` variants where `None` means
`run` and `show`. Bare `facelock daemon` and `facelock daemon -c X` must keep
parsing: five init-system units invoke the bare form, `commands::setup::run_systemd`
matches on it (§4), and `--config` is `global = true`.

**The old spellings are deleted, not hidden.** Clap has no cross-level alias, so
preserving `restart` would mean a hidden duplicate `Commands` variant carried
until 1.0 and removed in a later breaking change. The project is pre-1.0 at
0.1.4, no tag ships the renames yet, and nothing outside this repository invokes
the four names: the byte-coupled surfaces in §4 name none of them. The two
in-repository Omarchy prototype wrappers that existed when this ADR was
accepted also named none of them (verified). Issue #173 later retired those
prototypes; they are not current callers. The rename lands as a `feat!:` with a
CHANGELOG `BREAKING` line naming each one.

**`tpm` absorbs the key commands, not a new `key` group.** `tpm` already owns
the key's lifecycle (`seal-key`, `unseal-key`, `unseal-check`) and `reseal`
already dispatches into `commands::tpm::run_reseal`, so a `key` group would
either split key material across two groups or strand `tpm` with two
subcommands. The cost is that `tpm encrypt` runs software AES-256-GCM with no
TPM involved (ADR 004), which the group's `about` text has to say plainly:
`tpm` is where key material is managed, whether or not a TPM protects it.

**The rule**, adopted here and written into `docs/contracts.md` §CLI Subcommands
by the rename PR:

> A top-level command names a user task and keeps its spelling for the life of
> the binary. A noun group exists when the noun names a distinct operational
> domain and owns two or more subcommands. The domains: `pam` (`/etc/pam.d`),
> `tpm` (the TPM device and the encryption key), `hyprlock` (`hyprlock.conf`),
> `daemon` (the running service), `config` (the config file), `bench`
> (measurement runs). Facelock's primary objects, meaning face models, cameras,
> the audit log and the install itself, are reached by top-level commands and
> never earn a group. Inside a group the second word is spelled the way its
> domain spells it, verb or noun: `tpm seal-key` and `tpm pcr-baseline` follow
> tpm2-tools, `bench cold-auth` names a measurement. A new command must fit an
> existing domain before it may claim a top-level name. Commands named by
> `pam_facelock.so`, the service units, or the Omarchy scripts never move.

At acceptance, “the Omarchy scripts” meant the then-shipped package-owned
prototypes. Issue #173 later retired those prototypes, so that clause records
the compatibility analysis at the time; it does not identify a current caller
or integration surface.

## 1. Why not the alternatives

**Why not freeze everything and only write the rule down.** That was the other
serious option and it saves nothing. The rename touches four names that no
script outside this repository invokes, so freezing buys no compatibility and
leaves `restart` sitting beside the `daemon` noun it acts on for the life of
the binary. The rule is worth writing either way, which is why it is adopted
here rather than treated as the alternative to renaming.

**Why not converge the whole tree on noun-then-verb.** `enroll`, `list`,
`remove`, `clear`, `test`, `preview`, `setup`, `auth` are task verbs on
facelock's primary object, and `git add` / `git commit` / `git log` is the
idiom they follow. A `models list` costs a word on the most-typed commands and
would still leave `enroll` and the frozen `is-enrolled` outside the group,
adding a shape rather than removing one.

**Why not keep the old spellings as aliases.** Clap aliases work only at the
same level, so a top-level `restart` cannot alias `daemon restart`. Every
preserved spelling would be a hidden duplicate variant with its own `about`
text and dispatch arm, permanent until someone justifies a breaking removal a
second time. Pre-1.0, with no external caller, the deletion is cheaper now than
the alias is later.

## 2. The tree this replaces

From `Cli::command()` in `crates/facelock-cli/src/main.rs`. Twenty-three
top-level commands in four shapes.

| Command | `Commands` variant | Shape | Object it acts on |
|---|---|---|---|
| `setup` | `Setup` | verb | the install |
| `is-enrolled` | `IsEnrolled` | predicate | a user's enrollment |
| `capabilities` | `Capabilities` | noun | this binary |
| `enroll` | `Enroll` | verb | a face model |
| `remove <id>` | `Remove` | verb | a face model |
| `clear` | `Clear` | verb | all face models |
| `list` | `List` | verb | face models |
| `test` | `Test` | verb | recognition |
| `preview` | `Preview` | verb | the camera |
| `config` | `Config` | noun | the config file |
| `status` | `Status` | noun | the install |
| `devices` | `Devices` | noun | cameras |
| `daemon` | `Daemon` | noun, but it *runs* the daemon | the daemon |
| `auth` | `Auth` | verb | a user |
| `encrypt` | `Encrypt` | verb | stored embeddings |
| `decrypt` | `Decrypt` | verb | stored embeddings |
| `reseal` | `Reseal` | verb | the AES key |
| `restart` | `Restart` | verb | the daemon |
| `audit` | `Audit` | noun | the audit log |
| `bench <sub>` | `Bench` | noun group | benchmarks |
| `tpm <sub>` | `Tpm` | noun group | the TPM |
| `pam <sub>` | `Pam` | noun group | `/etc/pam.d` |
| `hyprlock <sub>` | `Hyprlock` | noun group | `hyprlock.conf` |

Twelve bare verbs, one predicate, six bare nouns, four noun groups. The groups
are not uniformly noun-verb either. `pam` and `hyprlock` are two verbs plus a
`status` noun. `tpm` is three verb phrases (`seal-key`, `unseal-key`,
`unseal-check`) plus two nouns (`status`, `pcr-baseline`). `bench` is eight
subcommands: seven nouns plus `calibrate`.

## 3. What was inconsistent

| # | Inconsistency | Evidence | Fixed |
|---|---|---|---|
| 1 | The `daemon` noun exists, and one of its verbs lives outside it. | `Commands::Daemon` dispatches to `commands::daemon::run`; `Commands::Restart` dispatches to `commands::config::restart`, which lives in the *config* module and shells out to `systemctl restart`, falling back to `busctl … Shutdown` when systemd is unavailable | yes |
| 2 | Key protection is split across two levels. | `encrypt`, `decrypt`, `reseal` are top-level; `tpm seal-key`, `tpm unseal-key`, `tpm unseal-check` are nested. `Commands::Reseal` dispatches to `commands::tpm::run_reseal`, so `reseal` is implemented inside the `tpm` group it does not live in | yes |
| 3 | `config` is a noun whose verb is a flag. | `Commands::Config { edit: bool }`: bare `config` displays, `config --edit` edits. `pam status` and `hyprlock status` spell the same read as a verb | yes |
| 4 | `remove` means two things at two levels. | Top-level `Commands::Remove` deletes a face model by id; `pam remove` deletes a line from `/etc/pam.d` | no, deliberately: both are correct in their own domain |
| 5 | Face models have no noun. | `enroll`, `list`, `remove`, `clear` act on the per-user model set and only `enroll` says so | no: primary object, §1 |
| 6 | `is-enrolled` and `enroll` share an object and no shape. | One is a predicate, the other a verb. The predicate is a frozen integration point | no: cannot move (§4) |

## 4. Byte-coupled surfaces the rename must not touch

None of the four renamed names appears here, which is why the rename stops
where it does.

The two Omarchy rows record package-owned prototypes that existed when this ADR
was accepted. Issue #173 later retired them; the rows are historical evidence,
not current callers or shipped integration points.

| Invocation | Caller |
|---|---|
| `facelock auth --user X --config Y` | `pam_facelock.so`, pinned by `legacy_invocations_still_parse` |
| `facelock daemon` | `systemd/facelock-daemon.service`, `dist/nix/module.nix`, `dist/s6/facelock-daemon/run`, `dist/runit/run`, and a `justfile` check that greps the unit for that exact string |
| `ExecStart=/usr/bin/facelock daemon`, as a hard-coded literal | `commands::setup::run_systemd` passes it to `refresh_legacy_copy_if_present` as the marker deciding whether the unit at `/etc/systemd/system/facelock-daemon.service` is facelock's and should be refreshed. A rename makes the marker stop matching **silently**: the legacy unit is left stale and nothing fails to compile |
| `facelock setup --non-interactive`, `facelock devices`, `facelock enroll`, `facelock hyprlock enable`, `facelock test` | then-shipped Omarchy setup prototype (retired by #173) |
| `facelock hyprlock disable`, `facelock setup --pam --service X --remove --yes` | then-shipped Omarchy removal prototype (retired by #173) |
| `facelock is-enrolled`, `facelock pam status`, `facelock capabilities` | lock screens and wrapper scripts, per `docs/contracts.md` §CLI Subcommands |

## 5. Implementation

One PR, `feat!:`, CHANGELOG `BREAKING` line per rename.

**Clap tree** (`crates/facelock-cli/src/main.rs`): delete `Commands::Restart`,
`Encrypt`, `Decrypt`, `Reseal`; add `DaemonCommand { Run, Restart }` and
`ConfigCommand { Show, Edit }` behind `Option`; add `Encrypt`, `Decrypt`,
`Reseal` to `TpmCommand`. Update the dispatch arms and the `unreachable!()`
arm list. New group nodes need `about` text or `cli_flag_conformance` fails.

**Registries.** Two need no change and one needs less than the brief assumed:

- `SHORT_REGISTRY`: unchanged. None of the renamed commands binds a short
  letter (`encrypt --generate-key` and `config --edit` have none).
- `JSON_COMMANDS`: unchanged. None of the renamed commands offers `--json`.
- `capability_names_are_all_implemented`: unchanged. `CAPABILITIES` names none
  of the four, and no capability name changes.
- `legacy_invocations_still_parse`: **it has no `restart`, `encrypt`,
  `decrypt`, `reseal` or `config --edit` rows to delete** (verified). What it
  has is the `daemon -c X` / `-c X daemon` pair, which must keep passing under
  the new `Option<DaemonCommand>` shape. Add rows for bare `facelock daemon`
  and bare `facelock config` so the `None` arms are pinned. This ADR is the
  authority for not adding rows for the deleted spellings.

**Tests.** `crates/facelock-cli/tests/cli_smoke.rs`:
`config_edit_refuses_before_root_non_root` invokes `["config", "--edit"]` and
becomes `["config", "edit"]`; `help_output_contains_expected_subcommands` needs
no change, since its list names `config` and none of the four.

**Source strings.** These name an old spelling in help text, a `require_root`
hint, or a comment, and were found by grepping each spelling:
`commands/config.rs`, `commands/encrypt.rs`, `commands/tpm.rs`,
`message/setup.rs`, `facelock-core/src/config.rs`, `facelock-tpm/src/sealing.rs`,
`config/facelock.toml`, `po/facelock.pot`, `test/tpm-pcr-e2e.sh`.

**Docs.** `docs/cli.md`, `docs/contracts.md` (§CLI Subcommands table and the
§CLI Privilege Model DEC-6 escalation lists, which name `restart`, `encrypt`,
`decrypt`, `reseal` and `config --edit` by spelling), `docs/security.md`,
`README.md`, `man/facelock.1` §COMMANDS, `book/src/cli-reference.md`,
`book/src/contracts.md`, `book/src/gpu.md`, `CHANGELOG.md`. The `book/src/`
tree is an independent mdBook copy that has already diverged from `docs/`
(`book/src/contracts.md` is 199 lines against 1122, and its CLI table still
lists `setup --pam` as "Install PAM module"), so each book edit is a
re-derivation against a stale document rather than a copy of the `docs/` edit.

Two notes on the doc list. `docs/quickstart.md` and `docs/troubleshooting.md`
carry no occurrence of any renamed spelling today, so the PR should re-grep
rather than trust this list. `docs/adr/008-camera-lifecycle.md` names
`config --edit` and must **not** be rewritten: an accepted ADR records what was
true when it was written.

**G8 (#172).** The `docs_*` coverage tests do not exist on this branch. If they
land first, the rename updates them; if the rename lands first, G8 writes them
against the new tree.

**Sequencing.** Land after the CLI-consistency stack (#178 through #184) merges
and after `pam-resolution-and-setup-ux.md` P1 if that runs first, whichever is
later. Both churn `main.rs` and `pam.rs`, and this PR rewrites the `Commands`
enum, so rebasing under them costs more than waiting.

## 6. What this ADR does not decide

- The `TOP_LEVEL_COMMANDS` registry test and the `contracts.md` rule text land
  in the rename PR, not here.
- `tpm reseal` will sit beside `tpm seal-key`, two spellings of one object with
  different suffix conventions. Aligning them is a separate call and is not
  part of this decision.
- `status --json` (G4b), which is independent of where `status` sits.
- The D-Bus method surface and the client-duality boundary in #106. This ADR
  covers argv spelling.
- Flag spelling, owned by `docs/contracts.md` §CLI Flag Spelling and enforced
  by `cli_flag_conformance`.

## 7. References

- `crates/facelock-cli/src/main.rs`: `Cli`, `Commands`, `SHORT_REGISTRY`,
  `JSON_COMMANDS`, `cli_flag_conformance`, `legacy_invocations_still_parse`,
  `capability_names_are_all_implemented`
- `crates/facelock-cli/src/commands/setup.rs`: `run_systemd`,
  `refresh_legacy_copy_if_present`
- `docs/contracts.md` §CLI Subcommands, §CLI Flag Spelling, §CLI Privilege
  Model (DEC-6); `docs/releasing.md` pre-1.0 versioning contract
- [ADR 004](004-tpm-encryption.md): software AES with optional TPM key sealing
- Issue #175, the CLI-consistency cycle this gap belongs to
