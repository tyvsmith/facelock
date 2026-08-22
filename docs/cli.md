# CLI Reference

All commands are subcommands of the `facelock` binary.

## Global flags

The following flags are accepted by every subcommand (declared `global = true`):

| Flag | Description |
|------|-------------|
| `-c`, `--config <PATH>` | Override the config file path. Takes precedence over `FACELOCK_CONFIG`. |
| `-q`, `--quiet` | Suppress stdout: informational text, and on commands whose stdout is the payload, the payload too. Errors (stderr), prompts and exit codes are unaffected. |
| `-v`, `--verbose` | Raise diagnostic verbosity on stderr, one level per repeat. The CLI starts at `warn`, `daemon run` at `info`. `RUST_LOG` overrides it. |

Diagnostics default to `warn`, so a command prints warnings and errors on
stderr and nothing quieter. The setup wizard's questions and the `status`
report are readable again, rather than interleaved with timestamped log lines.
`-v` raises the level one step per repeat; `facelock daemon run` keeps `info`,
because it writes to the journal, where nothing competes with it. `RUST_LOG`
outranks both, and the level changes output volume only: exit codes and stdout
payloads are identical at every level.

`--quiet` and `-v` are separate knobs on separate streams, so `--quiet -v` is a
real combination (silent report, loud diagnostics) rather than a contradiction.

