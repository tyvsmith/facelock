# System Contracts

Stable contracts. Do not change without updating this document.

The CLI surface is the first half; the daemon, storage and protocol contracts
follow it.

- [Binaries](#binaries)
- [CLI Subcommands](#cli-subcommands)
  - [CLI Flag Spelling](#cli-flag-spelling)
  - [CLI Output Streams](#cli-output-streams)
  - [CLI Machine Output](#cli-machine-output)
  - [facelock setup Flag Composition](#facelock-setup-flag-composition)
  - [facelock pam Semantics](#facelock-pam-semantics)
  - [facelock status Semantics](#facelock-status-semantics)
  - [facelock capabilities](#facelock-capabilities)
  - [CLI Privilege Model (DEC-6)](#cli-privilege-model-dec-6)
  - [facelock test Semantics (N11)](#facelock-test-semantics-n11)
- [Operating Modes](#operating-modes)
  - [facelock is-enrolled Exit Codes](#facelock-is-enrolled-exit-codes)
  - [facelock auth Exit Codes](#facelock-auth-exit-codes)
- [Release Channels and APT Paths](#release-channels-and-apt-paths)
- [ONNX Runtime Trust and Fedora RPM Modes](#onnx-runtime-trust-and-fedora-rpm-modes)
- [Package Lifecycle Ownership](#package-lifecycle-ownership)
- [Filesystem Paths](#filesystem-paths)
  - [Audit Log Entries](#audit-log-entries)
- [Config Schema](#config-schema)
  - [Sections](#sections)
  - [Camera Auto-Detection](#camera-auto-detection)
- [Database Schema](#database-schema)
- [IPC Protocol](#ipc-protocol)
  - [Methods](#methods)
  - [Signals](#signals)
  - [Response types](#response-types)
  - [Authenticate error encoding](#authenticate-error-encoding)
  - [Rejection classes](#rejection-classes-authoutcomeerror)
  - [Daemon peer verification (PAM client)](#daemon-peer-verification-pam-client)
- [PAM Semantics](#pam-semantics)
  - [Syslog Format](#syslog-format)
- [Polkit Agent Semantics](#polkit-agent-semantics)
- [Anti-Spoofing](#anti-spoofing)
- [Models](#models)

## Binaries

| Binary | Crate | Purpose |
|--------|-------|---------|
| `facelock` | facelock-cli | Unified CLI (daemon, auth, enroll, test, setup, etc.) |
| `pam_facelock.so` | pam-facelock | PAM authentication module |
| `facelock-polkit-agent` | facelock-polkit | Polkit face authentication agent |

## CLI Subcommands

| Command | Purpose |
|---------|---------|
| `facelock setup` | Interactive setup wizard (camera, models, inference device, encryption, daemon, enrollment, PAM — the daemon before enrollment, so enrollment and the recognition test run on the transport later authentications use); removes a leftover `facelock` group from an older install, best-effort (ADR 010) |
| `facelock setup --systemd` | Install/enable systemd units |
| `facelock setup --pam` | Alias onto `facelock pam add\|remove` (see "facelock pam" below). Kept, and kept parsing, for every wrapper written against it |
| `facelock setup --pam --allow-sensitive` | Explicitly authorize an add to a sensitive PAM service. Does not suppress confirmation and conflicts with `--remove` |
| `facelock pam add` | Add the facelock line to one or more `/etc/pam.d/<service>` files. Root |
| `facelock pam remove` | Remove it. Root. Cleans validated Facelock-owned rollback state by default; `--keep-backup` preserves it |
| `facelock pam remove --all` | Config-independent, whole-machine removal of recognized Facelock-owned direct PAM edits beneath compiled roots. Root. Conflicts with `--service` |
| `facelock pam status` | Report whether services carry the line. Reads only, **no root** — the probe to branch on instead of grepping `/etc/pam.d` |
| `facelock setup` choice flags | `--camera <PATH\|auto>`, `--models <standard\|balanced\|high>`, `--execution-provider <cpu\|cuda\|rocm\|openvino\|auto>`, `--encryption <tpm\|keyfile\|none\|auto>`. Precedence: CLI flag > config file > built-in default |
| `facelock setup` action opt-outs | `--no-pam`, `--no-systemd`, `--no-enroll` decline an action outright (and their `--pam`/`--systemd`/`--enroll` counterparts force it). Later flag wins |
| `facelock is-enrolled` | Report whether face auth is operational for a user. Exit code is the contract; no daemon activation, no camera, no group: it opens the caller's own `0600` marker under `0711` directories (ADR 010) |
| `facelock capabilities` | Report what this build can do: one capability name per line, or `--json` for `{"version", "capabilities"}`. Unprivileged, reads no config, activates no daemon. The feature probe to branch on instead of grepping `--help` |
| `facelock enroll` | Capture and store a face |
| `facelock test` | Test face recognition |
| `facelock list` | List enrolled face models |
| `facelock remove <id>` | Remove a specific model |
| `facelock clear` | Remove all models for a user |
| `facelock preview` | Live camera preview |
| `facelock devices` | List V4L2 cameras |
| `facelock status` | Check system status. Root. `--json` renders the same report as one object a script can parse (see "facelock status Semantics") |
| `facelock config show` | Show configuration. Bare `facelock config` is `config show` |
| `facelock config edit` | Open the config file in `$EDITOR`, validate on save, restart the daemon when a cached setting changed. Root |
| `facelock daemon run` | Run the persistent daemon. Bare `facelock daemon` is `daemon run` — the form every shipped service unit invokes |
| `facelock daemon restart` | Restart the daemon (`systemctl restart`, or a D-Bus `Shutdown` when systemd is unavailable). Root |
| `facelock auth --user X` | One-shot auth (PAM helper). `--user` is required here and only here; `--config` is the global flag, not a per-command one |
| `facelock hyprlock enable\|disable\|status` | Manage hyprlock lock-screen integration (user, no root); `enable` accepts `--no-icon` to skip the cosmetic face glyph |
| `facelock tpm status` | TPM status, sealed-key presence and encrypted/plaintext embedding counts. Root, like every `tpm` verb |
| `facelock tpm encrypt` | Encrypt face database |
| `facelock tpm decrypt` | Decrypt face database |
| `facelock tpm reseal` | Re-seal the TPM AES key under current PCRs (recovery after a firmware/kernel change) |
| `facelock tpm seal-key` / `unseal-key` | Migrate keyfile↔tpm key protection |
| `facelock tpm unseal-check` | Read-only: verify the sealed key still unseals (PCR policy satisfied) |
| `facelock audit` | View audit log |
| `facelock bench` | Benchmarks |

**Where a command goes.** A top-level command names a user task and keeps its
spelling for the life of the binary. A noun group exists when the noun names a
distinct operational domain and owns two or more subcommands. The domains:
`pam` (`/etc/pam.d`), `tpm` (the TPM device and the encryption key), `hyprlock`
(`hyprlock.conf`), `daemon` (the running service), `config` (the config file),
`bench` (measurement runs). Facelock's primary objects, meaning face models,
cameras, the audit log and the install itself, are reached by top-level
commands and never earn a group. Inside a group the second word is spelled the
way its domain spells it, verb or noun: `tpm seal-key` and `tpm pcr-baseline`
follow tpm2-tools, `bench cold-auth` names a measurement. A new command must
fit an existing domain before it may claim a top-level name. Commands named by
`pam_facelock.so` or the service units never move. See ADR 009.

The top-level set is pinned by the `TOP_LEVEL_COMMANDS` registry in
`crates/facelock-cli/src/conformance/flags.rs`, checked in both directions against
`Cli::command()`: a name in the registry the binary does not offer fails, and a
top-level command with no row fails too. Nested verbs are deliberately absent
from it — where a verb sits inside its group is that group's business.

### CLI Flag Spelling

Flag spelling is a compatibility surface, not a presentation detail: `pam_facelock.so`
spawns `facelock auth --user <name> --config <path>` byte for byte, and wrapper
scripts hard-code the rest. Two things hold it still.

Shared clap arg structs in `crates/facelock-cli/src/args.rs` (`UserArg`,
`ConfirmArg`, `JsonArg`, `DryRunArg`) are flattened at every site, so a command
either offers a flag with the one spelling or does not offer it.
`cli_flag_conformance` in `crates/facelock-cli/src/conformance/flags.rs` walks
the whole command tree, nested subcommands included, and fails on any drift;
spending a new short letter means editing its registry on purpose.

The invariants it pins:

- `--user` is `-u` on every command that has it, including `auth`
- `auth --user` stays **required** — PAM names the subject and it must never
  fall back to the process owner. Every other `--user` defaults to the current user
- `--yes` is `-y` and accepts `--no-confirm` everywhere (it was `setup`-only)
- `--json` and `--dry-run` take no short letter
- `--config` (`-c`), `--quiet` (`-q`) and `--verbose` (`-v`) are declared once,
  `global = true`, and are accepted on either side of the subcommand name. No
  command re-declares them. `facelock daemon -c X` and `facelock -c X daemon`
  are equivalent, as are `facelock is-enrolled --quiet` and
  `facelock --quiet is-enrolled`
- `--verbose` counts its repeats, one level per `-v` from the program's own
  starting level (`warn` for the CLI, `info` for `daemon run`). `RUST_LOG`
  outranks it
- every subcommand has non-empty `about` text

`legacy_invocations_still_parse`, alongside it, is a table of real argv — the PAM
spawn included — that must keep parsing.

### CLI Output Streams

**stdout is the answer; stderr is everything else.** Every `facelock`
subcommand prints its result — the JSON payload of `--json`, the rendered
table, the state word — on stdout, and *only* that. Diagnostics (`tracing`
output, whatever `RUST_LOG` selects, warnings such as the D-Bus fallback
notice) go to stderr on every process this repository builds.

This is what makes `facelock devices --json | jq .` and
`facelock is-enrolled --json` safe to pipe: an integration reading stdout gets
the payload whatever the log level, and an operator raising `RUST_LOG` to debug
cannot break a script by doing so. Before this was contract, the subscriber
inherited `tracing_subscriber`'s stdout default and a single WARN corrupted the
JSON (#149).

An unparseable `RUST_LOG` is reported at WARN (on stderr) and the built-in
filter is used, rather than the value being silently discarded.

**Diagnostics default to `warn` in the CLI and `info` in the daemon.** Someone
typing a command reads its prompts and its report on the same terminal these
events land on, so the CLI prints warnings and errors and nothing quieter.
`facelock daemon run` keeps `info`, because the journal is its reader and
nothing competes with it there. `-v` raises the level one step per repeat from
whichever of the two this process started at. `RUST_LOG` outranks both,
including when it is the quieter of them: an override a flag can shout over is
not an override.

The level governs output volume and nothing else. Exit codes and stdout
payloads are identical at every level, so a consumer needs no flag it did not
need before. Every degradation an operator has to act on is WARN or above and
so survives the default: the D-Bus fallback in `backend::select`, an ONNX
Runtime that would not load, a provider that could not be queried, an
unreadable quirks file, an ignored `RUST_LOG`.

**`--quiet` suppresses informational chatter, and on commands whose stdout is
the payload, the payload too; errors, prompts and exit codes are unchanged.** A
quiet run that fails still says why on stderr and still exits non-zero, and a
prompt still asks — a silenced question is a hang, not a quieter program. This
is `is-enrolled --quiet`'s rule ("leave only the exit code") generalized to
every payload: `facelock --quiet devices --json` writes nothing on stdout, and
the exit code is the answer. `list --json` and `devices --json` printed their
payload under `--quiet` before this rule; they no longer do.

The flag is read once, by the two suppressible stdout sinks of the message seam
— `Terminal::info` for human text, `message::payload` for machine output — so
no command implements it and no command can forget it. There is a third stdout
sink, `Terminal::notice`, which `--quiet` deliberately does not reach: it is
for the human lines that must be seen and must stay on stdout: `pam add`'s
rollback instructions, the plaintext-embeddings warning, and the context a
confirmation needs to be answerable (`pam add`'s preview of the edit, and the
orphaned-models warning ahead of setup's delete confirmation). Everything else
informational stays on `Terminal::info` — a `notice` that did not have to be
seen is just an unquietable one.
[#140](https://github.com/tyvsmith/facelock/issues/140) tracks the commands
still printing human text directly.

`preview --json` is the one payload outside this rule: it emits a document per
frame until interrupted, so silencing it would leave a command that produces
nothing forever.

### CLI Machine Output

**Every command whose output a script would parse takes `--json`, and spells it
`--json`.** One flag family (the shared `JsonArg` in
`crates/facelock-cli/src/args.rs`), no short letter, no `--output json`, no
per-command invention. `cli_flag_conformance` pins both halves: an arg whose
help advertises JSON must carry the id `json`, so a second spelling fails the
build instead of shipping.

**A command gains `--json` when it has a named consumer, not to complete a
matrix.** The coverage list is the `JSON_COMMANDS` registry inside that test,
checked in both directions against the clap tree, so adding a row is the moment
someone states who parses the output.

| Command | Payload |
|---------|---------|
| `facelock is-enrolled --json` | one object. See "facelock is-enrolled Exit Codes" |
| `facelock capabilities --json` | one object. See "facelock capabilities" |
| `facelock list --json` | array of enrolled models |
| `facelock devices --json` | array of `IpcDeviceInfo` (`facelock_core::ipc`): `path`, `name`, `driver`, `is_ir`, `formats` (empty whenever the daemon answers, which carries no format detail). Serde-derived, so it is a typed schema rather than a scrape of the human renderer, whose columns and `[IR]` tag are free to change |
| `facelock preview --json` | one object per line, one per frame |
| `facelock status --json` | one object, one key per report section, each carrying an `ok`/`problem`/`unknown` verdict. See "facelock status Semantics" |
| `facelock pam add\|remove\|status --json` | one object, whose shape is a stability contract. See "facelock pam Semantics" |

`preview` is on the list because it always emitted JSON. It shipped calling the
flag `--text-only`, which survives as a hidden alias and keeps parsing; the
per-frame payload is byte for byte what it was.

Machine output does not pass through the translation seam: every `--json`
payload is built with `serde_json` and is C-locale by construction. It reaches
stdout through `message::payload`, which takes an already-rendered `&str` and
consults no catalog, so routing a payload through the seam to pick up `--quiet`
cannot translate it on the way. **Two** documented exceptions to the C-locale
rule, both diagnostics rather than things to branch on: `pam`'s `error` field
can interpolate a `strerror` string (see "facelock pam Semantics"), and
`status`'s `reason` and `error` fields can interpolate an OS, parser or runtime
message (see "facelock status Semantics"). Neither is a vocabulary — a consumer
prints them and branches on the typed words beside them. Neither exception
covers *facelock's own* catalog: a probe whose diagnostic is a translated string
does not get a field, which is why `status`'s `daemon` section has no `error`.

### facelock setup Flag Composition

Flags **compose**; they are not mutually exclusive. The rule:

- `--pam` and/or `--systemd` **on their own** perform just that action and touch
  nothing else. This preserves the historical standalone meaning, including
  `--pam --service <name>`, `--pam --remove`, and `--systemd --disable`.
- Any flag that only makes sense while the base setup runs — `--non-interactive`,
  a choice flag, or any of `--no-pam` / `--no-systemd` / `--enroll` / `--no-enroll`
  — forces the base setup to run, and the requested actions run **in addition**.

Consequently `setup --systemd --pam` now runs both (it previously dropped
`--pam`), and `setup --non-interactive --pam` now runs the base setup plus PAM
(it previously dropped `--non-interactive`). Both were silent flag drops.
`--remove` and `--service` require `--pam`, and `--disable` requires `--systemd`,
so a dropped flag is now a parse error rather than silence.

`--if-present` requires `--pam` and applies to the add side as well as
`--remove`. "Configure hyprlock if this machine has hyprlock" is the same
question in either direction, and it is what a provisioning script over a set
of optional integrations is asking. The flag turns a missing target service
file from an error into a successful no-op and does nothing else; read, parse
and write failures remain fatal, and without it both directions keep their
historical missing-file error. The exit code is `facelock pam`'s and identical
on both: an absent service is reported `absent` and the alias exits 0.

The flag is not a way around a service that *should* resolve. Since the search
path took in the vendor directories, `--service polkit-1` finds
`/usr/lib/pam.d/polkit-1` on a stock Arch box without it. Reserve
`--if-present` for services a machine may genuinely not have. The default stays
a hard error, which is what catches `--service polkti-1`.

**`--pam` is an alias onto `facelock pam add` / `facelock pam remove`.** The
plan resolution above stays on `setup` — `--pam`, `--no-pam`, `--service`,
`--remove`, `--if-present` and their precedence rules are unchanged — and only
the execution moved. The alias is exact, including the two things that make it
not a plain forward:

- **`setup --yes` maps onto `--no-confirm` only.** It suppresses the ordinary
  per-file question and does not authorize a sensitive PAM mutation.
  `--non-interactive` has the same prompt-only effect, as it always has.
  `setup --pam --allow-sensitive` maps onto the writer's separate
  authorization and does not suppress the question. The flag conflicts with
  `--remove`, whose safe direction is never sensitive-gated.
- **The root refusal is a hard error, not a `sudo` re-exec.** Standalone
  `--pam` never offered the interactive escalation (`needs_root_precheck`), and
  `facelock pam add|remove` does not either.

Supplying a choice flag suppresses the corresponding wizard step. `auto` means
"re-derive from hardware", **not** "use the default" — omitting the flag already
gives the default. Under `--non-interactive`, an unresolvable choice is an error,
never a prompt.

Setup's automatic camera choices apply the same post-classification decodability
predicate as device.path-unset auto-detection. The interactive wizard excludes
nodes that advertise none of GREY/Y16/YUYV/NV12/MJPG before it auto-selects or
presents candidates, and `--camera auto` considers only the remaining
IR-classified nodes. Exclusion never reclassifies a node: an IR node whose only
formats are Y8/Y10/Y12 remains IR, but setup reports its path and advertised
formats and does not select it. GREY and Y16 remain eligible. If
`security.require_ir = true` and IR-classified nodes were detected but none has
a decodable format, the wizard aborts setup before constructing its menu,
reports every excluded IR path and format, and never presents, recommends, or
persists an RGB fallback; unrelated camera-enumeration and prompt failures
retain the wizard's recoverable camera-step behavior. With `require_ir = false`,
decodable RGB nodes remain explicit wizard choices. `--camera auto` likewise
errors instead of falling back to RGB when no usable IR candidate remains. An
explicit `--camera /dev/videoN` remains an operator override and is still
subject to the auth/open fail-closed checks.

### facelock pam Semantics

`facelock pam add | remove | status` and the machine-wide
`facelock pam remove --all` cleanup own every direct write to `/etc/pam.d`.
`setup --pam` is an alias onto it (above), and the wizard's step 9 calls the
same writer, so there is one implementation of the edit and one set of rules.

**Resolution order: `/etc/pam.d`, then `/usr/lib/pam.d`. First hit wins.** That
is Linux-PAM's own precedence, and it is not academic: on current Arch `polkit`
ships its configuration as `/usr/lib/pam.d/polkit-1` and `/etc/pam.d/polkit-1`
does not exist, so a writer that looked only in `/etc/pam.d` could not
configure the service at all. The list is `[pam] config_dirs` (Config Schema
below) for a distribution whose vendor directory is somewhere else; there is no
way to ask Linux-PAM at run time which one it was compiled with, so the default
for named add, remove and status is the pair above and configuration is never
required. Machine-wide `remove --all` uses its own fixed roots described below.
A hit that is *refused* —
a hard link or any symlink — is still a hit: the search does
not fall through to the next directory, because that would let a vendor file
silently take over from an `/etc` entry facelock declined to follow.

**Only the first directory is ever written to.** The rest are package-owned: an
edit there is clobbered by the next upgrade and makes `pacman -Qkk` report a
modified file. A service that resolves only in a vendor directory is **copied**
into `/etc/pam.d` with the facelock line already in it — one atomic write of
the final content, not a copy followed by an edit — and the copy carries a
two-line provenance header naming the file it was forked from and saying that
it shadows it and will not track vendor updates. The copy reports `overridden`
rather than `installed`, and the operator is told at the time, on the
unsuppressible `notice` stream, because a new shadowing file in `/etc` is a
durable change with a maintenance consequence. Deleting the override restores
the vendor file. Named `pam remove` deletes an unchanged Facelock-created
override after taking out its line; header, payload, metadata, identity or
current-vendor drift keeps the no-rule local override and reports why. If no
current vendor source exists, an exact header path derived from a normalized
configured later-root candidate is recognition only: removal retains the
local override and reports that the source is absent. An arbitrary path in a
header is never authority. The
vendor file is never read-modified-written, never backed up, and never renamed
over.

**The module is probed too, and that is a different list.** The service-file
order above says where a *service file* is looked up; the module
`pam_facelock.so` is looked up in `/lib/security`, then `/usr/lib/security`,
then `/usr/lib64/security`, first hit wins. Two lists for two things: one is
configuration, the other is a shared object, and they are never merged.
`/lib/security` is first so the answer on usrmerged Arch is unchanged;
`/usr/lib64/security` is where `dist/facelock.spec` installs on x86-64 Fedora
and RHEL, which is why the single hardcoded path was a refusal-to-write on the
distribution this repository ships a spec file for. There is deliberately no
Debian multiarch triple: Debian's idiomatic path is `pam-auth-update`, which is
out of scope for this command. The probe is **read only** — it finds the
module, and never installs, copies or links it. When it finds nothing, `add`
refuses and the refusal names **every** candidate, so an operator on an
unlisted layout can see what to add. The list is not configurable.

**A vendor file that already carries the line needs no override.** `add`
reports `unchanged` and writes nothing, and `status` reports `present`: a
distribution that ships face auth in its own PAM stack is configured, and
saying otherwise would send an integrator off to create a copy that adds
nothing.

**Managed shared stacks are not leaf services.** Debian and Ubuntu compose
their shared stacks with `pam-auth-update`; Fedora and RHEL use `authselect`.
Facelock does not write through either manager's generated shared files.
Before any direct `pam add` or `setup --pam` plan, Debian's detector reads only
the compiled `/usr/share/pam-configs/facelock`, `/var/lib/pam/auth`, and
`/etc/pam.d/common-auth` locations. It requires root-owned, non-writable,
regular single-link files opened beneath no-follow directory descriptors, the
exact packaged profile bytes, an exact `Module: facelock` selection, and a live
Facelock rule inside pam-auth-update's Primary block. An active profile refuses
the direct edit before backup state is created and says exactly how to run
`sudo pam-auth-update --disable facelock`, verify password authentication, and
retry the original Facelock command with all services and flags intact.
Selected-but-inconsistent or untrusted state fails closed rather than being
treated as inactive. A live managed rule whose saved selection is absent fails
closed as well. The roots are not configurable and the environment cannot
redirect them. Explicit named leaf services remain supported on every package
family: the writer resolves and edits only that requested service under the PAM
roots, while the sensitive-service gate and no-follow checks refuse generated
`system-auth` and `password-auth` links.

**Confinement.** A service name is **one path component**: not empty, no `/`,
not `.` or `..`, no interior NUL. Rejected before any I/O, on `add`, `remove`
and `status` alike. `base.join(service)` is not a confinement primitive — an
absolute name *replaces* the base — so this is the check, not the join.
Anything else is accepted: `PAM_CANDIDATES` is the wizard's menu, **not** an
allowlist, and a service that is not on it must keep working.

**Every symlinked service entry is refused by named add, remove and status.**
The writer `lstat`s the entry for diagnostics, but read, mutation and recovery
all reopen the confined service basename relative to an already-open PAM root
with `O_NOFOLLOW`; neither a resolved absolute target nor a target recorded in
provenance is ever opened.
This applies even when the link text appears to remain in the same directory.
It also prevents Facelock from editing generated authselect state through
`/etc/pam.d/system-auth` or `/etc/pam.d/password-auth`, and prevents a hand-made
`/etc/pam.d/polkit-1 -> /usr/lib/pam.d/polkit-1` link from turning a vendor file
into a write target. A symlink is a validation failure for the whole write run,
and `pam status` reports it as `unknown` with the retained fixed reason
`symlinked outside /etc/pam.d`; that token names the compatibility class, not a
claim that an in-directory link would be followed. The human diagnostic names
the link text and the directory whose entry was refused.

**A file with more than one hard link is refused.** A symlink is a visible
indirection this can reject by name; a second hard link says another name for
the inode exists but not where, so the edit cannot be shown to stay inside the
directory. `pam status` reports it as
`unknown` with the fixed reason `hard-linked service file`. This is
conservative rather than adversarial: a `/etc` that has been through a
deduplicating backup or `jdupes -L` can trip it with nobody attacking anything,
so the message says how to break the link. The atomic replace does **not**
retire the rule: a rename writes a new inode, so it leaves the other name
holding the *old* content — one of a file's names carrying the line and the
rest not is a worse answer than a refusal, and still a change to a file
facelock cannot name.

**`--if-present` does not forgive a link fault.** A dangling or looping symlink
is not an absent service file: absence is a fact about the directory, and an
unresolvable link is the absence of an *answer* about where a write would land.
Both are phase-one failures on `add` and `remove` under `--if-present`, and
exit 2 on `status --if-present`.

**The sensitive gate is applied before any write.** It checks the typed service
and the confined basename returned by resolution. Symlinks cannot provide an
alternate ungated name because every symlink is refused; the `system-auth-ac`
and `password-auth-ac` spellings remain explicit members because older
`authconfig` installations can use those names as real service files.

**Two-phase across services.** Every requested service is validated — name,
existence (subject to `--if-present`), the sensitive gate, and what the edit
would be — before **any** file is written. A validation failure writes nothing at all,
which is what makes a caller's loop all-or-nothing for the failure that
actually happens: a typo'd or gated service name. It is **not** a transaction:
a write-phase I/O error on service N leaves 1..N-1 written. Those are reported
per service and the exit code is non-zero; the remaining services are still
attempted. Each individual local mutation has its own serialized,
crash-recoverable transaction and rollback pair, described below. Named
`pam add` and `pam remove` do not use a whole-set journal; the compiled-root
`pam remove --all` transaction is specified separately below.

**`--no-confirm` never implies `--allow-sensitive`, including through
`setup --pam`.** They are separate
authorizations: "do not ask me" and "yes, edit the shared auth stack". The
gated services are `common-auth`, `login`, `password-auth`,
`password-auth-ac`, `sshd`, `system-auth`, `system-auth-ac` and
`system-login`. Six of the eight are *shared stacks* — files that other
service files `include`, so one edit reaches `su`, `passwd`, `chsh` and the
display manager at once — and which name a distribution uses is the only
difference between them (`system-auth`/`password-auth` on Fedora, RHEL and
Arch, the `-ac` spellings where `authconfig` wrote the real file,
`common-auth` on Debian and Ubuntu, `system-login` on Arch). Gating one
spelling made the gate depend on the operator's distribution. `login` and
`sshd` are the two that are not: each locks one specific door — the TTY, the
network — rather than every one at once. `--yes` and
`--no-confirm` are the same flag (the shared `ConfirmArg` spelling, so "skip
prompts" reads the same on `setup`, `pam add`, `remove` and `clear`) and neither
unlocks the gate. Both `pam add` and its `setup --pam` alias expose the same
explicit `--allow-sensitive` authorization. `remove` is never gated **on
sensitivity** — removal can only take
away a way to authenticate — and never prompts, which is what
`setup --pam --remove` has always done; the confinement rules below apply to
every verb, `remove` and `status` included. `--yes`/`--no-confirm` is accepted there for symmetry and has
nothing to suppress today.

**With no TTY on stdin, `pam add` proceeds as if `--no-confirm` were given.**
A question nobody can answer is a hang, not a safeguard, and this is what has
always made `setup --pam` work from a provisioning script — so
`sudo facelock pam add --service sudo < /dev/null` writes without the flag.
The prompt this skips defaults to yes, so the flag changes nothing about the
outcome on a TTY either; what it changes is whether you are asked. This never
touches `--allow-sensitive`: the sensitive-service gate is decided in the
validation phase, before any prompt exists to skip, so an unattended
`pam add --service system-auth` still refuses.

**Exit codes.**

| Command | Code | Meaning |
|---------|------|---------|
| `pam status` | 0 | every requested service carries the line |
| `pam status` | 1 | at least one requested service exists without it, in `/etc/pam.d` (`missing`) or only in a vendor directory (`vendor-only`) |
| `pam status` | 2 | at least one is absent, unreadable, misnamed, symlinked, or hard-linked |
| `pam status --if-present` | 0 | every requested service **that exists** carries the line |
| `pam status --if-present` | 1 | at least one existing service carries no line |
| `pam status --if-present` | 2 | as above, minus the absent case, which no longer forces 2 |
| `pam status --all` | 0 | at least one service carries the line, and every directory was read |
| `pam status --all` | 1 | nothing on the machine carries it, or an enumerated service has no line in the file Linux-PAM reads |
| `pam status --all` | 2 | a directory could not be listed, or an enumerated service could not be answered for |
| `pam status --all --if-present` | 0/1/2 | unchanged from `--all`: an enumerated name was found, so there is no absent case to forgive |
| `pam add`, `pam remove` | 0 | every service reached its requested state and required default rollback-state cleanup completed — including `unchanged`, `overridden` (`add` created the `/etc/pam.d` copy), `vendor-only` (`remove` had nothing of its own to take out of a package-owned file), `absent` under `--if-present`, and `declined` |
| `pam add`, `pam remove` | non-zero | a validation failure (nothing written) or a write failure, including `cleanup-failed` after the requested PAM state was reached |
| `pam remove --all` | 0 | every recognized writable direct reference was removed, the final compiled-root scan was clear, and the whole-set transaction committed and cleaned; an empty scan is an idempotent success |
| `pam remove --all` | non-zero | preflight, journal, identity, write, final-rescan or recovery failure; direct PAM mutations are rolled back where their exact identities prove that safe, otherwise transaction evidence is retained |

`pam status` is on `grep`'s scale and `is-enrolled`'s: a boolean query whose
exit code is the answer. Across several services the worst outcome wins. A
**declined** confirmation is exit 0, since the command did what the operator
asked, and `--json` is how a script tells it from an install.

**`--dry-run`** prints the resolved plan, writes nothing, and exits 0. It is
honoured *after* the root check (see DEC-6 above). On `remove --all` it performs
no transaction recovery and creates no state.

**A service that exists in no directory names them all.** The refusal `add`
and `remove` raise, and the line `status` prints, both list every path tried:
the same question must not be answered two ways by two verbs. The machine
`path` field on a `status` row stays a single path — the first directory's,
where an override would go — because the field is one string and always has
been.

**`--if-present` means the same thing on all three verbs.** A service file that
is not there is not an error: on `add` and `remove` the service is reported
`absent` and the exit code is unaffected, and on `status` the `absent` row no
longer forces exit 2, so the exit code is decided by the services that do
exist. That is what lets "install the optional integrations with
`--if-present`, then verify" be written as a pair. It converts *absence* and
nothing else — an unreadable file, a rejected name, or a link out of the
directory is still an error on every verb.

**`pam status --all` reports every configured service; a bare `pam status`
still means `sudo`.** Without `--all` the command answers only about names it is
given, so a configured `polkit-1` or `omarchy-lock-face` is invisible to it.
`--all` replaces the service list with every service in the resolved
directories whose file names `pam_facelock.so`. It is a flag rather than a new
default because a bare `pam status` exits 0/1/2 *about `sudo`* today, and an
integrator branching on that would have got a different answer without changing
a command line. `--all` and `--service` are mutually exclusive: enumerating and
naming are two questions, and a request that asked both would have to drop one
silently.

**The scan parses; it keeps no manifest.** A state file listing what facelock
has edited drifts the moment anyone edits `/etc/pam.d` by hand, restores a
backup, or removes a package, and the report is then confidently wrong. Names
are collected across every directory and then resolved through the rules above,
so `--all` and `--service X` cannot answer differently about one service. Four
consequences:

- a **vendor file carrying the line while an `/etc` file shadows it without
  one** is reported `missing`. The file Linux-PAM reads has no line in it, and
  dropping the name would hide the one machine an operator cannot otherwise
  explain.
- an entry the resolver refuses (symlinked or hard-linked)
  is an `unknown` row with its usual reason: never followed, never dropped.
- a service file that could not be **read** is an `unknown` row too. Omitting it
  would report "not configured" for a machine this could not check.
- `.facelock-backup`, `.pacnew`, `.pacsave`, `.pacorig`, `.rpmnew`, `.rpmsave`,
  `.rpmorig`, `.dpkg-old`, `.dpkg-new`, `.dpkg-dist`, pam-auth-update's
  `.pam-old`, names ending in `~`, and dotfiles are not services. Each can carry
  the line, and none is a name Linux-PAM is ever asked for.
- only a **regular file** is read. A FIFO blocks the read until a writer
  appears, and a diagnostic command that hangs on a malformed `/etc/pam.d` is
  worse than one that omits an entry no PAM stack could use; device nodes,
  sockets and symlinks to directories go the same way. The check is on the
  *followed* metadata, since a symlink is how a non-regular file reaches the
  scan. **"Not a regular file" and "could not be examined" are different
  answers, and only the first is a skip**: an entry whose `stat` fails — a
  symlink into a directory the caller may not traverse, a symlink loop, a dead
  network mount — is carried into the report as `unknown`, the same answer
  `--service` gives for it. The one exception is `ENOENT`, which is an absence
  and is skipped like any other file that is not there; a dangling symlink is
  therefore absent from an `--all` report while `--service` on the same name
  reports `unknown`, because a link pointing at nothing carries no facelock
  line but is still an entry the writer refuses to follow.
- an entry whose name is not valid UTF-8 is skipped and logged. Spelling it
  lossily would hand the resolver a name no file has and report a configured
  service as `absent` at a path that does not exist.

**A directory that could not be listed is reported, not treated as empty.**
"Nothing is configured here" and "I could not look here" are different answers,
and rendering them identically is what made a broken lock stack and a healthy
one look the same. Every directory searched appears in the `--all` document
with a `status` of `scanned`, `absent` or `unreadable`, and an `unreadable` one
makes the exit code 2 whatever the services said. An **absent** directory is
not an error: one that does not exist demonstrably holds no service files, and
the default search path names a vendor directory many machines lack, so
treating that as unanswerable would make every one of them exit 2 forever.

**Nothing configured is exit 1.** A machine with no facelock line anywhere is
not configured, which is the answer `pam status` already gives for a service
file with no line in it. `--if-present` does not change it. A name reaches an
`--all` report by having been found, so there is nothing for the flag to
forgive, and `pam status --all --if-present` on an unconfigured machine exits 1
like the bare form. One state produces an `absent` row anyway: a file deleted
between the listing and the read. It is the only one, and `--if-present` scores
it 0 there as everywhere else.

**The empty answer is scoped to what could be read.** "No service file under
these directories carries the line" is a claim, and it may only name directories
that were listed or proven not to exist. When some directory could not be read,
the sentence names the ones that could and says in the same breath which could
not; when *none* could be read there is no set to make the claim about, so only
the per-directory lines are printed. Without that scoping the sentence read
under `2>/dev/null` asserts exactly what `--all` exists to stop it asserting.

**`facelock status` summarizes the same scan.** Its report carries one
`PAM services:` line built by running the scan above, so the two commands
cannot disagree about whether a service is configured. The detailed listing
stays in `pam status --all`. The summary keeps the "not checked" distinction:
it reads `none configured` only when every directory was read, `not checked`
when nothing was found and something could not be, and it names each unread
place on a `not checked:` line of its own.

**PAM publication is complete-file atomic and identity-checked.** Planning
captures the regular, single-link service file's device, inode, link count,
SHA-256 hash, exact mode, UID and GID. Stable identity comparisons bind all
seven values; timestamps and other unstable metadata are deliberately
excluded. Immediately before publication the writer reopens the confined
basename beneath its configured PAM write root and checks that same identity.
An existing local file is replaced with `RENAME_EXCHANGE`, leaving the exact
displaced inode named and open for a final check. There is an unavoidable
bounded interval after the exchange in which the complete replacement is the
canonical service file before that displaced-original check completes. If an
administrator or package replaced the file at the boundary, a second exchange
restores that complete intervening file; only a verified displaced inode is
unlinked. Neither side is ever partially written, and PAM's password fallback
is unchanged.

The replacement file preserves the existing file's owner, exact mode and
SELinux context. Ownership is applied before the final mode because `fchown`
can clear setuid/setgid bits. POSIX ACLs and xattrs other than the SELinux label
are not carried across. A vendor-only service is rechecked by identity and hash
immediately before a complete local override is published with
`RENAME_NOREPLACE`; an override that appeared after planning is preserved. The
new override takes the vendor file's owner and exact mode, but deliberately
does **not** copy the vendor file's SELinux xattr: the destination directory's
type transition supplies the local label. Each new file and its parent
directory are fsynced at the committed boundary.

**Rollback state has a fixed root.** An in-place `pam add` writes the original
service bytes under `/var/lib/facelock/pam-backups`, a `0700 root:root`
directory independent of both `[pam].config_dirs` and `storage.db_path`. The
production store opens that fixed directory without following the final path
component, applies root ownership before the final mode, and verifies exact
directory type, `0700` mode and `root:root` ownership before accepting it. It
reopens, locks and verifies the descriptor again before trusting state in a
transaction. State-entry authority is the fixed expected root owner, never the
owner observed on the directory. Only explicitly injected setup/test roots use
the process EUID and EGID as their expected state owner.

The published backup and its adjacent JSON record are regular, single-link
`0600 root:root` files. Their exact basenames are
`<service>.<seconds>-<nine-digit-nanoseconds>` and that basename plus `.json`;
seconds are decimal and in range for `u64`, and nanoseconds are decimal,
exactly nine digits and below one billion. Collision probing is bounded,
advances the nanoseconds and publishes with no-clobber semantics. A failed
publication cleans only an identity created and validated by that transaction,
never a pre-existing name.

Version 1 provenance JSON is strict and rejects unknown fields. It contains
exactly `version`, positive `sequence`, `state` (`prepared` or `committed`), a
confined `service`, the exact `backup` basename, `original_sha256` and
`installed_sha256`; both hashes are 64 hexadecimal characters, and no target
path is stored. A record can participate in sequence allocation, duplicate
detection, newest-backup reporting or cleanup only as part of a complete
record/backup pair whose names and schema agree, whose backup hash validates,
whose entries have the fixed expected state owner, mode `0600` and one link,
and whose record and backup can be read within 16 KiB and 1 MiB respectively.
Duplicate sequences make every pair at that sequence ambiguous for service
selection and cleanup. Sequence allocation checks overflow, and the newest
committed backup is selected by validated sequence rather than wall-clock
filename order.

**Every multi-name mutation has a strict durable intent.** Version 1 intent
JSON rejects unknown fields and always carries `version`, `role`, positive
`sequence`, confined `service`, strict transaction basename in `backup`,
`original_sha256`, `installed_sha256`, nullable `record_sha256` and
`replacement_record_sha256`, and nullable `original_device`, `original_inode`,
`original_links`, `original_mode`, `original_uid` and `original_gid`. Option
fields are serialized as JSON `null`. Each intent must be a regular,
single-link, state-owner `0600` file no larger than 16 KiB, every present hash
must be 64 hexadecimal characters, and every present mode must identify a
regular file. Fields that are irrelevant to the selected role must be `null`;
any other combination invalidates the intent. For mutation-only roles the
`backup` value is a collision-resistant operation identifier, not a claim that
a rollback pair exists.

| JSON `role` | Required role-specific fields |
|-------------|-------------------------------|
| `prepare`, `cleanup` | `record_sha256` present; replacement-record hash and all six original identity/metadata fields null |
| `commit` | record and replacement-record hashes present; all six original identity/metadata fields null |
| `pam_replace` | record hash present; replacement-record hash null; exact original device/inode, `original_links = 1`, mode, UID and GID |
| `pam_remove` | record hashes null; exact original device/inode, `original_links = 1`, mode, UID and GID |
| `vendor_create` | record hashes and original device/inode/links null; original mode/UID/GID present as the expected newly-created destination metadata |

**A strict publication binding authenticates every created inode.** Version 1
binding JSON rejects unknown fields and contains exactly `version`, `role`, a
positive `u64` `sequence`, confined `service`, strict transaction basename in
`backup`, `intent_sha256`, `device`, `inode`, `links`, `sha256`, `mode`, `uid`
and `gid`. JSON `role` is one of `commit`, `pam_replace`, `pam_remove` or
`vendor_create`. Both hashes are 64 hexadecimal characters, `links` is exactly
one, and `mode` identifies a regular file. The binding itself must be a
regular, single-link, fixed-state-owner `0600` file no larger than 16 KiB.

While the base intent exists, its exact bytes must hash to `intent_sha256`, and
its role, sequence, service and operation basename must agree with the binding.
The bound replacement hash must also equal the commit intent's replacement
record hash or, for a PAM/vendor mutation, its installed hash. After the named
replacement temp exists, Facelock publishes the binding atomically with
no-clobber semantics before the `RENAME_EXCHANGE` or `RENAME_NOREPLACE` that
makes the replacement canonical. The full identity is captured from the
still-open created temp only after its bytes and requested metadata/context are
applied, the file is synced, and its desired hash, mode, UID, GID and
single-link count are validated. Facelock then reopens the exact reserved temp
basename and full-compares it before publishing any binding. A failure at this
boundary uses identity-checked cleanup; any reopen, comparison or cleanup
uncertainty is ambiguous and preserves the base intent and filesystem evidence.
A crash in the gap before the binding is published leaves the base intent and
unbound temp conservatively preserved.
A synchronous no-clobber binding-publication failure is distinct from that
crash: it preserves the colliding administrator state entry and reopens and
full-compares the unpublished replacement temp. Facelock may remove the base
intent only after the exact temp unlink and parent-directory fsync succeed.
Any reopen, full-identity, unlink or fsync ambiguity returns
`AmbiguousPublication` and retains the base intent and colliding administrator
evidence. The temp is also retained unless its exact identity-checked unlink
succeeded and only the subsequent parent-directory durability sync failed; in
that case the temp name may already be absent. Commit, PAM replacement/removal
and vendor creation all use this same checked cleanup primitive.
The same full-identity cleanup applies after the binding is durable if source
drift or an exchange/no-replace failure prevents PAM or vendor publication.
Any uncertainty returns `AmbiguousPublication` and preserves the base intent,
binding and all remaining evidence, subject to the same post-unlink fsync
boundary above.

The reserved name grammar is exact:

- intents: `.facelock-intent-{prepare|commit|cleanup|pam-replace|pam-remove|vendor-create}-<transaction>.json`
- publication bindings: `.facelock-publication-{commit|pam-replace|pam-remove|vendor-create}-<transaction>.json`
- state quarantine: `.facelock-quarantine-backup-<backup>`, `.facelock-quarantine-record-<backup>.json`, `.facelock-quarantine-commit-<backup>.json`
- PAM-directory temps: `.facelock-pam-replace-<transaction>`, `.facelock-pam-remove-<transaction>`, `.facelock-vendor-create-<transaction>`
- PAM-directory vendor-retirement quarantine: `.facelock-vendor-retire-<transaction>`
- atomic state temps: `.facelock-tmp-<destination>-<64hex-content-hash>-<pid-digits>-<nanos-digits>`

The strict atomic-state-temp destination grammar includes publication-binding
destinations; a binding temp is not authenticated by its prefix alone.
Backup and record temp destinations additionally require a confined service
component, so empty, `.` and `..` services are never owned.

A reserved-looking name alone never establishes ownership. The applicable
intent or pair schema and derived names, the owner/mode/link requirements for
that role, the bounded content hash and, where applicable, the captured PAM
identity and metadata must all agree before Facelock resumes or removes it.
Every state match, committed-record transition, quarantine move and unlink
rechecks that the entry is single-link, state-owner and mode `0600` in addition
to its content and identity. Same-inode, same-content mode or ownership drift
is ambiguous and is preserved. State publication, vendor publication, state
quarantine and vendor-retirement quarantine moves use `RENAME_NOREPLACE`;
committed-record and existing-PAM transitions use `RENAME_EXCHANGE`.

If an atomic state temp-to-final `RENAME_NOREPLACE` succeeds but syncing the
parent directory fails, the result is `AmbiguousPublication`, not an ordinary
create failure, and every caller propagates it before cleanup. Prepare retains
its durable intent plus the visible final backup or record. Commit-replacement
publication retains the commit intent plus its named replacement. If the
destination is a publication binding, each of `commit`, `pam_replace`,
`pam_remove` and `vendor_create` retains its base intent, exact replacement temp
and visible binding. Checked cleanup is limited to definite failures before the
rename; strict identity binding lets recovery classify and complete each
retained evidence set.

After exchange or publication, the canonical file is reopened and compared
against the binding's full device, inode, link count, hash, mode, UID and GID.
This happens immediately after publication, immediately before a displaced
inode is unlinked where that boundary exists, and immediately before the base
intent is cleaned; commit checks once more after unlinking its displaced
prepared record. A canonical mismatch preserves the canonical name, any
remaining temp or displaced name, the base intent and the binding for manual
inspection. These checks do not weaken complete-file atomicity or PAM password
fallback.

**One state-directory flock spans a local mutation.** For an in-place add it
covers recovery, bounded timestamp and sequence allocation, durable prepare
intent and rollback-pair publication, PAM intent/temp/exchange, and committed
record intent/exchange. Local remove and vendor-create planning, publication
and recovery run under the same guard; unchanged vendor-override quarantine,
validation, deletion or restoration completes before the local-remove guard is
released. A competing writer or recovery cannot
discard an add's prepared pair between persistence and commit. Backup cleanup
after a successful or no-op remove is a separate locked quarantine phase; this
does not make named mutations multi-service atomic. The `pam remove --all`
transaction below keeps its whole-set lock through batch cleanup.

Recovery treats state as an untrusted hint and re-resolves only the recorded
confined service under the current PAM write root. It recovers publication
bindings before base intents. A bound commit, PAM or vendor canonical candidate
must match the exact full created identity in its binding; an original PAM
candidate must match the exact original identity captured in its base intent.
While both state files exist they must bind to one another as described above.
Without a valid binding, a named or published replacement is ambiguous and is
preserved; only a clearly pre-temp base-intent shape is cleaned.

The base intent is removed first and the self-contained binding last. Recovery
considers a binding orphaned only when the exact derived base-intent name is
definitely absent. An invalid-mode, invalid-owner, malformed, mismatching,
symlinked, or hard-linked exact entry blocks destructive binding recovery. If
a crash truly leaves an orphan after the base unlink, recovery removes it only
when the canonical file still matches the full bound identity and the temp is
absent. A mismatch preserves the binding and every ambiguous name. A crash
after the binding unlink leaves no publication-state debris.

Prepare recovery handles an intent alone, a backup alone, a record alone or a
complete pair. Bound commit recovery distinguishes pre-exchange, exchanged and
displaced-record-unlinked boundaries. Cleanup resumes the no-replace
backup/record quarantines and identity-rechecked unlinks. Bound PAM
replacement/removal recovery distinguishes an intent alone, a ready temp and
the exchanged canonical/displaced pair. A remaining ambiguous `pam_replace`
intent also blocks generic prepared-pair recovery, preserving the rollback
pair. Bound vendor-create recovery handles absent/temp and published/absent
boundaries. A wrong owner, mode, link count, schema, hash or identity, an extra
conflicting entry, a symlink/hard link, or any other ambiguity is preserved for
manual inspection.

`pam remove` takes no new rollback copy. After every successful or no-op
removal it deletes validated committed Facelock-owned pairs for that service and the
exact legacy `<service>.facelock-backup` entry by default. Pair deletion first
moves the record and backup to no-replace quarantine names, then rechecks the
identities before unlinking. Legacy cleanup is confined to the override root
and rechecks the exact regular, single-link entry immediately before unlink.
Malformed provenance, lookalike names, symlinks, hard links, changed entries
and unrelated administrator files are retained. `--keep-backup` opts out of
both versioned and legacy cleanup.

For a local copy created from a vendor-only service, named `pam remove` first
publishes the complete document without the Facelock rule through the existing
`pam_remove` exchange protocol. It then deletes the local override only when
the exact two-line Facelock header names the current vendor service. Current
vendor resolution reopens the configured later roots in order and stops at the
first existing entry; a malformed, linked, unreadable or oversized first entry
is a blocker rather than permission to accept a matching lower-priority file.
The remaining payload and mode/UID/GID must equal that bounded, regular,
single-link vendor file, the vendor file must contain no active Facelock rule,
and the complete local bytes must be either the exact header plus the one
document emitted by Facelock's insertion or the exact header-plus-vendor
no-rule restart shape. The journal backup used by batch cleanup is likewise
reopened within its size bound and must retain its full prepared identity
before its header is parsed.

The canonical local inode must still have the full identity captured by the
removal publication. Facelock moves that exact basename to the derived
`.facelock-vendor-retire-<transaction>` quarantine with a no-replace rename,
syncs the directory, and rechecks the quarantined identity, canonical absence,
exact emitted shape and ordered current vendor before identity-checked unlink.
If the local or vendor check fails while the canonical name remains absent,
Facelock restores the exact quarantine with a no-replace rename. A concurrent
canonical entry, quarantine collision, reopen failure, identity mismatch or
durability uncertainty preserves all available names and durable publication
evidence as ambiguous. Every quarantine, restore and unlink boundary is
restartable. Both service names are re-resolved beneath already selected PAM
roots; the header is a recognition hint, never a path to open. Header, payload,
metadata, vendor-source or identity drift preserves the local override after
removing its Facelock rule and reports that decision. When no current vendor
entry resolves, only a header naming a normalized path derived from a
configured later root is recognized, and it causes explicit retention rather
than deletion; the header path is not opened or followed. A restart may finish the
same exact deletion when the rule-removal exchange completed but the
header-bearing local override is still present.

**`pam remove --all` is the config-independent whole-machine cleanup.** It
ignores `[pam].config_dirs` and opens the compiled roots `/etc/pam.d`
(writable override), `/usr/lib/pam.d` (detection-only vendor state), and
`/etc/authselect` (detection-only generated state). Missing or corrupt config,
database, model, camera, daemon and ONNX Runtime state cannot redirect or block
it. It enumerates already-open directory descriptors and re-resolves each
confined basename with directory-relative, no-follow operations. A candidate
must remain a regular, single-link file whose bounded bytes and complete
identity can be rechecked. Directory contents are detection ground truth;
provenance is untrusted ownership evidence, never a target path.

No symlink is followed. A symlink is skipped only when its text is the exact
absolute path of the same service basename beneath a later compiled root that
this run scans independently. This accounts for stock links such as
`/etc/pam.d/system-auth -> /etc/authselect/system-auth` without trusting the
link's contents. Every other symlink, hard-linked, nonregular or unreadable PAM
entry that contains or could hide a Facelock reference is an unmanaged blocker.
A reference found in the independently scanned `/usr/lib/pam.d` or
`/etc/authselect` root is likewise an unmanaged read-only or external-root
blocker. Structural directories, including authselect profile directories, are
not PAM service files and are skipped. Nothing outside the fixed roots is
followed or deleted.

A writable direct file with a conventional PAM service basename is recognized
as Facelock-owned when every Facelock logical rule uses the exact pre-versioned
physical bytes `auth      sufficient pam_facelock.so`. Dot-prefixed names and
the administrator/package artifact suffixes `.facelock-backup`, `.pacnew`,
`.pacsave`, `.pacorig`, `.rpmnew`, `.rpmsave`, `.rpmorig`, `.dpkg-old`,
`.dpkg-new`, `.dpkg-dist`, `.pam-old`, and `~` are not conventional legacy
candidates.
They are considered only when a strict provenance basename exists for that
exact confined service and the current complete-file hash equals
`installed_sha256` in its validated committed pair, or when the regular local
file carries the exact Facelock vendor-copy header and matches its current
fixed-root vendor source as specified above. This lets `remove --all` find
every arbitrary name accepted by either named writer path without treating an
unowned administrator artifact as an active PAM service. Customized controls,
options or spacing, corrupt or ambiguous provenance for a candidate, invalid
bytes, path escapes, link swaps, identity drift and concurrent edits block the
whole run during preflight. Nothing is changed when preflight finds a blocker.
An empty scan is an idempotent success.
The initial cleanup scan also recognizes that exact unchanged header-bearing
vendor override after its Facelock rule is already absent. This is the bounded
restart shape of the named removal above; other no-rule files are ignored and
preserved. The final active-reference scan excludes this cleanup-only shape.

**The whole set is journaled before the first PAM mutation.** Version 2 state
is strict JSON with unknown fields rejected, regular single-link fixed-state
owner and mode `0600`, no-clobber publication, a 4 MiB encoded limit and at
most 1,024 unique confined services. Its reserved names and exact fields are:

- `.facelock-remove-all-<operation>.json` contains exactly `version`,
  `operation`, `keep_backup`, and `targets`. Each target contains exactly
  `service`, strict `backup`, `original`, `installed_sha256`, and the required
  boolean `delete_override`; `original` contains exactly `device`, `inode`,
  `links`, `sha256`, `mode`, `uid`, and `gid` and must describe a regular
  single-link file.
- `.facelock-remove-all-commit-<operation>.json` contains exactly `version`,
  `operation`, `journal_sha256`, `keep_backup`, and `targets`. Each target
  contains exactly `service`, strict `backup`, `installed`, and the required
  boolean `delete_override`, where `installed` uses the same complete-identity
  fields and validation.

Version 1 journal and commit files remain recoverable only when every target
omits `delete_override`; version 2 requires it on every target. JSON `null` is
not an absent field. The corresponding journal and commit flags must match.

`operation` is `<seconds>-<exactly-nine-digit-nanoseconds>`: seconds parse as
`u64`, nanoseconds are below one billion, and collision allocation is bounded.
Only a prefix plus that valid operation grammar is reserved batch state; a
prefix-shaped strict provenance basename with another suffix remains ordinary
per-service provenance. Both state files and every hash are bounded and
validated, and duplicate services invalidate either target list before
recovery. A commit pairs with a journal only when operation, keep flag, ordered
service/backup set, exact journal hash and planned/committed installed hashes
and `delete_override` flags agree. Multiple, malformed or conflicting reserved
entries require manual review.

`pam remove --all --dry-run` opens an existing backup directory for read-only
inspection and requires its owner and mode to be trusted already. It performs
no owner/mode repair, directory sync, recovery or write locking; an untrusted
directory makes the preview fail closed with its metadata and entries intact.

One fixed-state-directory flock spans recovery of any earlier journal, the
authoritative root scan and complete preflight, bounded operation and rollback
pair allocation, publication of every #171 backup/provenance pair, journal
publication, every PAM exchange, the final fixed-root active-reference scan,
commit publication and evidence cleanup. Every per-service rollback pair is
durable before the journal, and the complete journal is durable before the
first PAM mutation. Each service then uses the #171 intent, publication
binding, exact created-identity and `RENAME_EXCHANGE` protocol while retaining
the displaced original inode.

A later failure or a non-empty or unanswerable final scan reverse-exchanges
every earlier file in reverse order after complete identity checks. Recovery
does the same for a prepared journal without a valid commit. The strict,
self-contained commit authenticates the journal bytes and every installed
complete identity; once it is durable, recovery finishes per-file publication
cleanup and validated backup cleanup instead of rolling the PAM files back.
Any mismatch or ambiguity preserves the journal and per-file intent, binding,
temp or displaced evidence, including administrator bytes, for review. Files
are always published as complete byte sequences and PAM password fallback is
unchanged.

Prepared-journal recovery also recognizes the one provably unstarted
per-service publication shape: the canonical service still has the journal's
full original identity, the exact valid `pam_replace` intent agrees with the
prepared pair's sequence and record hash, and the exact replacement temp and
publication binding are both absent. It identity-checks and removes only that
intent before continuing rollback. Any mismatch or extra name remains
ambiguous. After reverse exchange and identity-checked replacement-temp
cleanup, rollback removes the exact publication binding before delegating the
remaining base intent to that intent-only recovery. Every boundary is
restartable; normal forward publication retains its existing intent-first
cleanup order. Rollback-pair cleanup is restart-idempotent across cleanup intent,
both quarantine moves and both unlinks. An exact matching cleanup intent is
resumed; a target whose canonical pair, quarantine pair and exact cleanup
intent are all absent is already clean. Partial, substituted or conflicting
pair state is preserved and blocks journal cleanup.

After the batch commit marker is durable and per-file publication evidence is
finalized, each target with `delete_override = true` is unlinked through the
writable-root directory descriptor only if its full committed identity still
matches. The corresponding journaled original backup must retain the full
identity captured by the prepared pair and parse as an exact Facelock-emitted
one-rule copy or exact no-rule restart shape. Its line-removed SHA-256 must
equal the journal target's installed hash. The header must name the first
existing service in the ordered later roots, whose bounded, re-opened regular
single-link entry must still match the payload and mode/UID/GID and contain no
active Facelock rule. Facelock never opens an arbitrary path from the header.
The same no-replace vendor-retirement quarantine protocol above performs the
committed deletion. Any mismatch preserves the override and the batch
journal/commit evidence. A crash after a checked unlink is restartable: absence
is accepted only for a committed `delete_override` target, while any partial,
substituted or unflagged absence remains ambiguous.

On success, default cleanup removes only validated Facelock-owned versioned
pairs and exact validated legacy `<service>.facelock-backup` state for every
committed target. `--keep-backup` instead commits and preserves the new pairs
and opts out of legacy cleanup. `--json` uses the standard single remove
document with one committed `removed` row per target (and a backup path only
under `--keep-backup`); an idempotent no-op has an empty `services` array.
`--quiet` suppresses that document.

**Every shipped uninstall surface delegates to this cleanup before deleting
the binary or PAM module.** This includes the Arch source and binary packages,
Debian `prerm`, RPM `%preun`, the Omarchy remover and `just uninstall`. Booted
package coverage exercises direct `dpkg`/`rpm` and their `apt-get`, `apt`, and
`dnf` frontends for abort retention and blocker-free success. Arch
also ships `/usr/share/libalpm/hooks/facelock-pam-remove.hook`, a package
Remove-only `PreTransaction` hook for target `facelock`; `AbortOnFail`
runs `/usr/bin/facelock pam remove --all` before pacman changes the package,
and the package scriptlet retains an idempotent second call. Debian and RPM
propagate cleanup failure so the package, binary and module remain. Source and
Omarchy removal stop before their deletes. The module can be removed only
after the cleanup's final compiled-root scan succeeds.

The all-or-nothing guarantee covers direct PAM edits owned and scanned by this
transaction and retention of the package/module when that cleanup fails.
Debian removal adds a read-only boundary before it: the exact shared-profile
probe runs first, followed by `pam remove --all --dry-run`, then the journaled
real cleanup, and only then the generated ordinary-removal service stop.
Ordinary removal preserves the unit's enabled state for reinstall; the
generated purge path alone retires that state. A selected Facelock
`pam-auth-update` profile blocks removal without changing `common-auth`, its
selection state, direct edits, the service, the binary, or the PAM module. The
diagnostic tells the administrator to run
`sudo pam-auth-update --disable facelock`, verify that a real correct password
succeeds and a wrong password fails, and retry removal. Unsafe or inconsistent
shared-profile state also blocks.

This release deliberately supports no automatic legacy/shared-profile
migration or deselection. Older packages persisted no durable fact that can
distinguish a package-auto-enabled profile from a later administrator choice;
therefore every selected state is administrator-owned and preserved. This is
the intentionally deferred legacy ambiguity: a future automatic transition
requires exact package provenance recorded before the choice, a byte-and-
metadata snapshot of the managed PAM graph, mutation while the old profile and
binary still exist, `pam-auth-update`, reapplication of only provenance-owned
direct edits, and real correct/wrong-password validation, with provable restore
or retained evidence on failure. An unselected profile needs no graph
transition: its `Default: no` metadata leaves with the package payload after
the fixed-root direct cleanup succeeds. Fedora is separate:
#226 owns only RPM payload retirement and the read-only upgrade guard.
Shared-stack migration, regeneration, editing and rollback are explicitly rejected.
`remove --all` scans `/etc/authselect` as a detection-only root and never edits
generated state.

**`--json`** emits exactly one document on stdout and no human text; `--quiet`
suppresses even that, leaving the exit code as the whole answer, as it does for
`is-enrolled`. Diagnostics stay on stderr either way. **`--json` implies
`--no-confirm`**: the per-file question is drawn on stderr while a parser waits
on stdout, so asking it would block the pipeline. It does **not** imply
`--allow-sensitive` — that is an authorization, and a machine caller has not
given one — so `pam add --service system-auth --json` still refuses.

On `add` and `remove`, a validation failure produces **no** JSON document: it
is reported as text on stderr and the process exits non-zero, matching
`is-enrolled`, whose unanswerable case prints a reason and no payload. The
phase that rejects is the phase that would have decided every row, so there is
no partial document to emit.

`pam status` is the other way round and **always** emits a document: it has no
all-or-nothing phase to fail, so a rejected service name — or one whose entry
this refuses to follow — becomes an `unknown` row inside the document (with the
reason in `error`) alongside the rows for every other requested service, and
the refusal is *also* written to stderr for a human. Exit 2 either way.

```json
{
  "command": "add",
  "dry_run": false,
  "services": [
    {
      "service": "sudo",
      "path": "/etc/pam.d/sudo",
      "action": "installed",
      "backup": "/var/lib/facelock/pam-backups/sudo.1770000000-123456789"
    }
  ]
}
```

`pam status --json` carries one extra top-level key, `module_path`: the
candidate `pam_facelock.so` was found at, or `null` when none was. It is a
property of the machine rather than of a service — which is why it is top-level
and not repeated in every service object — and it is what tells an integrator
that the line is present but names a module at a path nothing looks at. `add`
and `remove` refuse before writing when the module is missing, so their
documents do not carry the key.

`pam status --all --json` carries a second additive top-level key,
`directories`: every directory searched, in search order, each an object with
`path` and a `status` of `scanned`, `absent` or `unreadable`, and `error` on an
unreadable one. Only `--all` carries it, because only `--all` claims to have
looked everywhere; a named request resolves through the search path without
enumerating it.

A service object carries `shadows` when the file it names is a local copy
hiding a package's own: the value is the vendor path it hides. The key is
**absent** rather than `null` when nothing is shadowed, which is every row on a
machine with no vendor directory. It is a maintenance fact rather than a state
one, since the service is `present` either way, and it says the copy will not
follow the package's updates.

**`shadows` is a property of the row, not of a flag or a verb.** It appears on
any row whose file hides a vendor one — `pam status` with `--service` as well as
with `--all`, and `pam add` and `pam remove` alike — because one resolver
answers for every verb and a row that knows the fact does not withhold it. On an
`overridden` row it is the vendor file the copy was made from, which is what
that row has just started shadowing. The human line gained the same fact at the
same time: a configured service whose file shadows a vendor one reads
`facelock PAM line present (local override of <path>)` instead of
`facelock PAM line present`, on every form of `pam status`. Exit codes are
unchanged by it.

**This shape is a stability contract.** An object rather than a bare array so a
new top-level field is additive. Field names do not change and are not removed;
`service`, `path`, `action` and `backup` are always present on every service
object; `error` is present when `action` is `failed`, `cleanup-failed` or
`unknown`, and `shadows` when the file the row names hides a vendor one.
**`error` is a
diagnostic, not a contract** — branch on `action`, never on `error`'s text. A
rejected service name reports the fixed C-locale string `invalid service name`,
a symlinked service entry reports `symlinked outside /etc/pam.d` — the retained
fixed name for the class, whichever directory it was in — and a
hard-linked one `hard-linked service file`, but the OS-level failures
(`failed` on a write, `cleanup-failed` after the requested PAM state was
reached, or `unknown` on an unreadable file) interpolate a `strerror` string,
which follows the operator's
`LC_MESSAGES` like any other C library message. Nothing else in a `--json`
document is locale-dependent. `backup` is the newest committed backup path the
calling process can validate for the service, falling back only to the exact
legacy adjacent `<service>.facelock-backup` path; it is `null` otherwise. The
root write verbs can inspect the `0700` state directory; an unprivileged
`pam status` normally cannot and may therefore report `null` even while such a
versioned backup exists. This is the documented location change from the pre-0.2
adjacent path, so consumers treat the value as an opaque absolute rollback
path. It is always `null` under `--dry-run`, which writes none, and normally
`null` after a default `remove`, which cleans owned state; `--keep-backup`
retains it. It is always `null` for an `overridden` service: the copy preserved
nothing, so legacy state at the override path is not this run's rollback and
is not reported as one. Deleting the override is its undo, and the vendor
original is untouched.
`path` on an `overridden` row is the **override that was created**, not the
vendor file it was read from; on a `vendor-only` row it is the vendor file,
which is the one that exists. `path`
is itself `null` when `action` is `unknown` because the *name* was rejected: no
path was ever resolved, and reporting `/etc/pam.d/../escape` named a path
nothing went near, which reads as one that was acted on. A service rejected for
being a symlink does carry a `path` — the link, which is a real entry this did
`lstat` — and its `backup` field probes only the exact legacy adjacent name,
since a pre-0.2 version may have written through the link. It does not use an
untrusted link to select versioned state.

The `action` vocabulary — **new words may be added, so a consumer must tolerate
one it does not know rather than treat it as an error**:

| `action` | Verb | Meaning |
|----------|------|---------|
| `installed` | `add` | the line was written (under `--dry-run`, would be) |
| `overridden` | `add` | the service resolved only in a vendor directory, so an `/etc/pam.d` copy carrying the line was created from it (under `--dry-run`, would be) |
| `vendor-only` | `remove`, `status` | the service resolves only in a vendor directory: nothing was written, and there is no local override to carry a line |
| `removed` | `remove` | the line was deleted (under `--dry-run`, would be) |
| `unchanged` | `add`, `remove` | already in the requested state |
| `absent` | all three | the service file does not exist |
| `declined` | `add` | the operator answered no at the per-file confirmation |
| `failed` | `add`, `remove` | the write failed; see `error` |
| `present` | `status` | the file exists and carries a facelock line |
| `missing` | `status` | the file exists and carries none |
| `unknown` | `status` | the file could not be read, the name was rejected, or the entry is a symlink or a hard link; see `error` |

`pam status --json` is what replaces `grep -q pam_facelock.so
/etc/pam.d/<service>` in an integration script: it answers from the same file,
without root, and reports "absent" and "unreadable" as themselves rather than
as "not configured".

**Repeatable `--service`.** `--service a --service b` acts on both in one
process, one root check and one closing hint. Duplicates collapse. No
`--service` means `sudo`, which is what bare `setup --pam` has always meant.

**PAM line placement.** The direct CLI writer emits exactly this 36-byte
literal; the literal itself has no trailing newline:

```pam
auth      sufficient pam_facelock.so
```

The control is frozen to `sufficient`: a successful face can satisfy the
stack, while a non-match or unavailable face path continues under the
service-owned rules that follow. Ordinary login and privilege stacks commonly
reach their password modules there. Omarchy's face-only context instead
continues to `pam_deny.so`; its password attempt uses a separate PAM context.
There is intentionally no `--control` option. A caller cannot silently
substitute `required`, an extended control, or another stack policy for the
line whose behavior downstream consumers and Facelock cleanup rely on.

The line is inserted immediately before the first *logical* rule whose first
ASCII-whitespace-delimited type token is `auth`, matched
ASCII-case-insensitively and with Linux-PAM's optional leading `-`;
`authtok_type=` is not an auth type. If no auth rule exists and the first
physical line is exactly `#%PAM-1.0`, the line follows that header. Without
that exact leading header it starts at byte 0. This header-aware fallback is
the post-#192 contract; it supersedes #166's original top-of-file wording.

Omarchy owns this exact backend-neutral `omarchy-lock-face` skeleton:

```pam
#%PAM-1.0
auth       required                    pam_deny.so
account    include                     system-local-login
```

The direct writer produces this exact stack, leaving Omarchy's face-only lane
to reach its plain denial when Facelock does not succeed:

```pam
#%PAM-1.0
auth      sufficient pam_facelock.so
auth       required                    pam_deny.so
account    include                     system-local-login
```

Add idempotency and named `pam status` recognition are deliberately broader
than the emitted bytes. Any uncommented logical rule whose semantic bytes
contain the exact, case-sensitive byte sequence `pam_facelock.so` is an active
reference: `add` does not emit a duplicate, and `status` reports it present.
This is substring recognition, not a PAM module-token-boundary promise.
`pam remove --all` is stricter: a broad active reference is not automatically
Facelock-owned. Without validated provenance or the exact vendor-copy shape,
only the exact canonical physical line in a conventional service is eligible
for cleanup; custom control, spacing, or options block the whole-machine run
for administrator review rather than being rewritten.

This emitted-byte contract applies only to direct CLI service-file writes.
The packaged Debian `pam-auth-update` profile is opt-in (`Default: no`) and
intentionally emits `[success=end default=ignore] pam_facelock.so`. Legacy or
administrator-managed Fedora authselect profiles use authselect's generated
layout; Facelock RPMs no longer ship or select one. Both shapes remain visible
to the same broad `add`/`status` active-reference recognition, but neither is
required to match the canonical direct-writer bytes.

**Service-file edits are byte-preserving.** A backslash followed only by spaces
or tabs before LF or CRLF continues the same logical PAM rule, so insertion
never splits that rule. A `#` ends the semantic rule even after a continuation;
comment and blank physical lines remain untouched.

Removal drops the whole genuine logical Facelock rule. For recovery from older
facelock output that inserted the canonical physical line between an
administrator's continuation backslash and its following physical line,
removal deletes only that injected line. This reconnects the administrator's
logical rule instead of deleting it.

The editor never decodes the service file as UTF-8 and never reconstructs
unmodified lines. Existing LF or CRLF endings, invalid bytes, and the presence
or absence of the final newline survive unchanged; the one inserted line uses
the target rule's line ending, falling back to the document's first ending when
that target is unterminated. It uses the PAM header's ending when it follows
that header, or the document's first ending when it goes at the top. A header
with no final newline gains the separator before the inserted rule while the
rule itself remains unterminated. A byte-identical no-op writes no file and
takes no backup; an in-place add backs up before its real edit. Removal takes
no new backup and cleans validated rollback state by default; `--keep-backup`
preserves it. Vendor-override creation has no original at the override path to
preserve, as documented above. Golden fixtures pin insertion, removal,
invalid-byte, CRLF and no-final-newline behavior.

### facelock status Semantics

`facelock status` renders one `Health` value twice: as the report a
person reads, and — under `--json` — as one document a script parses. Both
renderers are pure functions of that value, and a unit test walks the two
outputs of one fixture and fails the build when they disagree about any
section's verdict or when a section appears in one and not the other.

It stays root-only (see "CLI Privilege Model" below): every fact in the report
comes from root's view — the 0600 database, other users' markers, the daemon's
root-only methods — so there is no unprivileged half to split out. The
consumers are root-run scripts: `test/run-integration-tests.sh` waits on the
daemon by parsing this document, and a setup script verifies the enumerated PAM
state without reading prose.

**Exit codes are unchanged and carry no verdict.** `status` exits 0 whenever it
produced a report, whatever the report says, exactly as it did before `--json`
existed; a failure to *reach* the report (not root) is the only non-zero exit.
The verdicts are in the document. So `--quiet --json` prints nothing and exits
0 — `--quiet` suppresses the payload as it does everywhere, but here the exit
code is not an answer, which makes the combination a no-op rather than a
terser query.

**Every fact is a tri-state.** Each section object carries a `state` of `ok`,
`problem` or `unknown`, and a `reason` string **on `problem` and `unknown`
only**. `unknown` is the report's whole reason for existing: "the database
could not be read" is a different answer from "this user has no models", and
JSON makes that distinction easier to lose than prose does, because `null` and
`false` read as answers. So a fact nobody established is never a `null` and
never a `false` — it is `"state": "unknown"` with a reason. The nested facts
that can be undetermined carry the same three words: `camera.device`,
`encryption.embeddings`, `enrollment.marker` and `pam.services`.

**Read `state` before anything beside it.** A section whose `state` is not `ok`
may omit any of its detail keys, because a probe that did not finish has nothing
to report there: on an unreadable database `enrollment` carries no `models` key
at all. That is deliberate — an empty array would be a *known* "this user has no
faces", which is the collapse this whole document is shaped to prevent — but it
means a defaulting read is a bug. `jq '(.enrollment.models // []) | length > 0'`
answers `false` for a machine whose database could not be opened. Branch on
`.enrollment.state == "ok"` first, then read `models`.

`reason` is never catalog output. The document is machine output and does not
enter the message seam (`message/mod.rs`, "What must NOT come through here"), so
no `reason` is ever a translated string: the ones facelock authors are C-locale
literals, and the one localized `why` the health probe produces — the reason
every config-dependent fact carries when the file did not parse — is replaced by
the literal `config not available` on the way out.

**`reason` is still not a fixed vocabulary.** Some of them embed the diagnostic
the probe captured, because that text is the part worth having:
`enrollment.reason` can read `database not accessible: <store error>`, and
`enrollment.marker.reason` carries the marker file's own read or parse failure.
The `error` fields are the same kind of thing — `config.error` is the `toml`
parser's message, `execution_provider.error` the ONNX Runtime's own load
failure, and each `pam.services.not_checked[].error` a listing or read failure.
An OS error rendered by the C library follows the operator's `LC_MESSAGES` like
any other `strerror` string, exactly as `pam`'s `error` field does. So `reason`
and `error` are both **diagnostics, not contracts**: branch on `state` and on
the section's typed words, print `reason` and `error`, and match on neither.

**`daemon` carries no diagnostic, on purpose.** It is the one probe whose error
string is not the transport's own words: the client attaches a *localized* hint
to a D-Bus `AccessDenied` — advice for a human reading stderr — and rendering
that chain yields the hint alone. Forwarding it would put translated text in a
payload on exactly the machine this section exists to diagnose, so the field is
not emitted. `reachability` and `reason` carry the fact; the hint still reaches
the person, on the report and on stderr.

**The typed words are what a consumer branches on.** `state` says how bad it
is; the section's own word says what it is: `config.outcome`
(`valid`/`not_found`/`invalid`), `daemon.reachability`
(`responding`/`not_responding`/`not_configured`), `camera.selection`
(`configured`/`auto_detect`), `execution_provider.availability`
(`available`/`not_built_in`/`unrecognized`/`unqueryable`), `encryption.key.method`
(`tpm`/`keyfile`/`none`), `notifications.mode`
(`off`/`terminal`/`desktop`/`both`), `config.device.selection`
(`configured`/`auto_detect`) and `models.files[].purpose`
(`detector`/`embedder`).

**Each section's `state` answers its own question, and some are narrower than
the section name suggests.** A section that owns a nested fact keeps the broad
question for itself and puts the specific one underneath, so reading only the
outer `state` can pass a machine the nested fact condemns:

| Section | Its `state` answers | And beside it |
|---------|---------------------|---------------|
| `config` | did the file parse | — |
| `daemon` | did the bus round trip complete, **or was the bus deliberately never asked** (`reachability: not_configured`, which `daemon.mode = "oneshot"` produces) | — |
| `oneshot_fallback` | are the three files daemon-less auth needs on disk | — |
| `camera` | under `configured`, does the node exist; under `auto_detect`, **only that auto-detection is enabled** | `camera.device` — whether a device was actually found and interrogated |
| `models` | is the model directory there, with both configured files | — |
| `execution_provider` | can the installed ONNX Runtime use the configured provider | — |
| `encryption` | is usable key material in place — `method: none` is a `problem` even though the config asked for nothing, because plaintext storage is a finding rather than a preference | `encryption.embeddings` — whether the stored embeddings could be counted |
| `enrollment` | does this user have at least one model | `enrollment.marker` — whether the `is-enrolled` marker agrees with the database |
| `security` | are the checks enabled at all | — |
| `notifications` | never a finding: `ok` whenever the config was read | — |
| `pam` | is `pam_facelock.so` installed | `pam.services` — what the `/etc/pam.d` scan found, and whether it could see everywhere |

Two of those are worth stating outright, because the obvious read is wrong.
Under auto-detection `"camera": {"state": "ok"}` says detection is on, **not
that a camera exists** — a machine with no camera at all renders exactly that,
with `camera.device.state` reporting `problem`. And `"pam": {"state": "ok"}`
says the module is installed, **not that anything uses it** — a machine with the
module in place and nothing wired up renders that, with
`pam.services.configured` empty. The human report is equally generous in both
cases (`[ok] auto-detect enabled`, `[ok] installed`); the nested fact is where
the answer is.

**The PAM section speaks `pam status --json`'s vocabulary.** Each row of
`pam.services.configured` is a `{"service", "path", "action"}` object with the
same `action` words, plus `shadows` when the file it names hides a vendor one
— the same key, absent rather than `null` when nothing is shadowed. Only
`present` rows appear, because a service that does not carry the line is not a
configured service; `backup` is not on these rows, because `status` does not
probe for backup files. `pam.services.state` is `ok` only when every directory
and every service file was read; a single unread place makes it `unknown` and
names each one in `not_checked`, so an incomplete list is never mistaken for a
complete one. What was found is still listed and still true.

**Stability tier.** Field names do not change and are not removed; new fields
and new sections are additive; a removal is breaking. New words may be added to
any of the typed vocabularies above, including `state`, so a consumer tolerates
a word it does not know rather than treating it as an error. **Key order is not
part of the contract** — parse the document, do not string-match it.

A conditional field is **absent**, not `null`, when it does not apply. The whole
list: `reason` (present unless `state` is `ok`), `error`, `shadows`,
`installed_at`, `device`, `files`, `checks`, `delivery`,
`daemon.reachability`, `camera.configured_path`, `camera.present`,
`enrollment.models` and `enrollment.marker`. A consumer must tolerate a section
carrying nothing but `state` and `reason`, which is what every
config-dependent section renders when the config did not parse. There is no roll-up verdict for the machine as a whole: what
counts as healthy is the consumer's policy, and the human report does not make
that judgment either.

The document for a fully healthy machine, which is the fixture both renderers
are tested against:

```json
{
  "config": {
    "state": "ok",
    "path": "/etc/facelock/config.toml",
    "outcome": "valid",
    "device": { "selection": "configured", "path": "/dev/video2" }
  },
  "daemon": {
    "state": "ok",
    "bus_name": "org.facelock.Daemon",
    "reachability": "responding"
  },
  "oneshot_fallback": {
    "state": "ok",
    "auth_bin": "/usr/bin/facelock",
    "binary_present": true,
    "models_present": true,
    "database_present": true
  },
  "camera": {
    "state": "ok",
    "selection": "configured",
    "configured_path": "/dev/video2",
    "present": true,
    "device": {
      "state": "ok",
      "path": "/dev/video2",
      "name": "Integrated IR Camera",
      "ir": true,
      "quirks": []
    }
  },
  "models": {
    "state": "ok",
    "dir": "/usr/share/facelock/models",
    "dir_present": true,
    "files": [
      { "purpose": "detector", "path": "/usr/share/facelock/models/det.onnx", "present": true },
      { "purpose": "embedder", "path": "/usr/share/facelock/models/emb.onnx", "present": true }
    ]
  },
  "execution_provider": {
    "state": "ok",
    "configured": "cpu",
    "availability": "available"
  },
  "encryption": {
    "state": "ok",
    "key": {
      "method": "tpm",
      "sealed_key_path": "/etc/facelock/sealed.key",
      "sealed_key_present": true,
      "tpm_device_path": "/dev/tpmrm0",
      "tpm_device_present": true
    },
    "embeddings": { "state": "ok", "encrypted": 2, "plaintext": 0 }
  },
  "enrollment": {
    "state": "ok",
    "user": "alice",
    "models": [
      { "id": 1, "label": "front" },
      { "id": 2, "label": "side" }
    ],
    "marker": { "state": "ok" }
  },
  "security": {
    "state": "ok",
    "disabled": false,
    "checks": {
      "require_ir": true,
      "require_frame_variance": true,
      "require_landmark_liveness": false,
      "min_auth_frames": 3
    }
  },
  "notifications": {
    "state": "ok",
    "mode": "both",
    "delivery": { "prompt": true, "on_success": true, "on_failure": false }
  },
  "pam": {
    "state": "ok",
    "module_path": "/lib/security/pam_facelock.so",
    "installed_at": "/lib/security/pam_facelock.so",
    "services": {
      "state": "ok",
      "configured": [
        { "service": "sudo", "path": "/etc/pam.d/sudo", "action": "present" },
        {
          "service": "polkit-1",
          "path": "/etc/pam.d/polkit-1",
          "action": "present",
          "shadows": "/usr/lib/pam.d/polkit-1"
        }
      ],
      "not_checked": []
    }
  }
}
```

A machine whose config did not parse renders all nine config-dependent sections
as `{"state": "unknown", "reason": "config not available"}`, **plus whatever
that section knows without the config**: `daemon` keeps `bus_name` (a property
of the design, not of the machine) and `enrollment` keeps `user` (resolved from
argv and the environment, never from the file). The other seven carry `state`
and `reason` alone. Meanwhile `config` reports the parse failure itself, and
`pam`, which is config-independent, still answers in full.

### facelock capabilities

`facelock capabilities` answers "what can *this* build do?" — from the binary's
own clap tree and compiled-in constants, without reading a config file,
activating the daemon, or opening a camera. It is what replaces
`facelock setup --help 2>/dev/null | grep -q -- "--no-pam"` in a wrapper
script: **help text is not an API**, and a grep against it breaks on a reworded
flag description, a line wrap, or a translated help template.

Bare `capabilities` prints one name per line on stdout. `--json` prints one
document on stdout. Both exit 0 — the command has no failure mode — and
`--quiet` suppresses stdout entirely, leaving the exit code as the whole
answer, as it does for `is-enrolled`. Neither form localizes: a capability name
is an identifier, not prose.

The `--json` document, with the array elided — the names this build emits are
the table at the end of this section:

```json
{"version": "0.1.4", "capabilities": ["capabilities", "devices-json", "is-enrolled"]}
```

`version` is this binary's own version — byte for byte the one `facelock
--version` prints. `capabilities` is a **sorted, deduplicated** array of
strings.

**Probe by name, not by version.** A version comparison is the wrong test
twice over: a git or distro build can carry a version that says nothing about
what is in it (`facelock-git` is exactly that case, and is why a downstream
package pin cannot express "needs the `pam` verb"), and a backport can add a
feature without moving the number. The name list cannot drift from the binary
it came out of: `capability_names_are_all_implemented` maps every name to the
clap argument, subcommand or constant that declares it, and what each surface
*means* is pinned by the section of this document that owns it. `version` is
for humans and bug reports.

**A build that predates the command** answers by failing: clap's
"unrecognized subcommand" error, usage text on stderr, exit 2, nothing on
stdout. A caller reads any non-zero exit as "no capabilities at all", which is
the true answer for that build.

**Stability.** The names are a contract of the same kind as the `pam --json`
`action` vocabulary, one degree stronger:

- a name, once emitted, never changes meaning
- names are **added**; none is ever removed or repurposed
- `version` and `capabilities` are always present, and a new top-level field is
  additive — a consumer ignores fields it does not know
- a consumer tolerates a **name** it does not know rather than treating it as
  an error
- key order within the JSON document is **not** part of the contract — parse
  the document, do not string-match it

**Naming.** Lowercase, hyphenated. A bare name (`quiet`, `is-enrolled`) means
the command or global flag itself exists; `<command>-<feature>` names one
feature of one command, and where the command's own name is hyphenated the
suffix simply appends (`is-enrolled-json`). One name promises one thing: a flag
that is not on this list is not being denied, only not yet promised.

| Name | Meaning |
|------|---------|
| `capabilities` | this command exists, so a consumer's membership test is uniform across every name |
| `config-edit` | `config edit` exists — the verb ADR 009 split out of the old `--edit` flag |
| `daemon-restart` | `daemon restart` exists — the verb ADR 009 moved under `daemon` from the top-level `restart` |
| `devices-json` | `devices --json` |
| `is-enrolled` | `is-enrolled` exists — the unprivileged enrollment probe whose exit code is the contract |
| `is-enrolled-json` | `is-enrolled --json` |
| `pam-allow-sensitive` | `pam add` accepts `--allow-sensitive`, the gate on the sensitive services; `pam remove` does not offer it, because removal is never gated |
| `pam-dry-run` | `pam add`/`pam remove` accept `--dry-run` |
| `pam-if-present` | `pam add`/`pam remove`/`pam status` accept `--if-present` |
| `pam-json` | `pam add`/`pam remove`/`pam status` accept `--json` |
| `pam-multi-service` | `pam add`/`pam remove`/`pam status` take a repeatable `--service` — several services in one process, one root check |
| `pam-remove-all` | `pam remove --all` exists, conflicts with `--service`, and uses compiled-root whole-set cleanup |
| `pam-status` | `pam status` exists — the unprivileged `/etc/pam.d` read (DEC-6 below) |
| `pam-status-all` | `pam status --all` exists, and conflicts with `--service` — the enumerating form, which answers "what is configured on this machine?" rather than "is this name configured?" |
| `quiet` | the global `--quiet` |
| `setup-allow-sensitive` | `setup --pam` accepts `--allow-sensitive` as the explicit sensitive-service authorization; `--yes` remains prompt suppression only |
| `setup-if-present` | `setup --pam --if-present`, on add and on `--remove` alike |
| `setup-no-pam` | `setup --no-pam` |
| `setup-systemd` | `setup --systemd` |
| `status-json` | `status --json` — the machine-readable system report, one key per section. See "facelock status Semantics" |
| `tpm-decrypt` | `tpm decrypt` exists — the verb ADR 009 moved under `tpm` from the top-level `decrypt` |
| `tpm-encrypt` | `tpm encrypt` exists — the verb ADR 009 moved under `tpm` from the top-level `encrypt` |
| `tpm-reseal` | `tpm reseal` exists — the verb ADR 009 moved under `tpm` from the top-level `reseal` |

The five names ADR 009 added are the only way a wrapper can tell a build that
takes `daemon restart` from one that still wants `restart`: the old spellings
were deleted rather than aliased, so probing by invocation costs a failed
command. Each promises only that the subcommand at that path parses.

### CLI Privilege Model (DEC-6)

The CLI is root by default: every subcommand requires root except the six
listed below, which are unprivileged by design, not by omission.

| Command | Why unprivileged |
|---------|-------------------|
| `facelock is-enrolled` | Answers from the caller's own `0600` marker file; the unprivileged integration point (see Exit Codes above). Never probes D-Bus |
| `facelock hyprlock …` | Edits the user's own dotfile — root would write root-owned files into `$HOME`, which is wrong, not just unnecessary |
| `facelock pam status` | Reads `0644` files under `/etc/pam.d` and writes nothing. Same role as `is-enrolled`: the probe an integration runs without `sudo`, replacing a hand-rolled `grep -q pam_facelock.so /etc/pam.d/<service>`. A file it cannot read reports `unknown` and exits 2 rather than reporting it as missing |
| `facelock config [show]` | Reads a `0644` file. The rename split the flag into a verb (ADR 009) and the privilege split survives it exactly: `config show`, and the bare `config` that means it, stay unprivileged; `config edit` is root |
| `facelock capabilities` | Reports what the *binary* can do, derived from its own clap tree and compiled-in constants — no file, no D-Bus, no camera, no per-user state, so there is nothing to protect. Unprivileged because the consumer is a user-level setup script deciding whether to invoke `sudo facelock …` at all: a probe that needed root to answer "do I need root?" would be useless |
| `--help`, `--version` | — |

Every other command requires root. Two escalation behaviors apply, and each
command uses exactly one:

- **Interactive prompt.** `setup`, `enroll`, `test`, `preview`, `bench`,
  `tpm` (including `tpm encrypt`, `tpm decrypt` and `tpm reseal`),
  `daemon run`, `daemon restart`, `config edit`, `remove`, `clear`, `list`,
  `status`, `devices`. Run as non-root with a TTY attached,
  these ask `Root required. Re-run with sudo? [Y/n]` and re-exec via `sudo`
  on yes. Run as non-root with no TTY (scripted, piped, or closed stdin),
  they hard-error instead — `Root required.\n  Run: sudo facelock <cmd>` —
  rather than hang waiting for input that will never arrive
  (`ipc_client::require_root`).
- **Hard error only.** `facelock pam add`, `facelock pam remove` and
  `facelock audit` never offer the interactive prompt at all, even with a TTY
  attached — each is typically invoked non-interactively or by a wrapper,
  where a stray confirmation prompt is a hang, not a convenience
  (`ipc_client::require_root_scripted`).

`facelock daemon run` is listed above under **interactive prompt** because
that is what `commands::daemon::run` calls (`ipc_client::require_root`), not
because a service manager should ever meet a prompt. Earlier revisions of this
table claimed it was hard-error-only; the code has always said otherwise, and
this row now matches the code. Under systemd there is no TTY, so the branch
taken is the hard error either way.
[#188](https://github.com/tyvsmith/facelock/issues/188) tracks whether it
should be scripted.

`facelock pam add|remove` are in the hard-error class because the surface they
replace was: standalone `setup --pam` bailed from its own root check rather
than prompting. The check runs **before `--dry-run` is honoured**, so a dry run
still needs root, and `pam status` is the unprivileged read to reach for
instead.

`facelock auth` is not user-facing — PAM spawns it directly, and it is not
part of this table.

**Ordering guarantee (C6).** Every command that prompts for confirmation or
runs an interactive question runs its root check **first**, before that
prompt or any other output or side effect. `remove` and `clear` both ask a
Y/N confirmation before deleting a face model; historically `remove`'s root
check ran *after* that confirmation, so a non-root user would confirm a
destructive action and only then discover it was refused — this is fixed. The same ordering applies to `status`, `devices`,
`preview`, `test`, `audit`, `bench`, and `config edit`: the root check is
the first statement in each command's entry point, before `Config::load()`
or any `println!`.

**AccessDenied hint.** A D-Bus `AccessDenied` reply carries one actionable
hint (`ipc_client::add_access_denied_hint`): root is required. Since almost
every D-Bus method is root-only (see IPC Protocol below) and, under ADR 010,
the bus admits a non-root caller to `Authenticate` alone, a denial from the
daemon's `require_root` and a denial from the bus policy have the same fix.
There is no group to join (ADR 010).

### facelock test Semantics (N11)

`facelock test` is root-only (issue #96) and, being root, keeps full detail
on both transports: on the daemon transport, `AuthResult.similarity` is
redacted to non-root D-Bus callers only (`redact_similarity_unless_root`) —
since `test` requires root, it always gets the real score. The direct
transport never redacts.

**`test` is a separate D-Bus method, not a privileged flavor of
`Authenticate`.** On the daemon transport `facelock test` calls the
root-only **`TestAuthenticate`** method; `Authenticate` is real
authentication only. The daemon does not infer which it is serving from the
caller's UID, and must not: `pam_facelock` runs inside the PAM stack of the
authenticating program, and `sudo` is setuid-root — as are `login`, `su`,
and root-run display-manager greeters — so a real failed face
authentication at a `sudo` prompt reaches the daemon as UID 0. A design that
exempted root callers from rate-limit consumption therefore left the limit
inert on the primary documented PAM target. Intent travels with the method
call instead (`AuthIntent` in `facelock_daemon::handler`).

Both entry points run the same pre-flight gates — `security.disabled`,
enrollment / `suppress_unknown`, the rate-limit check, and `require_ir` —
via `facelock_daemon::auth::pre_check_audited*`. `TestAuthenticate` differs
in exactly two documented ways:

1. **The `abort_if_ssh` / `abort_if_lid_closed` gates are skipped**
   (`PreCheckContext::test()`). Those two exist to stop an *attacker*'s
   physical-access shortcuts, not to block an admin who is already root (by
   construction, since `test` requires root) and is deliberately diagnosing
   recognition over SSH or with the lid closed on a docked laptop. This is a
   context flag threaded through `pre_check`, not a parallel copy of the gate
   logic (issue #95 was exactly that kind of drift). It applies identically
   on the direct transport, which calls `pre_check_audited_with_context`
   directly — the two transports no longer diverge here, as they did while
   `test` had no daemon-side method of its own to carry the context.
2. **A failed attempt consumes no rate-limit budget.** The direct transport
   gets this structurally (`direct::authenticate` never calls
   `RateLimiter::record_failure`); the daemon transport gets it because
   `TestAuthenticate` is the entry point that does not charge. Root-only is
   what makes a budget-free authentication endpoint safe to offer at all —
   root already owns the database and can clear the limiter directly, so
   exempting *consumption* for it costs nothing.

`Authenticate` charges a failed attempt on every transport and for every
caller including root — with one exception, added by ADR 008 §4: **an attempt
where the camera never saw a face charges nothing** (`face_detected == false`,
the `-1` wire sentinel). Nobody was there, so no guess was made; a screen
locker that starts face auth on every wake, or a laptop opened in front of an
empty desk, would otherwise spend the user's whole budget before they sit
down. A face that *was* seen and did not match (`-4`) still charges. The rule
is identical on the daemon and one-shot paths, which share the `rate_limit`
table.

Such an attempt also ends early, at `recognition.no_face_timeout_secs`
(default 2, clamped to `timeout_secs`, `0` disables) rather than at
`timeout_secs`; the outcome it reports is exactly the one the full timeout
reports, so no client gains a case.

The rate-limit *check* (whether `user` is already over budget) is unaffected
by any of the above and still runs on both methods and both transports: an
already-limited user's `test` run reports "rate limited", exactly like real
auth would — surfacing an existing lockout instead of masking it.

## Operating Modes

| Mode | Config | PAM Behavior | CLI Behavior |
|------|--------|-------------|-------------|
| Daemon | `daemon.mode = "daemon"` (default) | D-Bus IPC to daemon | Uses daemon if available, falls back to direct |
| Oneshot | `daemon.mode = "oneshot"` | Spawns `facelock auth` | Operates directly (no daemon) |

The CLI silently falls back to direct mode when the daemon is not available on D-Bus, regardless of config mode.

### facelock is-enrolled Exit Codes

The exit code **is** the contract — `is-enrolled` is designed to drop into a
shell one-liner, so integrations should branch on the status, not parse stdout.
The name follows systemd's `is-*` family (`systemctl is-active`, `is-enabled`),
which is the established idiom for a boolean query whose exit code is the answer;
the codes themselves match `grep`'s 0 = match / 1 = no match / 2 = error.

| Code | Meaning |
|------|---------|
| 0 | User has a usable enrollment |
| 1 | Not enrolled / not usable (includes an unreadable or absent marker) |
| 2 | Error — bad arguments, or a marker that exists but cannot be parsed |

`facelock pam status` uses the same 0/1/2 scale for the same reason; see
"facelock pam Semantics" above.

Default stdout is `enrolled` / `not-enrolled` — the state word, as `systemctl
is-active` prints `active`. `--quiet` suppresses stdout and leaves only the exit
code; it is the global `-q` flag, so `facelock --quiet is-enrolled` is the same
invocation. `--json` emits `{"enrolled": bool, "models": N, "updated": "<ISO8601>"}`;
when the user is not enrolled there is no marker to read a timestamp from, so
`models` is `0` and `updated` is `null`.

`is-enrolled` answers from `/var/lib/facelock/enrolled/<user>` alone. It never
activates the daemon over D-Bus, never opens a camera, and never reads the
database — so it is safe to call repeatedly from a lock screen as an
unprivileged user. The marker is a hint that can drift from the database; **PAM
at auth time remains authoritative** and nothing in the auth path consults it.

Markers are written by `enroll`, `remove` and `clear`, and converged from the
database by `setup`, by daemon startup, and by the one-shot `facelock auth`
path. Convergence re-derives markers from the database rather than replaying
recorded steps, so it is idempotent and there is no migration state to keep.
The scope differs by caller and that difference is contract:

| Caller | Scope |
|--------|-------|
| `setup`, daemon startup (`reconcile_all`) | **Every** marker: backfills each enrolled user and prunes every marker the database does not account for |
| one-shot `facelock auth` | **One** marker — the user being authenticated. It has no reason to read other users' rows and no privileged directory listing to prune with |

An install upgraded from a release that predates markers backfills itself on the
first daemon start or the first authentication; until one of those happens,
`is-enrolled` reports `not-enrolled` for a user who is in fact enrolled.

On the one-shot path the convergence point is bounded on both sides, and both
bounds are contract rather than convenience. It runs **after** the pre-flight
gates, so an attempt rejected as disabled / SSH / lid / rate-limited /
non-IR performs no marker write at all — no attacker-drivable filesystem work
from the wrong side of the rate limiter. It runs **before** the camera is
opened, so every later way an attempt can end — a signal, a failed model load,
a camera another process is holding, an undecryptable template, the no-face
timeout, a plain non-match — leaves the marker already converged. In short: an
attempt that reaches the camera has converged the marker, whatever it goes on
to decide.

That placement means the one-shot's *write* only ever converges a marker
upward: reaching it requires the enrollment gate to have passed. The downward
direction — a marker whose database rows are gone, which a daemonless install
has no `reconcile_all` to prune — is handled at the gate that has the evidence:
when the database authoritatively reports **zero** models for the user, the
one-shot deletes any marker claiming otherwise before returning the rejection.
That is a removal and nothing else: one `unlink(2)` on a single validated path
component, no temp file, no `chown`, no `rename`, and no marker directory
created. It is idempotent (a repeat attempt finds nothing to unlink) and it is
reachable only when the marker is already false, so it can delete a stale marker
and never a correct one.

### facelock auth Exit Codes

| Code | Meaning | PAM Code |
|------|---------|----------|
| 0 | Face matched | PAM_SUCCESS |
| 1 | No match / timeout / dark | PAM_AUTH_ERR |
| 2 | Error / no enrolled faces | PAM_IGNORE |

## Release Channels and APT Paths

`dist/release-matrix.json` is the checked-in release-target authority. A strict
prerelease tag has the form `vX.Y.Z-{alpha,beta,rc}.N`; it creates a GitHub
prerelease and direct artifacts, but it must not publish to stable APT, stable AUR, or production COPR. Staging COPR infrastructure is owned by issue #236
and is not provisioned or modified by the prerelease identity workflow.

A stable `vX.Y.Z` tag may publish to stable APT and AUR only after validated
release metadata classifies it as stable. Production COPR additionally
requires a deliberately restored `trigger: release` job in the stable-tagged
Packit configuration. Prerelease-capable configurations keep that job inert.
Preflight and CI compare the public `tyvsmith/facelock` COPR API read-only with
the production chroot authority; they never change the project. The required
supported production COPR chroots are exactly Fedora 43, Fedora 44, and Fedora
45. Rawhide is the only optional allowed experimental production chroot: its
presence or absence is accepted, while any missing supported chroot or any
other extra chroot fails closed.

Every Packit `copr_build` target must be an explicit member of the checked-in
allowlist: `fedora-43-x86_64`, `fedora-44-x86_64`, or
`fedora-45-x86_64`. Mutable aliases such as `fedora-all`,
`fedora-development`, and their architecture-suffixed forms are rejected, as
is any other undeclared target. Rawhide is not a Packit staging or production
release target; both `fedora-rawhide` and `fedora-rawhide-x86_64` fail
validation. The prerelease rule is that no alpha may publish to Rawhide.
Fedora 43 and Fedora 44 are the required full-lifecycle targets; Fedora 45 is a
required build/runtime-smoke target. Issue #230 owns the actual lifecycle
evidence. Rawhide cannot supply lifecycle, artifact, upgrade, rollback,
served-version, or availability evidence; it is limited to best-effort pinned
Track D smoke only. It is non-release and non-gating: its absence or a
Rawhide-only failure is not alpha-blocking, and its smoke result is not alpha
acceptance or release evidence. Promotion requires a separately reviewed
amendment and full Fedora gates.

Issue #236 owns pre-tag and post-publication proof that optional Rawhide serves
no alpha or candidate build. This contract does not provision, publish to, or
otherwise mutate COPR or Packit infrastructure.

The public APT base is `https://tysmith.me/facelock/apt/`. Its stable suite
paths and payload identities are:

| Suite | Public Release path | Architecture | Package |
|-------|---------------------|--------------|---------|
| `trixie` | `https://tysmith.me/facelock/apt/dists/trixie/Release` | amd64 | `facelock`, TPM enabled |
| `resolute` | `https://tysmith.me/facelock/apt/dists/resolute/Release` | amd64 | `facelock`, TPM enabled |

The former `main` and `legacy` suite names are retired; they are not aliases or
redirects. Existing source entries must replace that suite component with the
host operating-system codename while keeping the `facelock` component.
Debian-family release support is exactly Debian 13 (Trixie) and Ubuntu 26.04
LTS (Resolute). Bookworm and Noble artifacts may remain in historical releases,
but those suites are unsupported and receive no new packages.

Both suites ship one binary package named `facelock` with TPM support enabled.
There are no legacy/TPM package-name alternatives and the package declares no
`Provides`, `Conflicts`, or `Replaces` transition identity. Stable publication
consumes exactly two suite manifests, one matching package per suite, and a
prerelease or cross-suite version is rejected before signing or repository
writes.

### Debian source and binary package contract

Trixie package builds use the official Trixie Backports `cargo` and `rustc`;
Resolute uses its native distro packages. Both must satisfy the workspace and
`debian/control` minimum of Rust 1.88. No `rustup` toolchain participates in
Debian source builds.

The Debian source package contains the exact tagged main upstream tarball, the
reviewed ORT component, the deterministic Cargo-vendor component, and the
Debian quilt delta. For upstream `U`, Debian version `V`, and architecture `A`,
the release manifest lists exactly these eight files in canonical order:

```text
facelock_U.orig.tar.gz
facelock_U.orig-onnxruntime.tar.gz
facelock_U.orig-cargo-vendor.tar.xz
facelock_V.debian.tar.xz
facelock_V.dsc
facelock_V_A.buildinfo
facelock_V_A.deb
facelock_V_A.changes
```

The Cargo component is bound to the exact `Cargo.lock`, contains only regular
normalized files plus its lock hash, bytewise manifest, and generated legal
inventory, and is used through the package-only Cargo source replacement. The
inventory covers every exact vendored crate and records its path, name, version,
declared license or license-file, available authors/upstream metadata, and every
referenced license material that exists in the component. The ORT component contains the
reviewed library, license, third-party notices, version, commit, provenance,
manifest, and checksums. Neither component is added to the tagged main archive.

Complete `.dsc` rebuilds run with network denied and empty Cargo/Rustup caches.
The build uses only the extracted source components and declared distro build
dependencies, with Cargo locked and offline. The clean rebuild must produce the
same package identity, resolved dependencies, installed path set, and installed
file hashes as the release build. Fresh installation leaves
`facelock-daemon.service` disabled and inactive; D-Bus activation remains
available after explicit setup. Reinstall and upgrade restart the daemon only
when it was already active, including an active D-Bus-activated instance; they
preserve both enabled and disabled state and leave every inactive instance
inactive. The post-install convergence still removes the retired `facelock`
group, fixes ADR 010 ownership, refreshes a recognized legacy D-Bus policy
copy, asks the bus to reload policy, and registers the opt-in PAM profile
without selecting it. Package validation requires the installed TPM command
surface and the suite-native `libtss2` dependency closure.

Compat 13's generated `dh_installtmpfiles` post-install snippet is the sole
install-time tmpfiles activation and invokes `systemd-tmpfiles` for
`facelock.conf` only. The source `postinst` never runs a global tmpfiles create,
so another package's configuration cannot be activated by a Facelock
transaction.

Trixie's debhelper 13.24 omits its remove-only service stop when
`dh_installsystemd --no-start` is used, while Resolute's debhelper 13.31 emits
it. The package build therefore appends debhelper's canonical
`prerm-systemd-restart` template for the exact Facelock unit only when that stop
is absent. This compatibility path is idempotent: both suites produce exactly
one stop after successful PAM cleanup, neither starts or enables the daemon on
installation, and only the generated purge path retires enabled state.

## ONNX Runtime Trust and Fedora RPM Modes

ONNX Runtime (ORT) is executable code loaded into the daemon, the
PAM-spawned oneshot helper, and other privileged Facelock processes. A runtime
must therefore be selected deterministically and validated **before** it is
mapped. Loading a bare `libonnxruntime.so.1` through the dynamic linker's
ambient search path and inspecting it afterward is forbidden: ELF constructors
may already have executed before any post-map rejection.

### Deterministic candidate order

The resolver considers candidates in this order and stops at the first one
that passes the applicable trust checks and initializes ORT:

1. A non-empty `ORT_DYLIB_PATH`, **only in an unprivileged process**.
2. Trusted system locations for the configured GPU provider. ROCm first checks
   `libonnxruntime.so.1` beneath `/usr/lib64/rocm/lib`, then
   `/usr/lib/rocm/lib`; any non-CPU provider then checks the configured-GPU
   compatibility name `libonnxruntime.so` beneath `/usr/lib64`, then
   `/usr/lib`.
3. Package-manager stable-SONAME candidates
   `/usr/lib64/libonnxruntime.so.1`, then
   `/usr/lib/libonnxruntime.so.1`.
4. Facelock package-owned stable-SONAME candidates beneath
   `/usr/lib64/facelock`, then `/usr/lib/facelock`, followed by the existing
   package-owned unversioned Debian compatibility names in those same roots.

The CPU provider skips step 2. A system runtime therefore precedes a bundled
CPU fallback even in a direct package. A missing or rejected candidate advances
to the next fixed candidate; no other directory is searched.

A process is privileged when its real or effective UID or GID is 0, its
real/effective UID or GID differs, the kernel marks it `AT_SECURE`, or the
calling thread has any inheritable, permitted, effective, or ambient Linux
capability. Capability inspection reads `/proc/thread-self/status`, never the
thread-group leader's status; an unreadable file or a missing, duplicate,
empty, or malformed capability field fails closed as privileged. Every such
process ignores `ORT_DYLIB_PATH` entirely and has no `/usr/local` candidate.
The explicit override is an unprivileged caller choice: it is still opened and
checked as a bounded ELF with the required architecture and SONAME before
mapping, but it does not claim package-manager root ownership.

### Privileged pre-map validation

Every privileged system or bundle candidate has a fixed approved trust root
and a normal relative path beneath it. One descriptor-held component walker is
used on every kernel, with no alternate or weaker kernel-version path. The
loader:

- requires the trust root, each ancestor, and every traversed directory to be
  root-owned and not group- or world-writable; a linked trust root is rejected,
  and the fixed root is opened and retained with
  `O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_NONBLOCK`
- inspects every relative component and link through a held parent descriptor
  using `O_PATH|O_NOFOLLOW|O_NONBLOCK`; directory links, absolute targets,
  non-normal targets such as `.` or `..`, and paths that escape and later
  return beneath the root are rejected
- follows only a root-owned, single-link, relative package SONAME chain beneath
  the held root; every link target is decomposed and walked again from held
  descriptors rather than resolved by an ambient pathname lookup
- opens the final object through its held parent with
  `O_RDONLY|O_NOFOLLOW|O_NONBLOCK`, then requires a bounded regular file with
  exactly one hard link, root ownership, no group/world write bits, no
  setuid/setgid bits, and no `security.capability` xattr
- requires device, inode, link count, size, UID/GID, mode, modification time,
  and change time to remain stable across component inspection, the final
  open, the bounded read, and the last pre-map check
- requires a 64-bit ELF for the running architecture, SONAME exactly
  `libonnxruntime.so.1`, and no RPATH/RUNPATH entry except exactly `$ORIGIN` or
  `${ORIGIN}`

Only after every check passes is the same held read descriptor mapped (for
example through `/proc/self/fd/<fd>`). No pathname is reopened after validation.

If every candidate is missing, rejected, or fails ORT initialization, model
loading fails and authentication degrades through its existing password
fallback. Authentication never downloads a runtime or model.

### Fedora package modes

`dist/facelock.spec` has two mutually exclusive ORT modes:

| RPM channel | Spec mode | Runtime payload and dependency contract |
|-------------|-----------|-----------------------------------------|
| GitHub direct RPM (Fedora 44) | `--with bundled_ort` | Installs the pinned CPU runtime as `%{_libdir}/facelock/libonnxruntime.so.1.20.1` with a package-owned `libonnxruntime.so.1` symlink; carries no `BuildRequires` or `Requires` on Fedora `onnxruntime` |
| Packit/COPR (Fedora 43/44/45) | default `%bcond_with bundled_ort` disabled | Contains no bundled ORT library or bundle metadata; `BuildRequires` and `Requires` Fedora's runtime-only `onnxruntime` package, with `onnxruntime-devel` absent |

The COPR `%check` constructs a real ORT session from the checksum-pinned
minimal model in `test/fixtures/`; finding a library or running
`facelock --version` is not a substitute. The two RPM validators independently
reject a direct RPM with a system-ORT dependency and a COPR RPM with bundled
payload, and reject the inverse missing dependency/payload.

Track D validates only direct/COPR build success, real ORT runtime
initialization, and the intended payload and dependency policy. It supplies no
clean-install, upgrade, erase, rollback, served-repository, availability,
alpha-acceptance, or release evidence. Issue #230 owns exact-artifact package
lifecycle proof; issue #236 owns staging and production repository publication
and served-version proof.

Optional experimental Rawhide may attempt only the separately digest-pinned,
best-effort system-ORT build/session smoke. It is non-release and non-gating,
must not publish or modify a COPR channel, and cannot substitute for any
supported Fedora result or any lifecycle, artifact, served-version,
availability, alpha-acceptance, or release evidence.

### Reviewed direct-bundle identity

The direct RPM bundle is exactly:

| Field | Reviewed value |
|-------|----------------|
| Version | `1.20.1` |
| Upstream archive | `https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-linux-x64-1.20.1.tgz` |
| Archive SHA-256 | `67db4dc1561f1e3fd42e619575c82c601ef89849afc7ea85a003abbac1a1a105` |
| Upstream commit | `5c1b7ccbff7e5141c1da7a9d963d660e5741c319` |
| Library SHA-256 | `a5faaf78a37590d3fe640f887620e74f6022d34550172b91ad2131bf0ad77d64` |
| License identity | MIT |

The release network stage downloads the archive to a file and verifies the
archive digest **before extraction**. It then verifies `VERSION_NUMBER`,
`GIT_COMMIT_ID`, and the library digest against the reviewed values. Streaming
an unverified response into `tar` is forbidden.

The prepared bundle contains the exact library plus upstream `LICENSE`,
`ThirdPartyNotices.txt`, `VERSION_NUMBER`, and `GIT_COMMIT_ID`, and generated
`PROVENANCE.md`, `manifest.json`, and `SHA256SUMS`. The checksum file covers
the library and every listed metadata/provenance file except itself. Direct RPM
assembly requires and re-verifies the complete prepared bundle.

Before creating the source archive or any rpmbuild tree, the whole assembly
enters `.github/workflows/scripts/run-networkless.sh`. That wrapper uses
util-linux `enosys` as a fail-closed seccomp boundary: it denies socket
creation/connection and message syscalls plus `io_uring_setup`, closes every
inherited non-stdio file descriptor, and requires a socket probe to fail with
`ENOSYS` before it invokes the assembly command. Cargo offline mode remains
defense in depth; it is not the network-isolation boundary.

The installed `libonnxruntime.so.1.20.1` bytes must retain the exact reviewed
library digest above. Fedora strip/debug/post-processing must not rewrite the
pinned runtime; bundled mode disables the modifying strip hook, and validation
extracts the final RPM member and checks its digest. The RPM also ships the
license, notices, version, commit, checksums, provenance, and component manifest
under its documentation/license directories.

Those metadata files are inputs for later SBOM, release-manifest, attestation,
and signing work. Their presence does **not** claim that Track D generated or
signed a final SBOM/manifest, signed the RPM, or published a release. Issue
#235 owns native signing and final immutable direct-artifact publication.

### RPM tmpfiles transaction

The RPM transaction creates Facelock's runtime directories through the
package-scoped `%tmpfiles_create facelock.conf` invocation. It must not run a
global `systemd-tmpfiles --create` or otherwise process unrelated packages'
tmpfiles configuration. Package validation observes the directories created by
the actual install transaction and does not manufacture them with a later
global tmpfiles command.

## Package Lifecycle Ownership

This is the Wave 0 ownership freeze for issue #232. It defines what later
package lifecycle work is allowed to remove; it does not claim that the current
Debian purge script already implements the bounded purge described below.
**Ordinary removal is not data deletion.** Removing the package must leave a
machine reinstallable without losing its biometric or operational state.

The ownership classes are deliberately separate:

| Class | Examples | Ordinary removal |
|-------|----------|------------------|
| Package-owned static integration | binaries and shared libraries, systemd/OpenRC/runit/s6 units, D-Bus policy and activation, tmpfiles configuration, shipped quirks, PAM/authselect profiles, translations, bundled runtime libraries | Remove through the package manager. These files can be recreated byte-for-byte by reinstalling the package |
| Administrator configuration | `/etc/facelock/config.toml` and the package manager's saved replacement for an administrator-modified copy | Apply the native package-family rules below. Do not treat administrator configuration as biometric state or as disposable static integration |
| Biometric and operational state | the database and its WAL/SHM sidecars, encryption keys and sealed keys, downloaded models, enrollment markers, setup state, audit logs, and snapshots under the compiled roots | Preserve all of it. A reinstall reuses it; ordinary removal never interprets absence of the package as consent to discard it |
| PAM integration and provenance | a `pam_facelock.so` rule, a Facelock-created local override and its provenance header, and `<service>.facelock-backup` rollback files | Attempt safe cleanup inside the fixed PAM root. Delete provenance only after the corresponding PAM cleanup is proven complete |
| Externally configured state | any database, model directory, key, sealed key, audit log, or snapshot path configured outside the compiled Facelock roots | Never package-owned. Leave it untouched and report it as an external remnant |

PAM provenance and rollback files are not biometric state. They exist to
explain or reverse an authentication-stack edit, so retaining all of them
forever makes an otherwise successful uninstall look incomplete. Conversely,
deleting them before the PAM edit is known to be gone destroys the evidence and
rollback path for a service that still references a removed module.

**Preserve PAM provenance when cleanup is incomplete; remove it only after
successful cleanup.** Successful cleanup means the service file was safely
resolved inside `/etc/pam.d`, its Facelock rule was removed (or was already
absent), and any candidate override or backup was proven to be Facelock-created
and no longer needed. A Facelock-created override may be deleted to reveal its
vendor file only when it has no administrator changes. Never restore a backup
over a newer service file merely because the backup exists. An unreadable,
unwritable, wrong-owner, non-regular, changed, linked, or mount-separated
service file makes cleanup incomplete: preserve its override, provenance
header, and `.facelock-backup`, and report the exact remnant. Cleanup of one
service does not authorize deleting provenance for a different service.

### Native configuration lifecycles

The package families reach the same ownership result through different native
mechanisms:

| Family and operation | Administrator-configuration contract |
|----------------------|--------------------------------------|
| Debian `remove` | `/etc/facelock/config.toml` is a Debian conffile and remains at its installed path. Biometric and operational state also remains |
| Debian `purge` | `dpkg` removes the conffile, and the post-removal purge may then remove only safe remnants inside the compiled roots. Unsafe and external remnants are retained and reported |
| RPM erase | `/etc/facelock/config.toml` is RPM `%config(noreplace)`. RPM removes an unmodified copy and retains an administrator-modified copy according to RPM semantics, commonly as `config.toml.rpmsave`. A `.rpmsave` is retained state, not evidence of a failed erase and not something a Facelock script deletes |
| Arch package removal | the `backup` entry follows pacman's native saved-configuration behavior (including `.pacsave` when applicable). Facelock lifecycle code does not bypass it |
| `just uninstall` | no package manager owns the config, so the source-install uninstall preserves `/etc/facelock` with the biometric and operational state |

Debian `postrm purge` is self-contained. It never invokes the already-removed
`facelock` binary. By the time `postrm` runs, package payloads cannot be treated
as available cleanup tools. The future bounded purge must make its decisions
from fixed constants and the remaining filesystem state, using only utilities
that the maintainer script can rely on after removal; it cannot delegate safety
checks or deletion to the CLI it is purging.

RPM and Arch have no Debian-style second `purge` phase. Their ordinary erase
therefore removes static integration and safely cleaned PAM provenance, while
preserving biometric state and whatever administrator-configuration artifact
their package manager retained.

### Fedora authselect retirement boundary

The RPM does not ship or select an authselect profile, does not edit
`system-auth` or `password-auth`, and has no runtime or scriptlet dependency on
authselect. Fresh installation is PAM-inert. The supported opt-in is an
explicit, named leaf service through `facelock pam add --service <name>` or its
`setup --pam --service <name>` alias. That operation edits only the resolved
leaf service plus Facelock's fixed backup state; the selected authselect profile
and shared generated files remain byte-for-byte unchanged.

An incoming RPM upgrade runs the source-controlled
`facelock-authselect-retirement-guard` from `%pre`, while the old payload is
still installed. A fresh transaction is an immediate no-op. An upgrade also
succeeds without authselect installed or when the fixed selection-state file
`/etc/authselect/authselect.conf` is absent.

An already-installed v0.1.4 RPM cannot be retroactively guarded: direct
uninstall runs only that installed release's unguarded scriptlets.
Administrators must install a guarded release before a later uninstall so the
upgrade guard can first retire the old authselect payload safely.

When that file exists, the guard reads no other authselect path and invokes no
authselect command. It requires a root-owned, root-group, regular, single-link
0644 file of at most 16 KiB, compares the first line's original bytes with its
shell-decoded value so no NUL or other control byte can be discarded, and then
accepts only the profile grammar used by authselect: one confined profile
identifier, `custom/<identifier>`, or `@system-default`. A malformed, linked,
oversized, control-bearing, or wrong-metadata file is untrusted and blocks the
package transaction without changing it.

The exact retired profile identifier `facelock` also blocks upgrade. The
diagnostic requires the administrator to inspect the active identity provider
and features, select an appropriate supported profile while asking authselect
to create a backup, and retry the RPM transaction. Facelock does not guess a
replacement or migrate generated PAM state. A different valid identifier,
including the separately administrator-owned `custom/facelock`, is preserved
unchanged and does not block the upgrade.

The booted Fedora lifecycle test uses the released 0.1.4 RPM to prove fresh,
unselected, selected-retired, custom-profile, malformed-state, and
authselect-absent upgrade cases. It also proves correct and wrong password
fallback through the real selected profile, and the real RPM package test
proves that service-scoped setup and removal leave the selection and shared
generated files unchanged. Neither test mutates the host PAM stack.

### Fixed-root purge boundary

The only purge roots are the compiled Facelock roots:
`/etc/facelock`, `/var/lib/facelock`, and `/var/log/facelock`. `/etc/pam.d` is
a separate, fixed root for the narrow PAM cleanup above; it is never a recursive
purge root. A configured path that remains within a compiled Facelock root is
eligible for a later Debian purge only under the same safety checks as every
other descendant.

Configured paths outside those roots are external remnants. This includes
external values of `daemon.model_dir`, `storage.db_path`, `encryption.key_path`,
`encryption.sealed_key_path`, `audit.path`, and `snapshots.dir`. Removal and
purge must leave them untouched, report that they were refused as external, and
must not claim that all Facelock data is gone. A path becoming external through
configuration does not expand package ownership.

Any later purge implementation operates from fixed path constants and examines
each entry without trusting path traversal. It must never follow a symbolic link
or act through a hard-linked object, never cross a mount point, and never recurse through a
non-directory or an object whose ownership cannot be proven safe. A root or
descendant that fails those checks remains in place and is reported. A safe
root may still be cleaned around an unsafe child, but the final report must name
every remnant rather than describe the root as removed.

Safety refusals and external remnants must not strand package-manager state.
In particular, Debian purge reports and preserves an unsafe object but lets the
maintainer-script lifecycle finish, so a link, mount, or wrong-owner file cannot
leave the package permanently half-purged. This is not a broad recursive-delete
contract: later code must enumerate the bounded roots and reject anything it
cannot prove is inside them.

Finally, filesystem removal does not promise secure erasure. Unlinking files
does not guarantee that data is absent from SSD flash translation layers,
snapshots, backups, journal history, or remapped blocks. Lifecycle messages may
say which names were removed and which remnants remain; they must not describe
purge as forensic destruction of biometric data.

## Filesystem Paths

| Path | Owner | Mode | Purpose |
|------|-------|------|---------|
| `/etc/facelock/config.toml` | root:root | 644 | Configuration |
| `/var/lib/facelock/` | root:root | 711 | State dir. Traversable by every local user, listable by root only: anyone can open a path it knows by name (its own enrollment marker, a model file) but nobody can enumerate what is there (`models/` is itself `0755` and listable — public data) |
| `/var/lib/facelock/facelock.db` | root:root | 600 | Face embeddings. Read by the daemon (root) only; user-run PAM stacks request authentication through the daemon, they never read templates |
| `/var/lib/facelock/models/` | root:root | 755 | ONNX models — public, SHA256-verified downloads |
| `/var/lib/facelock/enrolled/` | root:root | 711 | Enrollment markers; traversable by all, listable by none |
| `/var/lib/facelock/enrolled/<user>` | \<user\>:\<user\> | 600 | `{"models": N, "updated": "<ISO8601>"}` — a hint for `is-enrolled`, never authoritative |
| `/var/lib/facelock/pam-backups/` | root:root | 700 | Fixed-root PAM rollback state; not affected by `[pam].config_dirs` or `storage.db_path` |
| `/var/lib/facelock/pam-backups/<service>.<seconds>-<nanoseconds>` | root:root | 600 | Original PAM service bytes; nanoseconds are exactly nine digits |
| `/var/lib/facelock/pam-backups/<service>.<seconds>-<nanoseconds>.json` | root:root | 600 | Strict versioned provenance for the adjacent rollback bytes |
| `/var/lib/facelock/pam-backups/.facelock-remove-all-<operation>.json` | root:root | 600 | Strict prepared whole-set PAM removal journal |
| `/var/lib/facelock/pam-backups/.facelock-remove-all-commit-<operation>.json` | root:root | 600 | Strict self-contained whole-set commit and recovery marker |
| `/var/log/facelock/` | root:root | 700 | Log dir — per-user auth history and raw face snapshots are root-only |
| `/var/log/facelock/audit.jsonl` | root:root | 600 | Structured audit log |
| `/var/log/facelock/snapshots/` | root:root | 700 | Auth snapshots (raw face images) |
| `/usr/bin/facelock` | root:root | 755 | CLI binary |
| `/lib/security/pam_facelock.so` | root:root | 755 | PAM module |

Config-described data and model paths are overridable as documented by their
schema fields. `/var/lib/facelock/pam-backups` is deliberately fixed for the
PAM writer and shared state layout. Neither `[pam].config_dirs` nor
`storage.db_path` redirects it: the former selects PAM service roots and the
latter relocates biometric database state only. `FACELOCK_CONFIG` is honored
for unprivileged processes, but privileged PAM/root auth flows ignore the
environment and use either an explicit `--config` path or
`/etc/facelock/config.toml`.
Runtime-created DB sidecars (`-wal`, `-shm`), audit logs, and snapshots are created with explicit restrictive modes. The packaged systemd unit also sets `UMask=0027`.

#### Traversal for everyone, listing for nobody (ADR 010)

The state directory and `enrolled/` are `0711 root:root`: any local user may
*enter* them, nobody but root may *list* them. That is the whole grant. Every
entry below is locked down in its own right — `0600 root:root` database and
sidecars, `0600 <user>:<user>` markers, and a `0700 root:root` PAM-backup
directory containing `0600 root:root` state files — and `models/` is the one
subtree that carries "other" read bits of its own, because its contents are
public, SHA256-verified downloads. There is no group in the file contract:
nothing under `/var/lib/facelock` is group-owned or group-readable (ADR 010).

D-Bus is required for user-run screen lockers (hyprlock/swaylock) and the
polkit agent — their PAM stack runs as the user, and nothing makes the `0600
root:root` database or encryption key readable to them — and the bus admits
their `Authenticate` call without any group (see IPC Protocol). Root-invoked
PAM (`sudo`, `login`, `sshd`) can also use the oneshot fallback, which reads
the files directly as root.

Known residual: any local user can `stat` a path it can guess by name —
`facelock.db` (size, mtime) or `enrolled/<user>` (existence) — because
traversal permits exactly that. Closing it would mean denying the traversal
that `is-enrolled` and model loading depend on. Accepted; before ADR 010 the
same residual existed for `facelock` group members.

#### Contract change: permissions tightened (no paths moved)

The default paths are unchanged — the database stays at
`/var/lib/facelock/facelock.db` and the models at `/var/lib/facelock/models`;
**no data moves on upgrade**. What changed are modes and ownership, recorded
here per the repo rule that path and permission contracts live in this file:

| Path | Was | Now |
|------|-----|-----|
| `/var/lib/facelock/` | 750 root:facelock | 710 root:facelock |
| `/var/lib/facelock/facelock.db` (+`-wal`/`-shm`) | 640 root:facelock | 600 root:root |
| `/var/lib/facelock/models/` | 755 root:root | 755 root:root (unchanged) |
| `/var/lib/facelock/enrolled/` | — (new) | 710 root:facelock |
| `/var/log/facelock/` | 750 root:facelock | 700 root:root |
| `/var/log/facelock/audit.jsonl` | 640 root:facelock | 600 root:root |
| `/var/log/facelock/snapshots/` | 750 root:facelock | 700 root:root |

The group loses direct reads of the database, the audit log (per-user auth
history) and the snapshots (raw face images) — all strictly more sensitive
than anything the group needs, since every group operation goes through the
daemon. For an existing install the entire on-disk change is a `chmod`/`chown`
of the paths above plus `mkdir enrolled/` — idempotent, applied by packaging
(tmpfiles, install scriptlets) and re-applied by any root invocation of the
binary; none of it touches the data itself.

#### Contract change: traversal opened to every local user (ADR 010)

No paths move. The two directories that carried a group grant drop it:

| Path | Was | Now |
|------|-----|-----|
| `/var/lib/facelock/` | 710 root:facelock | 711 root:root |
| `/var/lib/facelock/enrolled/` | 710 root:facelock | 711 root:root |

Everything else in the table above is unchanged. For an existing install the
on-disk change is a `chmod`/`chown` of those two directories — idempotent,
applied by packaging (tmpfiles, install scriptlets) and re-applied by any root
invocation of the binary (`ensure_state_layout` on daemon start, best-effort on
the auth path). `sudo facelock setup`, `just install-files` and the package
scriptlets remove a leftover `facelock` group best-effort; `sudo groupdel
facelock` if it lingers.

### Audit Log Entries

`audit.jsonl` is JSONL; each line carries `timestamp`, `user`, `result` (`success`, `failure`, `error`, `rate_limited`, `suppressed`, `cancelled`) and, when known, `similarity`, `frame_count`, `duration_ms`, `device`, `model_label`, `error`.

`cancelled` (ADR 008 §5) is an attempt that was **abandoned, not answered**: the caller's bus connection went away, the system suspended, `ReleaseCamera` arrived, or a one-shot process was signalled. It is deliberately not a `failure` — no comparison reached a verdict, so it charges no rate-limit budget. The entry carries `frame_count` and `duration_ms` (how far the attempt got) and no `similarity`.

`source` names the code path that produced the entry — `daemon` (the `Authenticate` D-Bus method), `oneshot` (the `facelock auth` helper PAM spawns), or `test` (`facelock test`, on either transport: the daemon's `TestAuthenticate` method or the in-process direct loop). It records the **enforcement path, not the caller's identity**: `daemon` and `oneshot` are fully-enforced authentications whose failures count against the rate limit, while `test` skips the SSH/lid physical-presence gates and charges nothing. So a `success` stamped `test` is a recognition result, not a policy-approved authentication — and a real authentication is never stamped `test`, whatever privilege its caller holds. The field is absent on entries written before it existed.

## Config Schema

TOML format. All keys optional — camera auto-detected, sensible defaults for everything.

### Sections

| Section | Key fields |
|---------|-----------|
| `[device]` | `path` (Option), `max_height`, `rotation`, `warmup_frames`, `dark_threshold`, `dark_pixel_value`, `ir_emitter`, `camera_release_secs`, `camera_release_after_success_secs` |
| `[recognition]` | `threshold`, `timeout_secs`, `no_face_timeout_secs`, `detector_model`, `detector_sha256`, `embedder_model`, `embedder_sha256`, `threads`, `execution_provider` |
| `[daemon]` | `mode` (DaemonMode enum), `model_dir`, `idle_timeout_secs` |
| `[storage]` | `db_path` |
| `[security]` | `disabled`, `suppress_unknown`, `require_landmark_liveness`, `require_ir`, `require_frame_variance`, `frame_variance_max_similarity`, `ir_texture_min_stddev`, `min_auth_frames`, `bind_templates_to_device`, `device_match_granularity`, `bind_legacy_templates`, `bind_device_aad`, `allow_plaintext`, `abort_if_ssh`, `abort_if_lid_closed`, `pam_policy`, `rate_limit` |
| `[notification]` | `mode` (off/terminal/desktop/both), `notify_prompt`, `notify_on_success`, `notify_on_failure` |
| `[snapshots]` | `mode` (off/all/failure/success), `dir` |
| `[encryption]` | `method` (keyfile/tpm/none — **default keyfile**), `key_path`, `sealed_key_path` |
| `[audit]` | `enabled`, `path`, `rotate_size_mb` |
| `[tpm]` | `seal_database`, `pcr_binding`, `pcr_indices`, `tcti` |
| `[polkit]` | `face_eligible_actions` |
| `[pam]` | `config_dirs` |

`[polkit].face_eligible_actions` is the allowlist of polkit `action_id`s for which
the face authentication agent may offer face auth. Default:
`["org.freedesktop.login1.lock-sessions"]`. Any action not in the list is declined
by the agent. An empty list disables face for all actions. High-risk actions
(pkexec, PackageKit, udisks mount, accounts-service) are excluded by default.

**Scope:** this allowlist governs the **agent model** only. Under the **PAM model**
(`pam_facelock.so` as `auth sufficient` in `/etc/pam.d/*`, the common Howdy-style
deployment that also covers `sudo`), the list is ignored: face is attempted for
every action in that PAM stack, always with password fallback because the line is
`sufficient`, never `required`. See `docs/security.md` §7a/§7b for the two models.

**NOTE (agent model only):** polkit registers a single authentication agent per
session and does not chain agents. When this agent declines a non-allowlisted
action it returns an error, which — depending on the desktop's agent
registration — may present as an authorization denial rather than a
fallthrough to a password dialog. The intended UX (non-eligible actions
handled by the desktop's normal password agent) is unverified pending
live-desktop testing and may require a design change. Behavior here is
fail-closed: a non-eligible action is never face-authorized.

`[pam].config_dirs` is where named `facelock pam add | remove | status` looks
for PAM service files, in search order — Linux-PAM's own precedence, earliest
wins. `facelock pam remove --all` always uses the compiled `/etc/pam.d`,
`/usr/lib/pam.d`, and detection-only `/etc/authselect` roots and cannot be
redirected by configuration.
Default: `["/etc/pam.d", "/usr/lib/pam.d"]`. **The first entry is the override
directory: every write lands there and every later entry is read-only**, so a
service that resolves only in a later one is copied into the first before the
line is inserted. Setting a package-owned directory first would make facelock
edit package files. An empty list is treated as the default rather than as a
request to disable the writer, and so is **any list with a non-absolute entry**
— a relative first entry would resolve the write target against the invoking
shell's working directory — and **any list whose first entry is also one of the
later ones**, spelled twice or reached through a symlink, which would collapse
the override layer onto a read-only one. `pam` is dispatched before the process-wide config
parse, so a missing or broken config yields the default list rather than an
error — editing `/etc/pam.d` must not be blocked by an unrelated config
mistake. `FACELOCK_CONFIG` is ignored in a privileged process, so the
environment cannot redirect where a root `pam add` writes; the global
`--config` flag is a process override and *is* honoured under `sudo`, which is
root naming a different file on purpose rather than the environment doing it
behind root's back. See "facelock pam
Semantics" above for the resolution rules themselves.

**Encryption defaults (Plan 04).** `encryption.method` defaults to `keyfile`: face
templates are encrypted at rest by default. The keyfile is auto-generated at mode `0600`
on first use if absent. `method = "none"` (plaintext) is **refused at enrollment** unless
`security.allow_plaintext = true`. Auth always degrades to password on a decrypt failure —
never a lockout.

**Camera hold semantics (ADR 008).** `device.camera_release_secs` (default **3**) is the
number of seconds the **daemon** keeps the camera streaming **after a failed
authentication** — the one ending a retry plausibly follows — so that retry skips the
reopen cost. A success releases the camera immediately **unless**
`device.camera_release_after_success_secs` (default **0**) is greater than zero, in which
case a success holds for that many seconds instead; it is an opt-in for repeated
privileged actions with no authentication caching in front of them, and at its default
nothing about a success changes. Cancellation and every error (including a capture failure
or an all-dark scan) always release immediately, whatever both keys say: the interaction is
over, and on IR hardware the emitter LED goes out with it. `camera_release_secs = 0` means
**never hold** after a failure; it previously fell back to 5 seconds. Enrollment follows
the same rule as authentication, on both keys. Preview frames are exempt: each one extends
the hold to `max(camera_release_secs, 2s)` so a ~10 fps preview never reopens per frame,
and the CLI still calls `ReleaseCamera` on exit. The hold deadline is absolute and polled
every 250 ms. One-shot mode (`facelock auth`) never holds — process exit is the release —
and ignores both keys. Changing either value needs no daemon restart: they are read per
request.

**Hard device binding (opt-in).** `security.bind_device_aad = true` folds the enrolling
camera's `device_id` into the AES-GCM AAD, so a template cannot be decrypted under a
different camera. Default false (fails closed on unstable ids). Complements the advisory
device coupling of Plan 02.

**TPM sealed-key format & unseal semantics (Plan 04).** The sealed-key blob is versioned:
`0x01` = no PCR policy; `0x03` = PCR-bound, and self-describes its PCR index list. A
PCR-bound object is created with `userWithAuth = false`, and unseal starts a real policy
session and replays `PolicyPCR` — so a changed bound PCR makes unseal **fail** (finding #5).
`facelock tpm reseal` re-seals the key under the current PCRs (recovery path).

### Camera Auto-Detection

When `device.path` is omitted:
1. Enumerate `/dev/video0` through `/dev/video63`
2. Filter to VIDEO_CAPTURE devices
3. Classify every node's IR provenance from queried evidence: a quirks
   `force_ir` match (authoritative by USB vendor:product ID; a name-only match
   only when corroborated by a real USB identity or the node's own mono-format
   evidence), otherwise a node whose queried formats are mono-only/IR-typical
   (GREY/Y8/Y10/Y12/Y16, with no color format mixed in). The device name never
   classifies a node on its own. Node-level disambiguation for multi-node USB
   devices: when several nodes share one quirk-matched VID:PID and at least one
   has an IR-typical format (GREY/Y8/Y10/Y12/Y16), only the format-bearing
   node(s) are IR. A quirk's `format_preference` counts as node-level IR
   evidence only when it is itself IR-typical and the node actually advertises
   it
4. Exclude devices that advertise no decodable pixel format
   (GREY/Y16/YUYV/NV12/MJPG) — e.g. raw Bayer sensor nodes (Intel IPU6/IPU7).
   This filter runs *after* step 3 and never feeds back into it: it changes
   which node is selected, never whether a node counts as IR. The IR-typical
   list (step 3) and the decodable list are deliberately different sets — a
   node whose only IR evidence is Y8/Y10/Y12 is IR **and** undecodable, and is
   excluded here with a syslog warning naming its path and formats
5. Among the remaining nodes, prefer a quirks-confirmed IR node with a native
   IR format, then any quirks-confirmed IR node, then an evidence-classified IR
   node (breaking ties toward one whose name also carries an `ir`/`infrared`
   token — a hint only, never a promotion of a node that lacks format evidence)
6. Fall back to first decodable device; if none, error listing every detected
   device and its formats

Opening a device (auto-detected or explicit `device.path`) negotiates a format
in priority order `quirk format_preference > GREY > Y16 > YUYV > NV12 > MJPG`
and **fails** if the device advertises none of them (no silent fallback to an
undecodable format).

A quirk's `format_preference` is compared whitespace-trimmed and is **dropped
with a warning** if it names a format facelock cannot decode, rather than
winning negotiation and then failing every capture.

On a Y16 device, open also pins the session's 16-bit-to-8-bit shift, which is
never recomputed per frame (`docs/security.md` §1.C). A quirk's `y16_bit_depth`
(8..=16) is authoritative and skips frame inspection; otherwise the shift comes
from the brightest sample in a burst of frames captured at open (at least the
device's `warmup_frames`; the burst stops starting captures after one second,
so a dequeue already in flight can carry it to roughly one second plus one
`CAPTURE_TIMEOUT`). The pinned shift belongs to
the open camera: a warm hold (see "Camera hold" above) keeps it, and a reopen
recalibrates. A Y16 device that produces no frame at all within the calibration
budget fails `Camera::open` rather than opening with a guessed scale.

Open also **rejects a padded stride**: for GREY/NV12 (`bytesperline == width`)
and Y16/YUYV (`bytesperline == 2 * width`), a device reporting anything else
errors at open instead of decoding sheared frames. Compressed formats (MJPG)
are exempt — their `bytesperline` is not a row size.

**FourCC normalization.** V4L2 pads FourCCs to four characters with trailing
spaces (`"Y16 "`). Facelock strips that padding at every ingest point — device
enumeration (`query_device`) and quirks-file parsing — so `DeviceInfo.formats`
carries the unpadded spelling (`"Y16"`, not `"Y16 "`).

The only machine-readable surface that changes is `facelock devices --json`
**on the direct backend**, which is where format detail exists at all: the
D-Bus `DeviceInfo` does not carry formats, so under the daemon backend
`--json` reports `"formats": []` and there is no spelling to change
(`BackendCaps::device_formats`, false for `BackendKind::Daemon`). The
human-readable `facelock devices` table already trimmed.

## Database Schema

SQLite with WAL mode and foreign keys:

```sql
CREATE TABLE face_models (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user TEXT NOT NULL,
    label TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    embedder_model TEXT NOT NULL DEFAULT '',  -- V5: embedder that produced the embeddings
    device_id TEXT,                           -- V6: enrolling camera fingerprint "vid:pid:serial" (NULL = legacy/uncoupled)
    UNIQUE(user, label)
);

CREATE TABLE face_embeddings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_id INTEGER NOT NULL REFERENCES face_models(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL,  -- 512 x f32 = 2048 bytes (or encrypted blob)
    sealed INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE rate_limit (
    user TEXT NOT NULL,
    attempt_time INTEGER NOT NULL
);
```

Only failed authentication attempts are recorded in `rate_limit`, and only those where a face was actually detected (ADR 008 §4 — see §facelock test Semantics for the full charging rule). Daemon mode and oneshot mode share the same SQLite-backed window, so daemon restarts do not clear lockout state.

**Schema version** is tracked in `schema_version`; migrations are additive and forward-only. Current version: **6**. Migration V6 adds the nullable `face_models.device_id` column (Plan 02 device coupling); pre-V6 databases open cleanly, keep their rows, and leave `device_id` NULL. NULL rows are governed by `security.bind_legacy_templates` (default allow-with-warn), so upgrades never lock a user out.

`device_id` is the canonical fingerprint (`"vid:pid:serial"`) of the camera that enrolled the template. It is **model-granularity at best and forgeable by a programmable USB device** — advisory defense-in-depth, NOT attestation. See `docs/security.md` §Device Coupling.

## IPC Protocol

D-Bus system bus (`org.facelock.Daemon`). Only used in daemon mode.

The daemon registers on the system bus via D-Bus activation.

- **Bus name**: `org.facelock.Daemon`
- **Object path**: `/org/facelock/Daemon`
- **Interface**: `org.facelock.Daemon`

### Methods
`Authenticate`, `TestAuthenticate`, `Enroll`, `ListModels`, `RemoveModel`, `ClearModels`, `PreviewFrame`, `PreviewDetectFrame`, `ListDevices`, `ReleaseCamera`, `Ping`, `Shutdown`

Method authorization contract (updated under DEC-6/N13 — the CLI's
root-by-default privilege map left no unprivileged consumer for most of
these, so tightening them to root-only closes the per-frame similarity
hill-climbing oracle by construction rather than by redacting fields):
- `Authenticate`: root or the matching Unix user. The one user-scoped method
  — screen lockers run their PAM stack as the user, so this is architecture,
  not policy. It is **real authentication**: a failed attempt always
  consumes rate-limit budget, whatever the caller's UID.
- `TestAuthenticate`: **root only.** Same arguments and same `AuthResult`
  reply as `Authenticate`, and the same gates except that it skips the
  SSH/lid physical-presence aborts and charges no rate-limit budget on
  failure (see "facelock test Semantics" above). It exists so the daemon
  never has to infer a caller's purpose from their privilege; root-only is
  what makes a budget-free endpoint safe to expose.
- Every other method — `Enroll`, `ListModels`, `RemoveModel`, `ClearModels`,
  `PreviewFrame`, `PreviewDetectFrame`, `ListDevices`, `ReleaseCamera`,
  `Ping`, `Shutdown` — is root only.
  The bus policy (`dbus/org.facelock.Daemon.conf`) grants root the whole
  interface and every local user exactly `Authenticate` (ADR 010) — so
  adding a root-only method needs no policy edit, and a future user-scoped
  method needs one deliberately; there is no group policy, and signals are
  root-only. The per-method root/user-scoped decision is the in-daemon check on
  the caller UID from `GetConnectionUnixUser`, keyed by a table-driven scope
  (`authorize_method` in `facelock_daemon::server`) so a new method is
  root-only by default until deliberately opened up.

Raw camera frames require privilege. Both `PreviewFrame` and
`PreviewDetectFrame` are root-only, so a non-root caller is denied with
`AccessDenied` before either method touches the camera. On top of that
denial the daemon strips `jpeg_data` from any non-root reply, so raw
camera/IR imagery cannot reach an unprivileged caller even if the
authorization table were ever to regress.

Method timeouts: `Enroll` runs synchronously inside the method call for up to
`Config::enroll_timeout_secs()` seconds server-side (`3 × max(recognition.timeout_secs, 5)`
seconds — i.e. minimum 15s). Clients MUST use a method timeout **greater
than** this deadline plus startup/inference margin for `Enroll` (the CLI uses
deadline + 15s); the shared 15-second client timeout applies to every other
method. A client timeout at or below the server deadline aborts the call while
the daemon is still enrolling.

Enrollment behavior is mode-independent: oneshot (`facelock enroll` in direct
mode) and the daemon's `Enroll` method run the same capture loop, so the
quality gate and the angle-diversity check apply in both.

Capture concurrency: `Authenticate`, `TestAuthenticate`, `Enroll`,
`PreviewFrame`, and `PreviewDetectFrame` are serialized by an in-flight
capture guard. While one capture is in progress, a concurrent call to any of
these methods fails **immediately** with an
`org.freedesktop.DBus.Error.Failed` error whose message contains `daemon
busy` (no queuing on the internal handler lock).
Clients (PAM included) must treat this like any other daemon error — degrade
to the next auth mechanism (password), never a lockout.

### Signals
- `AuthAttempted(user: s, matched: b)` — emitted after each camera-backed
  attempt, from `Authenticate` and `TestAuthenticate` alike. The payload
  intentionally carries **no similarity score** (the raw biometric score is
  an information leak / spoof-tuning oracle). The system bus policy
  (`dbus/org.facelock.Daemon.conf`) denies signal reception from the daemon
  by default; only root may receive it (ADR 010: no group policy).

### Response types
`AuthResult`, `Enrolled`, `Models`, `Removed`, `Frame`, `DetectFrame`, `Devices`, `Ok`, `Error`

`Models` carries `ModelInfo { id, user, label, created_at, embedder_model, device_id }`. `device_id` (added Plan 02) is the enrolling camera's canonical fingerprint; D-Bus has no Option type, so an **empty string is the NULL sentinel** for legacy/uncoupled templates (same convention as `AuthResult`).

### Authenticate error encoding

`Authenticate` returns `AuthResult (matched: b, model_id: i, label: s, similarity: d)`.
`TestAuthenticate` returns the same type with the same sentinels — one
encoding, so the two cannot drift.
Sentinel `model_id` values (only meaningful with `matched == false`):

| model_id | Meaning |
|----------|---------|
| >= 0 | Matched model id (with `matched == true`) |
| -1 | No match, and no face was detected (also: no enrolled faces, and the pre-camera gates) |
| -2 | Recoverable daemon error; `label` carries the error message (rate limited, IR required, camera/storage failure) |
| -3 | Suppressed: no enrolled models and `security.suppress_unknown = true` |
| -4 | No match, and the detector **did** see a face |

Recoverable errors travel **in-band** (model_id `-2`), not as D-Bus errors, so
clients can distinguish "the daemon decided auth cannot proceed" from "the
daemon is unavailable". D-Bus errors remain for authorization failures,
daemon-busy, and transport problems. In particular, a rate-limited state is a
daemon decision and must never make the PAM client retry via a root oneshot.

`-4` exists because `similarity` cannot carry "was a face seen?": the score is
redacted to `0.0` for every non-root caller, so a user-run locker (hyprlock)
could not tell a genuine face-seen non-match from an empty frame and abstained
(`PAM_IGNORE`) for both. It is a *detector* signal — a face was present, never
how close it came to an enrolled template — so unlike `similarity` it is not a
hill-climbing oracle and is not redacted.

A PAM module older than `-4` decodes it as an ordinary non-match (its sentinel
match falls through to the same arm as `-1`), so a daemon newer than the
installed module degrades to the previous behavior rather than breaking. In the
other direction, a `-1` reply carries no face-seen signal at all, and the module
falls back to the score test it used before.

### Rejection classes (`AuthOutcome::Error`)

The class of a rejection is carried as a type
(`facelock_daemon::auth::ErrorKind`), not inferred from its message. The audit
`result` label, the oneshot exit code, and the message itself all derive from
it; `ErrorKind::render` is the only place any of these sentences is written.
The wire has no field for the class, so the CLI's D-Bus client reconstructs it
with `ErrorKind::classify`, the exact inverse of `render`.

Three rendered messages are **frozen protocol** because the PAM module
matches them to choose its return code, and it cannot link the daemon
crate to share the type (its dependency ceiling is libc/toml/serde/zbus):

| Substring PAM matches | Class | PAM code |
|---|---|---|
| `rate limited` | `RateLimited` | `PAM_AUTH_ERR` |
| `IR camera required` | `IrRequired` | `PAM_IGNORE` |
| `cancelled` (matched **exactly**) | `AuthOutcome::Cancelled` | `PAM_IGNORE` |

Changing any of these strings is a protocol break.

`cancelled` is not an `ErrorKind`. A rejection class is a statement about this
user's face; a cancellation is the absence of one, so it is its own
`AuthOutcome` variant (`facelock_daemon::auth::CANCELLED_MESSAGE`) that reuses
the recoverable-error encoding to cross a wire with no field for it. PAM
abstains on it: the attempt was abandoned, so the daemon has no opinion and the
password modules run. It is matched exactly rather than as a substring, so an
arbitrary error message that happens to mention cancelling cannot claim the row.

**`auth_attempted` and a cancelled attempt.** The signal carries only `user` and
`matched`, and its signature is frozen; a cancelled attempt therefore emits
`auth_attempted(user, false)`, indistinguishable on the signal from a non-match.
The audit log is where the two are told apart (`cancelled` vs `failure`).

They are pinned byte-exactly in
`crates/facelock-daemon/src/auth.rs` (renderer, including the frozen
cancellation string) and
`crates/facelock-daemon/tests/server_authz.rs` (wire), and every class's
message, audit label and exit code are pinned together in
`crates/facelock-cli/src/commands/auth.rs`.

### Daemon peer verification (PAM client)

Before trusting an `Authenticate` reply, the PAM module resolves the owner of
`org.facelock.Daemon` (`GetNameOwner`, activating the service first if
needed), requires the owner UID to be 0 (`GetConnectionUnixUser`), and pins
the method call to the owner's unique bus name. A non-root owner is refused:
the module falls through (oneshot fallback / password), never `PAM_SUCCESS`.

## PAM Semantics

| Outcome | PAM Code |
|---------|----------|
| Face matched | `PAM_SUCCESS` (0) |
| No match, face seen (model_id -4) | `PAM_AUTH_ERR` (7) |
| No match, no face seen (model_id -1) | `PAM_IGNORE` (25) |
| Rate limited (daemon, model_id -2) | `PAM_AUTH_ERR` (7) — no oneshot fallback |
| IR required / internal daemon error (model_id -2) | `PAM_IGNORE` (25) — no oneshot fallback |
| Suppressed (model_id -3) | `PAM_AUTHINFO_UNAVAIL` (9) |
| Daemon unavailable / untrusted (non-root) peer | oneshot fallback, else `PAM_IGNORE` (25) |
| Config missing, unparseable, or untrusted (not root-owned / group- or world-writable, incl. parents) | `PAM_IGNORE` (25) |
| Timeout (structured zbus timeout or overall deadline) | `PAM_AUTH_ERR` (7) |

PAM module never blocks indefinitely. All operations have timeouts, including
D-Bus connection establishment (overall deadline on a worker thread).

The oneshot fallback spawns `facelock auth` with a sanitized environment:
`env_clear()` plus an allow-list of `SSH_CONNECTION`, `SSH_TTY`, and a pinned
`PATH=/usr/bin:/bin`. No other variables (`LD_*`, `XDG_*`, `DBUS_*`, ...) are
inherited. Stdin is `/dev/null`.

### Syslog Format

```
pam_facelock(<service>): <result> for user <username>
```

## Polkit Agent Semantics

The `facelock-polkit-agent` offers face authentication for polkit actions, but
scoped to an allowlist — face is **not** a universal key for every privileged action.

| Outcome | Agent behavior |
|---------|----------------|
| `action_id` not in `polkit.face_eligible_actions` | Declines (returns `org.freedesktop.DBus.Error.Failed`) — see fallthrough-vs-denial caveat below |
| Allowlisted action, face matches | Responds success to polkit authority |
| Allowlisted action, no match / daemon error | Declines (same caveat) |
| Username cannot be resolved to a uid | Refuses to respond; **never** sends UID 0 for an unresolved name |

**NOTE (agent model only):** polkit registers a single authentication agent per
session and does not chain agents. When this agent declines, the decline
returns an error, which — depending on the desktop's agent registration — may
present as an authorization denial rather than a fallthrough to a password
dialog. The intended UX (non-eligible actions handled by the desktop's normal
password agent) is unverified pending live-desktop testing. Behavior here is
fail-closed: a non-eligible action is never face-authorized. Does not apply to
the PAM model, which always falls through to the password prompt.

A decline never fails open to root, and never causes this agent itself to grant
authorization it should not — but see the caveat above on whether polkit
treats a decline as a fall-through to another agent or as an outright denial.

## Anti-Spoofing

| Defense | Config | Default |
|---------|--------|---------|
| IR camera enforcement | `security.require_ir` | **true** |
| Frame variance check | `security.require_frame_variance` | **true** |
| Frame variance cutoff | `security.frame_variance_max_similarity` | 0.985 |
| IR texture cutoff (raw frame) | `security.ir_texture_min_stddev` | 10.0 |
| Landmark liveness | `security.require_landmark_liveness` | **false** |
| Minimum auth frames (= variance window size) | `security.min_auth_frames` | 3 |
| Frame variance default const | `DEFAULT_FRAME_VARIANCE_MAX_SIMILARITY` | 0.985 |

IR classification is derived from queried device evidence: a node is IR when its
enumerated pixel formats are mono-only/IR-typical (GREY/Y8/Y10/Y12/Y16, with no
color format mixed in), or when a quirks `force_ir` entry matches (authoritative
by USB vendor:product ID; a name-only match requires corroborating format
evidence or a real USB identity). The free-text device name never classifies a
device on its own, and a GREY/Y16 format offered *alongside* a color format is
not treated as IR. A `force_ir` quirk is device-level ("this USB device has an
IR sensor"): when the device exposes multiple capture nodes and at least one has
an IR-typical format, only the format-bearing node(s) classify IR. A quirk's
`format_preference` participates in that decision only when the preferred
format is itself IR-typical and actually advertised; an RGB preference such as
MJPG cannot exempt an RGB sibling from demotion (see
`docs/security.md` §A). Frame variance is passive
anti-photo only (does not stop video replay); it is evaluated over a sliding window
of the most recent `min_auth_frames` matched frames (see `docs/security.md` §B), with
a 0.985 cutoff rejecting truly static input (≳0.999) with margin; the
field-measured frozen-human band is 0.98–0.995, and the default sits inside it —
a fully frozen user recovers via the sliding window as soon as they move
slightly. IR texture is measured on the raw frame, never CLAHE. These defaults
must not be weakened without security review.

## Models

| Model | File | Size | Default |
|-------|------|------|---------|
| SCRFD 2.5G | `scrfd_2.5g_bnkps.onnx` | ~3MB | Yes |
| ArcFace R50 | `w600k_r50.onnx` | ~166MB | Yes |
| SCRFD 10G | `det_10g.onnx` | ~17MB | Optional |
| ArcFace R100 | `glintr100.onnx` | ~249MB | Optional |

Configurable via `recognition.detector_model` and `recognition.embedder_model`.
Bundled model filenames are verified against the manifest hash at load time. Custom model files require matching `recognition.detector_sha256` or `recognition.embedder_sha256`.
