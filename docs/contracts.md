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
| `facelock pam add` | Add the facelock line to one or more `/etc/pam.d/<service>` files. Root |
| `facelock pam remove` | Remove it. Root |
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
`pam_facelock.so`, the service units, or the Omarchy scripts never move. See
ADR 009.

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

- **`setup --yes` keeps its combined meaning** and is the one documented
  exception to the flag split below. It maps onto *both* of the writer's knobs:
  `--no-confirm` (skip the per-file question) **and** `--allow-sensitive`
  (accept the sensitive services listed below). `--non-interactive` maps onto
  `--no-confirm` alone, as it always has.
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

`facelock pam add | remove | status` owns every write to `/etc/pam.d`.
`setup --pam` is an alias onto it (above), and the wizard's step 9 calls the
same writer, so there is one implementation of the edit and one set of rules.

**Resolution order: `/etc/pam.d`, then `/usr/lib/pam.d`. First hit wins.** That
is Linux-PAM's own precedence, and it is not academic: on current Arch `polkit`
ships its configuration as `/usr/lib/pam.d/polkit-1` and `/etc/pam.d/polkit-1`
does not exist, so a writer that looked only in `/etc/pam.d` could not
configure the service at all. The list is `[pam] config_dirs` (Config Schema
below) for a distribution whose vendor directory is somewhere else; there is no
way to ask Linux-PAM at run time which one it was compiled with, so the default
is the pair above and configuration is never required. A hit that is *refused* —
a hard link, a symlink out of its directory — is still a hit: the search does
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
the vendor file; `pam remove` takes the line out and leaves the override in
place. The vendor file is never read-modified-written, never backed up, and
never renamed over.

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

**Direct service-file editing is the Arch-family path.** Debian and Ubuntu
compose their stacks with `pam-auth-update` from profiles in
`/usr/share/pam-configs/`, and Fedora and RHEL with `authselect` (see
`dist/authselect/facelock`); on those systems a hand-inserted line is
overwritten by the tool that owns the file. Being able to resolve a vendor
directory does not make this command idiomatic there.

**Confinement.** A service name is **one path component**: not empty, no `/`,
not `.` or `..`, no interior NUL. Rejected before any I/O, on `add`, `remove`
and `status` alike. `base.join(service)` is not a confinement primitive — an
absolute name *replaces* the base — so this is the check, not the join.
Anything else is accepted: `PAM_CANDIDATES` is the wizard's menu, **not** an
allowlist, and a service that is not on it must keep working.

**Symlinks are followed only inside the directory the entry was found in.** A
well-formed name still has to survive the filesystem: on an authselect system
`/etc/pam.d/system-auth` and `/etc/pam.d/password-auth` are symlinks into
`/etc/authselect`, and read, copy and write all follow a link without being
asked to. So the entry is `lstat`ed, and a link is canonicalized: a target
**under that same directory** is followed — the real file is what gets read, rewritten and backed up, so the
`.facelock-backup` lands beside the file that changed rather than beside the
link — and a target anywhere else is a validation failure — **including a target in a
directory later on the search path.** `/etc/pam.d/polkit-1 ->
/usr/lib/pam.d/polkit-1`, wired up by hand, is therefore refused rather than
treated as a vendor hit: it is a link out of `/etc`, and following it would put
the edit in the package's own file, which is the one thing this must never do.
The fix is to delete the link and let `pam add` create a real override. So is a
link that cannot be resolved at all; the rule is "prove it stays inside", and a
dangling link proves nothing. Being a validation failure, it writes nothing for the
whole run, and `pam status` reports the service as `unknown` with the fixed
reason `symlinked outside /etc/pam.d` (the human message on stderr names the
target). That reason is a fixed name for the class, not a rendering of the
directory that was violated: an entry in a vendor directory pointing out of
*that* directory reports the same string, and the human message is what names
the real one. Editing through such a link would edit a file authselect regenerates,
so the change would disappear on the next `authselect apply-changes` with
nothing to say it had.