`--quiet` is complete for every command whose output goes through the message
seam: `setup`, `enroll`, `test`, `remove`, `clear`, `is-enrolled`,
`capabilities`, `pam`, and the `--json` payloads of `list`, `devices` and
`status`. Seven still write human text straight to stdout and stay noisy under
it until [#140](https://github.com/tyvsmith/facelock/issues/140) is finished:
`status`, `bench`, `tpm` (every verb, `encrypt`/`decrypt`/`reseal` included),
`config`, `daemon restart`, `hyprlock` and `audit`, as do the human tables of
`list` and `devices` — so `status --json --quiet` is silent while a bare
`status --quiet` is not. `preview --json` is on neither list: its frame stream
is stdout by design and `--quiet` is documented not to reach it.

## Machine-readable output

Every command whose output a script would parse takes `--json`, and spells it
exactly that — one flag family, no short letter, no `--output json`. It is not
offered everywhere: a command gains it when it has a named consumer, which
today means `facelock is-enrolled`, `facelock capabilities`, `facelock list`,
`facelock devices`, `facelock preview`, `facelock status`, `facelock pam add`,
`facelock pam remove` and `facelock pam status`. Each payload is described in
that command's section below; the rule behind the flag, and the promise each
payload carries, are in [`contracts.md`](contracts.md) under "CLI Machine
Output".

The payload goes to stdout and nothing else does — diagnostics are on stderr
whatever `RUST_LOG` says — so `facelock devices --json` is safe to pipe at any
log level. `--quiet` suppresses the payload on every one of these except
`preview`, whose frame stream runs until interrupted and would otherwise become
a command that prints nothing forever. What that leaves behind depends on the
command: where the exit code is the answer it is the whole answer, but `status`
exits 0 whenever it produced a report, so `status --quiet --json` leaves nothing
at all.
**This changed:** `list --json --quiet` and `devices --json --quiet` used to
print their payload and now print nothing; the exit code is unchanged.

## facelock setup

Interactive setup wizard. Walks through camera selection, model quality,
inference device (CPU / CUDA / ROCm / OpenVINO), model downloads, encryption,
the daemon, enrollment and PAM configuration. Every step can also be answered,
or declined, from the command line.

The daemon is configured before enrollment on purpose. `enroll` and `test`
select their transport once, when they start, so on a first install a daemon
configured after them would never be the one they used: enrollment would fall
back to direct camera access and the recognition test would validate a
transport no later authentication takes. The step starts the daemon, or
restarts it if one is already running, because the daemon reads the encryption
method, the model preset and the inference device once at startup: on a re-run
of `setup` an untouched daemon would hold the answers from before the wizard.
A restart interrupts any authentication that daemon is mid-way through, so a
`sudo` prompt waiting on a face in another terminal falls back to a password
that once.

```bash
facelock setup                          # interactive wizard
facelock setup --non-interactive        # base setup, no prompts, no PAM/systemd/enroll
facelock setup --systemd                # install systemd units
facelock setup --systemd --disable      # disable systemd units
facelock setup --pam                    # install to /etc/pam.d/sudo
facelock setup --pam --service polkit-1 # install to a specific service
facelock setup --pam --remove           # remove the PAM line
facelock setup --pam --service hyprlock --if-present  # a missing service file is success
facelock setup --pam --remove --if-present  # ...on removal too
facelock setup --pam --service sshd -y --allow-sensitive  # suppress the prompt and authorize the sensitive write
facelock setup --no-pam                 # wizard, but never touch /etc/pam.d
facelock setup --camera /dev/video2     # answer step 1 from the command line
```

Three rules generate the whole flag list. Supplying a value answers that
question and therefore replaces its prompt, which is why there is no
`--skip-<x>-prompt` family. A `--no-<action>` flag declines an action outright;
declining is not defaulting. And `auto` means re-derive from the hardware,
since omitting a flag already gives the default.

### Modes

| Flag | Meaning |
|------|---------|
| *(none)* | Full interactive wizard. Falls back to the non-interactive flow when stdin is not a terminal. |
| `--non-interactive` | No prompts. Choices resolve to config-or-default. Runs the base setup only: directories, model download and verification, encryption, path permissions. No PAM, no systemd, no enrollment unless asked for explicitly. |
| `-y`, `--yes` (alias `--no-confirm`) | Suppress ordinary confirmation prompts. Does not authorize a sensitive PAM edit. |

`--yes` and `--non-interactive` suppress the per-file "Proceed?" confirmation;
neither unlocks the sensitive-service gate. The shared auth stacks
`common-auth`, `password-auth`, `password-auth-ac`, `system-auth`,
`system-auth-ac` and `system-login`, plus `login` and `sshd`, require
`--allow-sensitive`. Thus even `facelock setup --pam --service sshd --yes`
refuses. Locking yourself out of a machine takes two independent decisions:
whether to skip the prompt, and whether to authorize the sensitive write.

### Choice flags

Precedence for all four: **CLI flag > config file > built-in default.**
Supplying the flag suppresses the corresponding wizard step and writes the
value back to `/etc/facelock/config.toml`. A value that cannot be honoured is
fatal, never a silent fallback: `--camera /dev/video9` on a machine without
that node aborts, and `--encryption tpm` with no usable TPM aborts rather than
quietly writing a software keyfile.

| Flag | Values | What `auto` does | Wizard step |
|------|--------|------------------|-------------|
| `--camera <PATH\|auto>` | a `/dev/video*` path, or `auto` | Re-classifies the attached devices and picks the single IR-capable node that advertises a format Facelock can decode. Zero usable IR devices and more than one are both errors that list what was found; an IR node excluded for formats such as Y8/Y10/Y12 is reported with its path and formats. | 1 |
| `--models <standard\|balanced\|high>` | three presets | *no `auto`*: quality is a preference, not something the machine can report | 2 |
| `--execution-provider <cpu\|cuda\|rocm\|openvino\|auto>` | provider name | Asks the installed ONNX Runtime which providers it was built with and takes the best, in the order cuda > rocm > openvino > cpu. Availability is a property of the runtime build, not of the hardware, and the choice is always printed. | 3 |
| `--encryption <tpm\|keyfile\|none\|auto>` | method | Uses the TPM when a working TPM 2.0 is present, otherwise a software keyfile. | 5 |

When `security.require_ir = true` and the wizard detects IR-classified nodes but
all of them advertise only unsupported formats, camera selection is a fatal
refusal. It lists every excluded IR path and format and does not present or
default to an attached RGB camera. With `require_ir = false`, decodable RGB
cameras remain available as explicit wizard choices.

Model presets:

| Preset | Detector | Embedder |
|--------|----------|----------|
| `standard` | `scrfd_2.5g_bnkps.onnx` | `w600k_r50.onnx` |
| `balanced` | `scrfd_2.5g_bnkps.onnx` | `glintr100.onnx` |
| `high` | `det_10g.onnx` | `glintr100.onnx` |

### Action flags

| Pair | Without either flag (wizard) | Without either flag (`--non-interactive`) |
|------|------------------------------|-------------------------------------------|
| `--pam` / `--no-pam` | prompt (step 9) | off |
| `--systemd` / `--no-systemd` | prompt (step 6) | off |
| `--enroll` / `--no-enroll` | prompt (step 7) | off; enrollment needs a human in front of the camera |

Each pair is a clap override pair, so **a later flag wins over an earlier
one**: `--pam --no-pam` declines PAM, `--no-pam --pam` installs it. That
matters when a wrapper appends an override to a command line it did not
construct. `--no-pam` means nothing under `/etc/pam.d` is read, backed up or
written; it is not "use the PAM default".

`--pam` inside the wizard configures **exactly one service**, `--service`
defaulting to `sudo`, and does not apply the multi-select's pre-checked
candidates. `--enroll` answers the "enroll a face now?" confirmation as well as
forcing the step, so it runs unattended.

### Action modifiers

| Flag | Requires | Meaning |
|------|----------|---------|
| `--service <NAME>` | `--pam` | Target PAM service. Default `sudo`. |
| `--remove` | `--pam` | Remove the facelock PAM line instead of adding it. |
| `--if-present` | `--pam` | Treat an absent service file as success rather than an error, on the add side as well as `--remove`. Read, parse and write failures stay fatal. Without it, a service that is not there is a hard error. |
| `--allow-sensitive` | `--pam` add | Explicitly authorize adding Facelock to `common-auth`, `login`, `password-auth`, `password-auth-ac`, `sshd`, `system-auth`, `system-auth-ac`, or `system-login`. Does not suppress the confirmation prompt and conflicts with `--remove`. |
| `--disable` | `--systemd` | Disable and stop the units instead of installing them. |

The parser enforces these, so `facelock setup --remove` is an error naming the
missing `--pam` rather than a silently ignored flag.

### How setup flags compose

`--pam` and/or `--systemd` **on their own** perform just that action and touch
nothing else. Any flag that only makes sense while the base setup runs —
`--non-interactive`, a choice flag, or any of `--no-pam` / `--no-systemd` /
`--enroll` / `--no-enroll` — forces the base setup, and the requested actions
run **in addition**. `-y` on its own does not force it, so `facelock setup -y
--pam` is still PAM-only. When both run the order is base setup, then systemd,
then PAM.

`--pam` is an alias onto [`facelock pam add | remove`](#facelock-pam), which is
the primary spelling and the one that takes several services in one process.
Existing `setup --pam` invocations keep parsing. Sensitive additions now use
the same explicit `--allow-sensitive` authorization as `facelock pam add`,
while `-y` only suppresses the prompt.

Eight services are gated: the shared auth stacks `common-auth`,
`password-auth`, `password-auth-ac`, `system-auth`, `system-auth-ac` and
`system-login`, plus `login` and `sshd`.
`facelock setup --pam --service login` refuses until `--allow-sensitive` is
added, even when `-y` is present.

Every `setup` run reconciles the per-user enrollment markers behind
[`facelock is-enrolled`](#facelock-is-enrolled) against the database, which is
what backfills users who enrolled before markers existed.

## facelock is-enrolled

Report whether a user has a usable face enrollment. Unprivileged and cheap
enough to call repeatedly from a lock screen: it reads one marker file under
`/var/lib/facelock/enrolled/` and never activates the daemon, opens a camera,
or reads the face database.

No group is involved (ADR 010): the marker sits under two `0711 root:root`
directories, so any local user can open its own marker by name. A missing or
unreadable marker (`ENOENT` or `EACCES`) is reported `not-enrolled` rather than
as an error.

```bash
facelock is-enrolled                    # prints enrolled / not-enrolled
facelock is-enrolled --user alice       # specific user
facelock is-enrolled --json             # machine-readable
facelock is-enrolled --quiet            # no stdout; the exit code is the answer
```

The exit code is the contract — branch on it rather than parsing stdout:

| Code | Meaning |
|------|---------|
| 0 | the user has a usable enrollment |
| 1 | not enrolled; an absent or unreadable marker reports this way |
| 2 | error — an invalid `--user`, or a marker that exists but cannot be parsed |

`--json` emits one object and does not change the exit code:

```json
{"enrolled":true,"models":2,"updated":"2026-08-12T00:00:00Z"}
```

`models` is `0` and `updated` is `null` when the user is not enrolled. The
error case prints its reason on stderr and no payload at all.

The marker is a hint for deciding whether to offer a face-auth affordance; PAM
at authentication time remains authoritative and nothing in the auth path
consults it. See [`contracts.md`](contracts.md), "facelock is-enrolled Exit
Codes", for the stability promise and for how markers are reconciled with the
database.

## facelock capabilities

Report what this build can do, as capability names. Unprivileged: it answers
from the binary's own clap tree and compiled-in constants, reading no config
file, activating no daemon and opening no camera. It is what replaces grepping
`--help` in a wrapper script.

```bash
facelock capabilities                   # one name per line
facelock capabilities --json            # {"version", "capabilities"}
```

With the name array elided:

```json
{"capabilities":["capabilities","devices-json","is-enrolled"],"version":"0.1.4"}
```

Both forms exit 0 — the command has no failure mode — and `--quiet` suppresses
stdout, leaving the exit code as the whole answer. A build that predates the
command answers by failing: clap's unrecognized-subcommand error on stderr,
exit 2, nothing on stdout. A caller reads any non-zero exit as "no capabilities
at all", which is the true answer for that build.

Probe by name, never by version. The names this build emits, what each one
promises, and the stability rules that govern them are in
[`contracts.md`](contracts.md), "facelock capabilities".

## facelock enroll

Capture and store a face model.

```bash
facelock enroll                         # current user, auto-label
facelock enroll --user alice            # specific user
facelock enroll --label "office"        # specific label
facelock enroll --skip-setup-check      # enroll on a tree setup never marked complete
```

Captures 3-10 frames over ~15 seconds. Requires exactly one face per frame. Re-enrolling with the same label replaces the previous model.

Without `--skip-setup-check`, an install whose setup-complete marker is missing
is offered `facelock setup` first and enrolls through it, since setup enrolls a
face itself. `--skip-setup-check` goes straight to the capture loop. It is for a
tree assembled by hand or by a configuration manager, where the marker was never
written but the models, database and encryption key are all in place; enrollment
still fails on its own terms if any of them is not.

## facelock test

Test face recognition against enrolled models.

```bash
facelock test                           # current user
facelock test --user alice              # specific user
```

Reports match similarity and latency.

## facelock list

List enrolled face models.

```bash
facelock list                           # current user
facelock list --user alice              # specific user
facelock list --json                    # JSON output
```

`--json` emits an array of objects:

```json
[
  {
    "id": 1,
    "label": "office",
    "user": "alice",
    "created_at": 1700000000,
    "embedder_model": "arcface_r50"
  }
]
```

## facelock remove

Remove a specific face model by ID.

```bash
facelock remove 3                       # remove model #3
facelock remove 3 --user alice          # for specific user
facelock remove 3 --yes                 # skip confirmation
```

## facelock clear

Remove all face models for a user.

```bash
facelock clear                          # current user
facelock clear --user alice --yes       # skip confirmation
```

## facelock preview

Live camera preview with face detection overlay.

```bash
facelock preview                        # Wayland graphical window
facelock preview --json                 # one JSON object per frame on stdout
facelock preview --user alice           # match against specific user
```

`--json` shipped as `--text-only`, which stays a hidden alias and keeps
parsing; the payload is unchanged. One object per line, one per frame:

```json
{"faces":[{"confidence":0.5,"height":180.0,"recognized":true,"similarity":0.75,"width":180.0,"x":112.0,"y":88.0}],"fps":15.0,"frame":1,"height":480,"jpeg_size":24576,"recognized":1,"unrecognized":0,"width":640}
```

Keys come out sorted, which is `serde_json`'s doing and not a promise.
`jpeg_size` is present only when the daemon serves the frames; the direct
(oneshot) path has no JPEG and omits that key, and every other key is on both.
Numbers are `f32` rounded then widened to `f64`, so a rounded `0.988` reaches
you as `0.9879999756813049`: compare numerically, never as text.

## facelock devices

List available V4L2 video capture devices.

```bash
facelock devices                        # human-readable listing
facelock devices --json                 # JSON output
```

Shows device path, name, driver, formats, resolutions, and IR status.

`--json` emits an array of device objects with `path`, `name`, `driver`,
`is_ir`, and `formats`; each format carries `fourcc`, `description`, and
`sizes`, a list of `[width, height]` pairs. It is a typed schema derived from
the device struct, so a script reads it rather than parsing the listing above,
whose columns, indentation and `[IR]` tag are free to change.

`formats` is empty whenever the daemon answers: the D-Bus device type does not
carry format detail, so only the direct (oneshot) backend fills it in. The
human listing omits the section for the same reason. Read `formats` for
capability detection only when you know you are on the direct path.

## facelock status

Check system status — config, daemon, oneshot fallback, camera, models,
encryption, enrollment, security posture, notifications, PAM wiring. Requires
root. A check that cannot be performed (unreadable database, broken config) is
reported as "cannot determine" — never as a guessed value.

```bash
facelock status
facelock status --json
facelock status --json | jq -e '.daemon.reachability == "responding"'
```

`--json` prints one object with a key per section of the report — `config`,
`daemon`, `oneshot_fallback`, `camera`, `models`, `execution_provider`,
`encryption`, `enrollment`, `security`, `notifications`, `pam` — each carrying
a `state` of `ok`, `problem` or `unknown` and, when it is not `ok`, a `reason`.
It is the same value the report is rendered from, and a test walks both outputs
of one fixture, so a section cannot answer differently in the two. This is the
form to branch on: the third line above is what replaces grepping the report
for `[ok] responding`. A fact nobody established is `"state": "unknown"` with a
reason and no value — never a `null` and never a `false`, so read a section's
`state` before any field beside it: on an unreadable database `enrollment`
carries no `models` key at all, and `(.enrollment.models // [])` would answer
"not enrolled" for a machine nobody could check.

**Two sections keep a narrower question than their name suggests**, and both
have the specific answer nested one level down. Under auto-detection
`.camera.state` reports only that detection is enabled, so it reads `ok` on a
machine with no camera at all — `.camera.device.state` is the hardware fact.
And `.pam.state` reports that `pam_facelock.so` is installed, not that anything
uses it — `.pam.services` is the scan. The full schema, the per-section table
of what each `state` answers, and the stability tier are in
[`contracts.md`](contracts.md) under "facelock status Semantics".

Exit codes do not change under `--json`: `status` exits 0 whenever it produced
a report, and the verdicts are in the document. `--quiet` therefore suppresses
the payload and leaves nothing behind, which makes `--quiet --json` a no-op
rather than a terser query.

The `PAM services:` line lists every service that carries the facelock line,
from the same scan [`facelock pam status --all`](#facelock-pam-status---all)
runs, and marks how many are a local override of a vendor file. It reads `none
configured` only when every directory was read; when one could not be, it reads
`not checked` and names the place on a line of its own, because "nothing is
configured" and "I could not look" are different answers.

## facelock config

Show or edit the configuration file. Bare `facelock config` is
`facelock config show`.

### facelock config show

Print the config file path and its contents, then report whether it parses.
Unprivileged — it reads a `0644` file.

```bash
facelock config                         # show config path and contents
facelock config show                    # the same, spelled out
```

### facelock config edit

Open the config file in `$EDITOR` (then `$VISUAL`, then `nano`/`vi`/`vim`),
validate it on save, and restart the daemon when a setting it caches at startup
changed. Requires root.

```bash
sudo facelock config edit
```

## facelock daemon

Run or restart the persistent authentication daemon. Bare `facelock daemon` is
`facelock daemon run`, which is the form every shipped service unit invokes.

### facelock daemon run

Run the daemon in the foreground. Requires root — it opens the camera and the
face database. Normally managed by systemd, not run manually.

```bash
sudo facelock daemon                         # use default config
sudo facelock daemon run                     # the same, spelled out
sudo facelock daemon -c /path/to/config.toml # short alias for --config
sudo facelock daemon --config /path/to/config.toml
```

### facelock daemon restart

Restart the persistent daemon. On systemd systems, runs `systemctl restart
facelock-daemon.service`. Otherwise, sends a D-Bus shutdown request and the
daemon restarts on next use via D-Bus activation.

Requires root. If run interactively as a non-root user, the CLI prompts to
re-run via `sudo`.

```bash
sudo facelock daemon restart
```

## facelock auth

One-shot authentication. Used by the PAM module in oneshot mode.

```bash
facelock auth --user alice              # authenticate
facelock auth --user alice --config /etc/facelock/config.toml
```

Exit codes: 0 = matched, 1 = no match, 2 = error.

## facelock tpm

Everything that manages the embedding encryption key: the TPM device that can
seal it, and the key material itself. `encrypt`, `decrypt` and `reseal` live
here because the group owns the key's lifecycle — `encrypt` and `decrypt` run
software AES-256-GCM with no TPM involved.

### facelock tpm status

Report TPM availability and configuration.

```bash
sudo facelock tpm status
```

### facelock tpm seal-key

Seal the AES encryption key with the TPM, migrating from a plaintext keyfile to TPM-backed storage.

```bash
sudo facelock tpm seal-key
```

### facelock tpm unseal-key

Unseal the AES key from the TPM back to a plaintext keyfile, migrating from TPM-backed to keyfile storage.

```bash
sudo facelock tpm unseal-key
```

### facelock tpm unseal-check

Read-only check that the sealed AES key still unseals under the current PCR
values. Writes nothing, and exits non-zero when it does not — which is the
signal to run [`facelock tpm reseal`](#facelock-tpm-reseal).

```bash
sudo facelock tpm unseal-check
```

### facelock tpm pcr-baseline

Display the current PCR values for all configured PCR indices.

```bash
sudo facelock tpm pcr-baseline
```

### facelock tpm encrypt

Encrypt all unencrypted embeddings in the database with AES-256-GCM. The cipher
is software either way; `encryption.method` decides only where the key lives.

```bash
sudo facelock tpm encrypt                 # encrypt using the configured key
sudo facelock tpm encrypt --generate-key  # generate a new key file (or seal a new TPM key) WITHOUT re-encrypting embeddings
```

`--generate-key` only creates the key material. Run `facelock tpm encrypt`
(without the flag) afterwards to encrypt the embeddings.

### facelock tpm decrypt

Decrypt all software-encrypted embeddings in the database (reverting
AES-256-GCM encryption).

```bash
sudo facelock tpm decrypt
```

### facelock tpm reseal

Re-seal the TPM AES key under the current PCR values. This is the recovery step
after a firmware or kernel change moves a measured PCR and the sealed key stops
unsealing. Requires root, and applies only when `encryption.method = "tpm"` —
under any other method it errors rather than quietly doing nothing.

```bash
sudo facelock tpm reseal
```

It prefers unsealing the existing blob, which still works while the PCR policy
is satisfied, so it is safe to run proactively before a firmware update; once
the PCRs have moved it falls back to the plaintext key backup. With neither
available there is nothing to re-seal and it fails. Run
`facelock tpm unseal-check` to find out which of those you are in.

## facelock bench

Benchmark and calibration tools.

```bash
facelock bench cold-auth                # cold start authentication latency (model load + first auth)
facelock bench warm-auth                # warm authentication latency (pre-loaded models, 10 iterations)
facelock bench preview                  # frame capture + face detection latency
facelock bench enrollment               # time to capture and embed snapshots (dry run, embeddings not stored)
facelock bench model-load               # ONNX model load time (SCRFD + ArcFace)
facelock bench calibrate                # sweep FAR/FRR thresholds and recommend optimal value
facelock bench camera-reopen            # cost of reopening the camera: open / STREAMON / warmup split
facelock bench report                   # full benchmark report
```

**Every `bench` subcommand requires root** (DEC-6): direct-mode access needs the
`0600` root:root database whatever the subcommand, and the auth benchmarks may
need TPM access besides. `cold-auth`, `warm-auth`, `calibrate`, and `report`
additionally require enrolled faces.

`camera-reopen` needs no enrolled face and loads no models — but is root like
the rest: it closes and reopens the camera `--iterations` times (default 5) and
reports the per-phase median. That total is what `device.camera_release_secs`
trades LED-on time against — holding the stream warm after a failed attempt
buys a retry exactly this much (ADR 008).

## facelock pam

Manage the facelock line in `/etc/pam.d` service files. This command owns every
write to `/etc/pam.d`; `setup --pam` is an alias onto it, and the setup wizard
calls the same writer.

`--service` is repeatable on all three verbs and defaults to `sudo`, so several
services are configured in one process, under one root check. `add` and
`remove` require root and never offer to re-exec under `sudo`; `status` reads
only and needs no root.

A service name is looked up in `/etc/pam.d` first and `/usr/lib/pam.d` second —
Linux-PAM's own order, first hit wins — because packages ship their
configuration there: on current Arch `polkit` installs `/usr/lib/pam.d/polkit-1`
and there is no `/etc/pam.d/polkit-1` at all. Only `/etc/pam.d` is ever written
to. A service that exists only in a vendor directory is copied there first, with
the facelock line already in it and a two-line header saying what it was forked
from; the package's own file is left byte for byte. That copy reports
`overridden` rather than `installed`, and `pam status` reports a service with no
local copy as `vendor-only` rather than as `missing`. Deleting the override
restores the vendor file. Named `pam remove` does that automatically only while
the two-line Facelock header, the bytes below it after removing the module rule,
and the file owner/mode still match the first existing vendor service in the
configured search order. If either copy
has drifted, it removes the module rule but keeps the local override and says
why. If no current vendor source exists, an exact header naming a normalized
configured candidate is reported as absent and the local override is retained;
an arbitrary header path is not trusted or opened. Set `[pam] config_dirs` if your distribution's vendor directory is
somewhere else for explicit `add`, named `remove`, and test resolution.
Machine-wide `pam remove --all` deliberately ignores that setting, scans the
compiled system roots `/etc/pam.d` and `/usr/lib/pam.d`, and separately scans
the fixed detection-only generated root `/etc/authselect`.

### facelock pam add

```bash
sudo facelock pam add                                        # /etc/pam.d/sudo
sudo facelock pam add --service polkit-1 --service hyprlock  # several at once
sudo facelock pam add --service sshd --allow-sensitive       # unlock a gated service
sudo facelock pam add --service hyprlock --if-present        # a missing file is success
sudo facelock pam add --service sudo --dry-run               # print the plan, write nothing
sudo facelock pam add --service sudo --json                  # machine-readable result
```

| Flag | Meaning |
|------|---------|
| `--service <NAME>` | service to act on; repeat for several (default: `sudo`) |
| `-y`, `--yes` (alias `--no-confirm`) | skip the per-file confirmation, and nothing else |
| `--allow-sensitive` | also permit the gated services `common-auth`, `login`, `password-auth`, `password-auth-ac`, `sshd`, `system-auth`, `system-auth-ac`, `system-login` |
| `--if-present` | treat a missing service file as success instead of an error |
| `--dry-run` | print the resolved plan, write nothing, exit 0 |
| `--json` | emit one JSON document instead of human text (implies `--no-confirm`) |

`--yes` never implies `--allow-sensitive`: they are separate authorizations,
"do not ask me" and "yes, edit `system-auth`". Every service is validated before
any file is written, so a rejected service name leaves the rest untouched.
The confirmation is skipped as if `--yes` were given whenever it could not be
answered — no TTY on stdin, no TTY on stderr (where the prompt is drawn, so
`2>install.log` counts), or `--json` — and the gate is decided before any
prompt exists, so an unattended `pam add --service system-auth` still refuses.

Any symlinked service file is refused rather than written through: on an
authselect system `system-auth` and `password-auth` link into generated state,
and even an in-directory link would make a recorded service name resolve to a
different file. A file with more than one hard link is refused too: a link
count says another name exists and not where, so the edit cannot be shown to
stay in the directory.

Before an in-place edit, `add` writes a `0600 root:root` backup under
`/var/lib/facelock/pam-backups/<service>.<timestamp>` and an adjacent versioned
JSON provenance record. The record stores a confined service name, backup
basename, positive monotonic sequence, hashes, and prepared/committed state;
it never stores a target path. Only the exact
`<service>.<seconds>-<nine-digit-nanoseconds>` basename grammar is recognized.
This also moves the human and JSON `backup` value from the former adjacent
`/etc/pam.d/<service>.facelock-backup` location to the dedicated state path.
Legacy adjacent files remain visible as rollback hints and are removed by a
default `pam remove`, but they are not rewritten into versioned provenance.

`--dry-run` is honoured after the root check, so it still needs root.
`pam status` is the unprivileged read to reach for instead.

### facelock pam remove

```bash
sudo facelock pam remove                                     # /etc/pam.d/sudo
sudo facelock pam remove --service login                     # removal is never gated
sudo facelock pam remove --all                               # every recognized owned edit
sudo facelock pam remove --service hyprlock --if-present     # a missing file is success
sudo facelock pam remove --service sudo --keep-backup        # retain rollback state
sudo facelock pam remove --service sudo --dry-run --json
```

Takes the same flags as `add` except `--allow-sensitive`, which it does not
offer: removal can only take away a way to authenticate, so there is nothing to
gate. It never prompts either. Named removal uses the configured lookup path.
By default it removes committed Facelock-owned provenance and backups for the
requested service, including the legacy adjacent `<service>.facelock-backup`
name. Unresolved prepared state is preserved for recovery. `--keep-backup`
opts out of cleanup. A cleanup error remains non-zero, but the JSON action is
`cleanup-failed` and the human diagnostic says that the PAM state change
already completed.
For a Facelock-created local vendor copy, named removal first uses the normal
crash-safe complete-file replacement to remove the module rule, then deletes
the override only after moving its exact published inode to a no-replace
transaction quarantine and rechecking that inode, canonical-name absence, and
the current vendor bytes and metadata. The first existing later-root service
wins; Facelock does not accept a matching lower-priority copy. The pre-removal
document must contain exactly the
one Facelock-emitted rule; extra or customized rules are drift. A restart also
recognizes the exact header-bearing copy after the line is already absent.
Header, payload, owner/mode, or vendor drift keeps the override; Facelock never
deletes a merely similar local file. If the current vendor source is absent,
only a header path derived from a normalized configured later-root candidate is
recognized, solely to report why the local override is retained; header paths
are never opened.

`remove --all` is the package-safe, config-independent form. It opens the
compiled `/etc/pam.d`, `/usr/lib/pam.d`, and detection-only `/etc/authselect`
roots without following links, enumerates the opened directory descriptors,
and uses directory-relative regular/single-link reads. A symlink is skipped
only when its exact absolute target is the same service in a later compiled
root that is scanned independently; every other linked entry is an unmanaged
blocker. This lets Fedora's unrelated generated PAM links be checked at their
fixed root without traversing the links. Directory contents are detection
ground truth; provenance can authenticate an arbitrary service Facelock
previously changed, but never supplies a target path. An exact pre-0.2
`auth      sufficient pam_facelock.so` edit is recognized only under a
conventional service basename. Dot-prefixed and package/administrator artifact
names such as `.pacsave`, `.rpmsave` and `~` require strict provenance for that
exact name or an exact current Facelock vendor-copy header; unowned artifacts
are ignored and preserved. A customized control,
options or spacing, corrupt provenance for a candidate, any other linked entry,
or a reference in a read-only root is an unmanaged blocker. Nothing is changed
when preflight finds one. The same scan recognizes an exact unchanged
Facelock-created vendor override even if a previous run already removed its
module rule, so package cleanup can finish that bounded intermediate.

With `--dry-run`, an existing PAM backup directory is inspected read-only and
must already have its trusted owner and mode. The preview does not repair or
sync that directory, acquire its write lock, or run recovery; it refuses the
preview if the directory is not already trusted.

Before the first PAM file changes, the command persists rollback state for the
complete target set and one bounded, root-owned whole-set journal. Each
replacement re-resolves and rechecks the planned identity. A later failure or
the final compiled-root rescan finding any active reference exchanges every
earlier original inode back in reverse order. Only after the rescan is clear is
a self-contained commit marker published and cleanup finalized. Version 2
journal and commit targets carry a required `delete_override` boolean; version
1 state remains recoverable and must omit it. Once committed, a flagged target
is deleted only while the exact installed inode still matches and the
journaled header payload, owner/mode, and first existing fixed-root vendor
service still agree. The journal backup's full prepared identity and the
line-removed installed hash are checked before parsing that shape. An already
absent flagged target is an idempotent completed
unlink. Recovery rolls back a prepared journal and completes a durable commit
marker. It recognizes
an exact intent-only, pre-publication service as unstarted only while the
canonical full identity still matches and both temp and binding are absent.
After a reverse exchange, rollback removes the identity-checked replacement
temp, then its publication binding, then delegates the remaining base intent
to that exact intent-only recovery. Each boundary is restartable; ordinary
forward publication keeps its existing cleanup order.
Cleanup recovery resumes exact pair quarantine/unlink state; a fully absent
pair is already clean, but partial or conflicting state blocks. `--keep-backup`
preserves versioned and legacy rollback state for every target; the default
cleans only validated Facelock-owned state.

The command reads no Facelock config, database, model, camera, daemon, or ONNX
Runtime state. Package uninstallers invoke it while the CLI and PAM module are
still installed. Debian and RPM removal abort if cleanup cannot prove a clear
final scan. Booted coverage runs direct `dpkg`/`rpm` and the `apt-get`, `apt`,
and `dnf` frontends through abort retention and blocker-free success. Arch
packages also ship a Remove-only libalpm `PreTransaction` hook with
`AbortOnFail`, so pacman stops before removing either file.
This all-or-nothing promise covers direct PAM edits owned by this command.
Debian's separately managed `pam-auth-update` profile lifecycle and byte-exact
profile rollback are tracked in #224; the current `prerm` removes that profile
before calling the shared direct-edit cleanup.
Fedora's authselect profile lifecycle is tracked separately in #226; this
command only detects references in generated `/etc/authselect` state and never
changes that state.

### facelock pam status

```bash
facelock pam status                                          # /etc/pam.d/sudo
facelock pam status --service sudo --service polkit-1
facelock pam status --service sudo --json
facelock pam status --all                                    # everything configured
facelock pam status --all --json
```

Unprivileged, and the probe to branch on instead of grepping `/etc/pam.d`
yourself: it answers from the same file, without root, and reports "absent" and
"unreadable" as themselves rather than as "not configured". It offers
`--service`, `--all`, `--if-present` and `--json`, and neither `--dry-run` nor
`--allow-sensitive` — there is no write to preview or gate. The exit code is
the answer, on the same 0/1/2 scale as `is-enrolled` and `grep`:

| Code | Meaning |
|------|---------|
| 0 | every requested service carries the line |
| 1 | at least one exists without it |
| 2 | at least one is absent, unreadable, misnamed, symlinked out of the directory, or hard-linked |

Across several services the worst outcome wins. `--if-present` means here what
it means on `add` and `remove`: an absent service file is reported and no
longer forces exit 2, so exit 0 becomes "every requested service **that
exists** carries the line" and optional integrations can be installed and then
verified with the same flag on both commands. It forgives absence only — a
service whose file is a dangling or looping symlink is still exit 2, because an
unresolvable link is not an absent file.

```bash
sudo facelock pam add --service hyprlock --service swaylock --if-present
facelock pam status --service hyprlock --service swaylock --if-present
```

A service whose file is a local copy hiding a package's own of the same name
reads `facelock PAM line present (local override of <vendor path>)` rather than
`facelock PAM line present`, and its JSON row carries a `shadows` key naming
that file. It is configured either way; the note says the copy will not follow
the package's updates. This is a property of the row, so it appears with
`--service` as it does with `--all`, and on `pam add` and `pam remove` rows too.

`--json` emits one document:

```json
{"command":"status","dry_run":false,"module_path":"/lib/security/pam_facelock.so","services":[{"action":"present","backup":null,"path":"/etc/pam.d/sudo","service":"sudo"}]}
```

`module_path` is where `pam_facelock.so` was found, or `null` when no candidate
hit — a property of the machine rather than of a service, and what tells an
integrator that a service carries the line while the module it names is at a
path nothing looks at. `add` and `remove` refuse before writing when the module
is missing, so their documents do not carry the key.

The document's shape, the `action` vocabulary, and the rule that a consumer
must tolerate an `action` it does not recognize rather than treat it as an
error, are a stability contract — see [`contracts.md`](contracts.md), "facelock
pam Semantics", along with the exit codes for `add` and `remove` and what
`--json` does on a validation failure.

### facelock pam status --all

`--all` answers the other question: not "is this name configured?" but "what is
configured on this machine?". It replaces `--service` (the two conflict) with
every service in the resolved directories whose file names `pam_facelock.so`,
so a `polkit-1` or an `omarchy-lock-face` nobody thought to ask about is
reported. It scans rather than reading a list of what facelock has edited,
because such a list drifts the moment `/etc/pam.d` is edited by hand.

```bash
facelock pam status --all
facelock pam status --all --json
```

Nothing configured exits 1: a machine with no facelock line anywhere is not
configured, and `--if-present` does not convert that — a name reaches the report
by having been found, so there is nothing to forgive. The one exception is a
file deleted between the listing and the read, which reports `absent` like any
other.

A directory that could not be listed exits 2 and is named, rather than being
reported as holding nothing. The "nothing is configured" sentence is scoped to
the directories that *were* read and names the rest as unread in the same
breath, so taking the human answer with `2>/dev/null` cannot turn "I could not
look" into "nothing is there". A directory that does not exist is neither
case: it demonstrably holds no service files, and the default search path names
a vendor directory many machines do not have. `--all --json` adds a
`directories` key listing every directory searched with a `status` of
`scanned`, `absent` or `unreadable`.

Only regular files are read. A FIFO, socket, device node or symlink to a
directory in a `pam.d` directory is skipped rather than opened, since reading a
FIFO blocks until a writer appears and a diagnostic that hangs on a malformed
`/etc/pam.d` is worse than one that omits an entry no PAM stack could use. An
entry that merely *could not be examined* — a symlink into a directory you may
not traverse, a symlink loop, a dead mount — is not skipped: it is reported
`unknown`, exit 2, which is what `--service` says about it too. The exception
is a path that is simply not there, which is an absence rather than an
unanswerable question: a dangling symlink is skipped by `--all` and reported
`unknown` by `--service`. An entry whose name is not valid UTF-8 is skipped
and logged.

## facelock hyprlock

Manage hyprlock lock-screen integration: the face glyph in `placeholder_text`,
and the `ignore_empty_input = false` setting that lets a bare Enter submit to
PAM. Runs as your normal user and refuses to run as root, since it edits
`~/.config/hypr/hyprlock.conf` and root would leave root-owned files in `$HOME`.
A backup is taken before the first edit.

```bash
facelock hyprlock enable                # face icon, and allow the empty-Enter submit
facelock hyprlock enable --no-icon      # only set ignore_empty_input = false
facelock hyprlock disable               # undo
facelock hyprlock status                # report the current state
```

`--no-icon` is for a hyprlock font with no Nerd Font glyphs; it flips the
functional setting and leaves any existing icon alone. `disable` restores
`ignore_empty_input` only when no fingerprint icon coexists, so a machine using
both keeps working.

Wiring `/etc/pam.d/hyprlock` itself is a separate, root step — see
[`facelock pam`](#facelock-pam). This command touches no file outside `$HOME`.

## facelock audit

View the structured audit log of authentication events.

```bash
facelock audit                          # show last 20 entries (default)
facelock audit -l 50                    # show last 50 entries
facelock audit --lines 50               # long form
facelock audit -f                       # follow mode: stream new entries as they arrive
facelock audit --follow                 # long form
```

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--follow` | `-f` | false | Watch for new entries (like `tail -f`) |
| `--lines N` | `-l` | 20 | Number of recent entries to display |

## User Resolution

For commands that accept `--user`:
1. Explicit `--user` flag (highest priority)
2. `SUDO_USER` environment variable
3. `DOAS_USER` environment variable
4. Current user (`$USER` or `getpwuid`)

## Environment Variables

| Variable | Purpose |
|----------|---------|
| `FACELOCK_CONFIG` | Override config file path for unprivileged CLI commands. Ignored by privileged PAM/root auth flows; use `--config` there. |
| `RUST_LOG` | Control log verbosity (e.g., `facelock_daemon=debug`). Outranks both the built-in default and `-v`. An unparseable value is reported at `warn` and ignored. |