**A file with more than one hard link is refused.** A symlink can be followed
to somewhere and checked; a second hard link cannot — a link count says another
name for the inode exists and says nothing about where it is, so the edit
cannot be shown to stay inside the directory. `pam status` reports it as
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

**The sensitive gate is applied to the resolved file, not only to the typed
name.** A symlink `alias -> system-auth` *inside* `/etc/pam.d` is followed, so
without the second check `pam add --service alias` would be an ungated name for
a gated file — which is the shape RHEL's older `authconfig` leaves behind, and
why `system-auth-ac` and `password-auth-ac` are on the list as well.

**Two-phase.** Every requested service is validated — name, existence
(subject to `--if-present`), the sensitive gate, and what the edit would be —
before **any** file is written. A validation failure writes nothing at all,
which is what makes a caller's loop all-or-nothing for the failure that
actually happens: a typo'd or gated service name. It is **not** a transaction:
a write-phase I/O error on service N leaves 1..N-1 written. Those are reported
per service and the exit code is non-zero; the remaining services are still
attempted. The rollback is the `.facelock-backup` file written before each
edit, which nothing in this command deletes.

**`--no-confirm` never implies `--allow-sensitive`.** They are separate
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
prompts" reads the same on `pam add` as on `remove` and `clear`) and neither
unlocks the gate. `setup --yes` keeps the combined meaning and is the sole
exception. `remove` is never gated **on sensitivity** — removal can only take
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
| `pam status` | 2 | at least one is absent, unreadable, misnamed, symlinked out of the directory, or hard-linked |
| `pam status --if-present` | 0 | every requested service **that exists** carries the line |
| `pam status --if-present` | 1 | at least one existing service carries no line |
| `pam status --if-present` | 2 | as above, minus the absent case, which no longer forces 2 |
| `pam status --all` | 0 | at least one service carries the line, and every directory was read |
| `pam status --all` | 1 | nothing on the machine carries it, or an enumerated service has no line in the file Linux-PAM reads |
| `pam status --all` | 2 | a directory could not be listed, or an enumerated service could not be answered for |
| `pam status --all --if-present` | 0/1/2 | unchanged from `--all`: an enumerated name was found, so there is no absent case to forgive |
| `pam add`, `pam remove` | 0 | every service reached its requested state — including `unchanged`, `overridden` (`add` created the `/etc/pam.d` copy), `vendor-only` (`remove` had nothing of its own to take out of a package-owned file), `absent` under `--if-present`, and `declined` |
| `pam add`, `pam remove` | non-zero | a validation failure (nothing written) or a write failure |

`pam status` is on `grep`'s scale and `is-enrolled`'s: a boolean query whose
exit code is the answer. Across several services the worst outcome wins. A
**declined** confirmation is exit 0, since the command did what the operator
asked, and `--json` is how a script tells it from an install.

**`--dry-run`** prints the resolved plan, writes nothing, and exits 0. It is
honoured *after* the root check (see DEC-6 above).

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
- an entry the resolver refuses (symlinked out of its directory, hard-linked)
  is an `unknown` row with its usual reason: never followed, never dropped.
- a service file that could not be **read** is an `unknown` row too. Omitting it
  would report "not configured" for a machine this could not check.
- `.facelock-backup`, `.pacnew`, `.pacsave`, `.pacorig`, `.rpmnew`, `.rpmsave`,
  `.rpmorig`, `.dpkg-old`, `.dpkg-new`, `.dpkg-dist`, names ending in `~`, and
  dotfiles are not services. Each can carry the line, and none is a name
  Linux-PAM is ever asked for.
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

**Every write is atomic.** A temp file in the destination's own directory,
`fsync`, then a rename — so a reader sees the old file or the new one and never
a short one, which matters because a truncated `/etc/pam.d/polkit-1` breaks
polkit auth machine-wide and a truncated `system-auth` breaks the machine. The
new file carries the mode and owner of the file it replaces (of the vendor
original, for a copy) and the SELinux context of the file being replaced; a
copy has no context to inherit and takes the destination directory's, which is
what SELinux's own type transition would have given it. **POSIX ACLs and every
xattr other than the SELinux label are not carried across** — a `setfacl`'d
service file loses its ACL on the first `pam add`, which is written down rather
than guessed at. A failed write removes its temp file, so a refusal leaves no
debris in `/etc/pam.d`.

**The `.facelock-backup` is written the same way**, and that is what makes it
safe: a rename replaces the *name*, so a symlink standing at
`<service>.facelock-backup` is replaced rather than followed — the confinement
rules cover the service file, and nothing covered its backup while the backup
was a `copy`. It also means the file `add` tells the operator to restore from
cannot be a short one.

**Limit.** One, deliberate and not hidden: `remove` takes no backup of its own.
It relies on the one `add` wrote, which is why nothing in this command ever
deletes a backup.

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
      "backup": "/etc/pam.d/sudo.facelock-backup"
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
object; `error` is present when `action` is `failed` or `unknown`, and
`shadows` when the file the row names hides a vendor one. **`error` is a
diagnostic, not a contract** — branch on `action`, never on `error`'s text. A
rejected service name reports the fixed C-locale string `invalid service name`,
a service symlinked out of the directory it was found in
`symlinked outside /etc/pam.d` — a fixed name for the class, whichever
directory it was — and a
hard-linked one `hard-linked service file`, but the OS-level failures (`failed` on a write, `unknown` on an unreadable
file) interpolate a `strerror` string, which follows the operator's
`LC_MESSAGES` like any other C library message. Nothing else in a `--json`
document is locale-dependent. `backup`
is the `.facelock-backup` path when one exists on disk after the operation and
`null` otherwise — always `null` under `--dry-run`, which writes none, and
always `null` for an `overridden` service: the copy preserved nothing, so it
took no backup, and a `.facelock-backup` left at the override path by an
earlier run is not this run's rollback and is not reported as one. Deleting the
override is its undo, and the vendor original is untouched.
`path` on an `overridden` row is the **override that was created**, not the
vendor file it was read from; on a `vendor-only` row it is the vendor file,
which is the one that exists. `path`
is itself `null` when `action` is `unknown` because the *name* was rejected: no
path was ever resolved, and reporting `/etc/pam.d/../escape` named a path
nothing went near, which reads as one that was acted on. A service rejected for
being a symlink out of the directory does carry a `path` — the link, which is a
real entry this did `lstat` — and its `backup` field is probed, since a
facelock version that wrote through the link left one there.

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
| `unknown` | `status` | the file could not be read, the name was rejected, or the entry is a symlink out of the directory or a hard link; see `error` |

`pam status --json` is what replaces `grep -q pam_facelock.so
/etc/pam.d/<service>` in an integration script: it answers from the same file,
without root, and reports "absent" and "unreadable" as themselves rather than
as "not configured".

**Repeatable `--service`.** `--service a --service b` acts on both in one
process, one root check and one closing hint. Duplicates collapse. No
`--service` means `sudo`, which is what bare `setup --pam` has always meant.

**Service-file edits are byte-preserving.** A backslash followed only by spaces
or tabs before LF or CRLF continues the same logical PAM rule, so insertion
never splits that rule. A `#` ends the semantic rule even after a continuation;
comment and blank physical lines remain untouched. The line goes above the
first logical rule whose first ASCII-whitespace-delimited type token is `auth`,
matched ASCII-case-insensitively and with Linux-PAM's optional leading `-`;
`authtok_type=` is not an auth type. When there is no auth rule, the line goes
directly after a leading `#%PAM-1.0` header, or at the top when that header is
absent.

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
no new backup, and vendor-override creation has no original at the override
path to preserve, as documented above. Golden fixtures pin insertion, removal,
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
| `pam-status` | `pam status` exists — the unprivileged `/etc/pam.d` read (DEC-6 below) |
| `pam-status-all` | `pam status --all` exists, and conflicts with `--service` — the enumerating form, which answers "what is configured on this machine?" rather than "is this name configured?" |
| `quiet` | the global `--quiet` |
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
Fedora 43 and Fedora 44 supply full lifecycle evidence; Fedora 45 supplies
required build/runtime smoke. Rawhide cannot supply lifecycle, artifact,
upgrade, rollback, served-version, or availability evidence; it is limited to
best-effort pinned Track D smoke only. A Rawhide-only failure is not
alpha-blocking. Promotion requires a separately reviewed amendment and full
Fedora gates.

Issue #236 owns pre-tag and post-publication proof that optional Rawhide serves
no alpha or candidate build. This contract does not provision, publish to, or
otherwise mutate COPR or Packit infrastructure.

The public APT base is `https://tysmith.me/facelock/apt/`. Its stable suite
paths and payload identities are:

| Suite | Public Release path | Architecture | Variant |
|-------|---------------------|--------------|---------|
| `trixie` | `https://tysmith.me/facelock/apt/dists/trixie/Release` | amd64 | TPM |
| `bookworm` | `https://tysmith.me/facelock/apt/dists/bookworm/Release` | amd64 | legacy |
| `resolute` | `https://tysmith.me/facelock/apt/dists/resolute/Release` | amd64 | TPM |
| `noble` | `https://tysmith.me/facelock/apt/dists/noble/Release` | amd64 | legacy |

The former `main` and `legacy` suite names are retired; they are not aliases or
redirects. Existing source entries must replace that suite component with the
host operating-system codename while keeping the `facelock` component. Each
stable publication consumes exactly one matching package for all four suites,
and a prerelease or cross-suite version is rejected before signing or repository
writes.

## Filesystem Paths

| Path | Owner | Mode | Purpose |
|------|-------|------|---------|
| `/etc/facelock/config.toml` | root:root | 644 | Configuration |
| `/var/lib/facelock/` | root:root | 711 | State dir. Traversable by every local user, listable by root only: anyone can open a path it knows by name (its own enrollment marker, a model file) but nobody can enumerate what is there (`models/` is itself `0755` and listable — public data) |
| `/var/lib/facelock/facelock.db` | root:root | 600 | Face embeddings. Read by the daemon (root) only; user-run PAM stacks request authentication through the daemon, they never read templates |
| `/var/lib/facelock/models/` | root:root | 755 | ONNX models — public, SHA256-verified downloads |
| `/var/lib/facelock/enrolled/` | root:root | 711 | Enrollment markers; traversable by all, listable by none |
| `/var/lib/facelock/enrolled/<user>` | \<user\>:\<user\> | 600 | `{"models": N, "updated": "<ISO8601>"}` — a hint for `is-enrolled`, never authoritative |
| `/var/log/facelock/` | root:root | 700 | Log dir — per-user auth history and raw face snapshots are root-only |
| `/var/log/facelock/audit.jsonl` | root:root | 600 | Structured audit log |
| `/var/log/facelock/snapshots/` | root:root | 700 | Auth snapshots (raw face images) |
| `/usr/bin/facelock` | root:root | 755 | CLI binary |
| `/lib/security/pam_facelock.so` | root:root | 755 | PAM module |

All paths overridable via config. `FACELOCK_CONFIG` is honored for unprivileged processes, but privileged PAM/root auth flows ignore the environment and use either an explicit `--config` path or `/etc/facelock/config.toml`.
Runtime-created DB sidecars (`-wal`, `-shm`), audit logs, and snapshots are created with explicit restrictive modes. The packaged systemd unit also sets `UMask=0027`.

#### Traversal for everyone, listing for nobody (ADR 010)

The state directory and `enrolled/` are `0711 root:root`: any local user may
*enter* them, nobody but root may *list* them. That is the whole grant. Every
entry below is locked down in its own right — `0600 root:root` database and
sidecars, `0600 <user>:<user>` markers — and `models/` is the one subtree that
carries "other" read bits of its own, because its contents are public,
SHA256-verified downloads. There is no group in the file contract: nothing
under `/var/lib/facelock` is group-owned or group-readable (ADR 010).

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

`[pam].config_dirs` is where `facelock pam add | remove | status` looks for PAM
service files, in search order — Linux-PAM's own precedence, earliest wins.
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
