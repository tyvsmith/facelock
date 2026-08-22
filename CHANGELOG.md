# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Explicit authorization for sensitive `setup --pam` writes** (#207):
  `--yes` and its `--no-confirm` alias now suppress only the ordinary per-file
  confirmation. Adding Facelock to a shared or login PAM stack requires the
  independent `--allow-sensitive` flag, matching `facelock pam add`; the flag
  requires `--pam`, conflicts with `--remove`, and is advertised by capability
  `setup-allow-sensitive`.
- **`facelock pam status --all`** (#203): enumerate every service in the
  resolved pam.d directories that names `pam_facelock.so`, through the same row
  builder, `action` words and JSON document as `pam status --service`, so the
  two cannot disagree about one service. Bare `pam status` still means `sudo`
  and keeps its exit codes; `--all` conflicts with `--service`, has no short
  letter, and answers 1 when nothing is configured (`--if-present` does not
  change that). "Not checked" is a distinct fact from "not configured": a
  directory that could not be listed is named on stderr and in a new additive
  `directories` key (`scanned` / `absent` / `unreadable`), the empty answer is
  scoped to what could be read, and an entry that cannot be examined is an
  `unknown` row rather than a missing one. Rows carry `shadows` (omitted when
  nothing is shadowed) naming the vendor file an `/etc/pam.d` copy overrides,
  and the human line says `(local override of …)`. Package-manager leftovers
  (`.pacnew`, `.rpmsave`, `.dpkg-old`, `~`, `.facelock-backup`), dotfiles,
  non-regular files and non-UTF-8 names are not services. Capability
  `pam-status-all`.
- **`facelock status --json`** (#204): the last diagnostic command a script could
  not parse. A separate wire type built from the health model; nothing in
  `health.rs` learns `Serialize`, so Rust names are not the contract. Every
  section and nested fact is tri-state `ok` / `problem` / `unknown` with a
  `reason` on anything not `ok`; there is no `null`, and no `false`, `0` or
  `[]` ever stands in for "not interrogated" (an unreadable store omits
  `enrollment.models` rather than emitting an empty array; uncounted embeddings
  are `unknown`, not 0/0). Machine output never touches the translation catalog;
  reasons are fixed identifiers or the probe's own diagnostic, and `daemon`
  carries no free-text error because the transport's can be a localized hint.
  The PAM section reuses `pam status`'s row shape and words. One fixture drives
  both renderers and a test asserts they agree section by section; another
  asserts the example document in `docs/contracts.md` is byte for byte what the
  build emits. `state` is section-specific (under auto-detect `camera.state`
  says detection is on and `camera.device.state` says whether a camera exists),
  and the contract tabulates what each answers. Exit codes are unchanged (0
  whenever a report was produced), so `--quiet --json` prints nothing. The
  daemon integration tests wait on `status --json` + `jq` instead of grepping
  the human report. Capability `status-json`.
- **`--if-present` on `setup --pam` add, not only on `--remove`** (#202): the
  alias now takes the flag the verb already had, on the standalone form and
  under a wizard base (`setup --pam --service X --if-present --enroll`), where
  it used to be dropped silently. Absent service → exit 0 with a note;
  permission denied, a malformed file and a failed write stay fatal with or
  without it; the default is still a hard error, which is what catches
  `--service polkti-1`. A skipped or declined service is no longer listed in the
  closing summary or handed to the hyprlock integration.
- **`facelock pam add | remove | status`** (#180): one command owns every write
  to `/etc/pam.d`, and `setup --pam` is now an alias onto it that keeps parsing
  for existing wrappers. `--service` is repeatable on all three verbs, so
  several services are configured in one process under one root check.
  Validation is two-phase: a typo'd or gated service name writes nothing at all.
  `add` takes `--allow-sensitive` to unlock the shared auth stacks and
  `--dry-run` to print the resolved plan; `--if-present` turns an absent service
  file into a successful no-op on all three verbs; `--json` emits one document
  whose shape and `action` vocabulary are a stability contract. `status` reads
  only and needs no root — the probe to branch on instead of grepping
  `/etc/pam.d`, on `grep`'s 0/1/2 scale.
- **`facelock capabilities`** (#182): what this build can do, one name per line
  or `--json` for `{"version", "capabilities"}`. Unprivileged, reads no config,
  activates no daemon; it replaces grepping `--help`, which is not an API.
  **Probe by name, never by version** — a git or distro build carries a version
  that says nothing about what is in it, and a backport can add a feature
  without moving the number. The name list is a stability contract: names are
  added, never removed or repurposed, and a consumer tolerates one it does not
  know. A build that predates the command answers by failing, which a caller
  reads as "no capabilities at all".
- **Global `-c`/`--config` and `-q`/`--quiet`, accepted on either side of the
  subcommand** (#178/#179): each is declared once, so `facelock daemon -c X` and
  `facelock -c X daemon` are the same invocation and no command re-declares
  them. `--user` is `-u` everywhere it exists, `facelock auth` included, and
  `remove`/`clear` accept `--no-confirm` as an alias for `-y` (it was
  `setup`-only). A conformance test walks the whole command tree, nested
  subcommands included, and fails on any drift.
- **`facelock preview --json`** (#181): the flag shipped as `--text-only`, which
  survives as a hidden alias and keeps parsing; the per-frame payload is byte
  for byte what it was. It comes with a rule — every command whose output a
  script would parse takes `--json`, spelled exactly that, with no short letter
  and no `--output json`, and gains it when it has a named consumer rather than
  to complete a matrix. The coverage list is a registry checked in both
  directions against the command tree.
- **Every command is documented, and pinned there by tests** (#184): the CLI
  reference, the man page and the book. A subcommand that ships without a
  heading fails the build, as does a documented example that no longer parses or
  that installs into a gated PAM service without the flag that unlocks it.
- **The container PAM tier exercises `facelock pam` end to end**: `add`,
  `remove` and `status` against a real `/etc/pam.d`, covering the sensitive
  gate, the two-phase all-or-nothing rule, a rejected service name and a symlink
  out of the directory.
- **Optional idempotent PAM-line removal** (#148): `facelock setup --pam
  --remove --if-present` now succeeds when the requested PAM service file is
  absent, so teardown scripts can iterate optional integrations without their
  own existence guards. The behavior is opt-in: omitting `--if-present` keeps
  the historical missing-file error, and all non-NotFound I/O failures remain
  fatal. Existing service files are never deleted.
- **Complete flag surface for `facelock setup`**: every wizard step can now be
  answered or declined from the command line. Choice flags `--camera
  <PATH|auto>`, `--models <standard|balanced|high>`, `--execution-provider
  <cpu|cuda|rocm|openvino|auto>` and `--encryption <tpm|keyfile|none|auto>`
  supply a value and thereby replace the corresponding prompt (precedence: CLI
  flag > config file > built-in default). Action flags `--no-pam`,
  `--no-systemd` and `--no-enroll` decline an action outright, with `--pam`,
  `--systemd` and `--enroll` as their forcing counterparts; each pair is an
  override pair, so the later flag on the command line wins. There is
  deliberately no `--skip-<x>-prompt` family: on the steps that change the
  system, "skip" would have to mean "apply the default", so the one flag an
  integrator reaches for to avoid facelock touching PAM would instead configure
  it for every pre-checked service (`sudo`, `polkit-1`, `hyprlock`, `swaylock`,
  `kscreenlocker_greet`, `lightdm`). `auto` means re-derive from the hardware,
  not "use the default" — omitting a flag already gives the default.
- **`--execution-provider=auto` detects the available GPU providers**: setup now
  asks the loaded ONNX Runtime which execution providers it was built with and
  selects `cuda` > `rocm` > `openvino` > `cpu`. A machine with
  `onnxruntime-opt-cuda` installed previously got CPU inference and was never
  told GPU was available. The choice is always explained on stdout, including
  the CPU case ("the installed ONNX Runtime has no GPU execution providers
  compiled in; selecting cpu"), so `cpu` is never a silent outcome.
- **`facelock is-enrolled`**: a cheap, unprivileged query for whether a user has
  a usable face enrollment, so a lock screen can decide whether to offer a
  face-auth affordance. Named after systemd's `is-*` family (`systemctl
  is-active --quiet`), and like those it prints the state word — `enrolled` or
  `not-enrolled`. The exit code is the contract — `0` enrolled, `1` not
  enrolled, `2` error, matching `grep`'s convention — with `--user`, `--json`
  and `--quiet`. It answers from a
  per-user marker under `/var/lib/facelock/enrolled/` (`0710 root:facelock`
  directory, `0600` files owned by their user) and never activates the daemon
  over D-Bus, opens a camera, or reads the database. "Enrolled" means "face
  auth is operational for me": reaching the marker requires `facelock` group
  membership, so a caller outside the group reports `not-enrolled` — correct,
  since the group is required to reach the daemon at all. The
  marker is a hint for the UI, not authority: it can drift, and PAM at
  authentication time remains authoritative. Markers are maintained by `enroll`,
  `remove` and `clear`, and converged from the database — the authoritative
  source — by every `setup` run, by daemon startup, and by the one-shot
  `facelock auth` path for the user it is authenticating (#137). An install
  upgraded from a release without markers therefore backfills itself on the
  first daemon start or the first authentication, without a migration step:
  each convergence re-derives the markers rather than replaying recorded
  changes, so it is idempotent and keeps no state that a restored backup could
  contradict. On the one-shot path the convergence runs after the pre-flight
  gates (so a rate-limited attempt writes nothing) and before the camera opens
  (so a signal, a busy camera, a failed model load or a plain non-match cannot
  suppress it).
- **Enrollment failure breakdown** (#89): when enrollment captures too few
  frames, the error now reports why frames were rejected (too dark, no face,
  multiple faces, low quality, capture errors with the last error message) and
  hints at the fix when one cause dominates (e.g. "improve lighting").
- **Setup manages facelock group membership** (#89): `sudo facelock setup` now
  creates the `facelock` system group if missing and adds the invoking
  sudo/doas user to it (the interactive wizard asks first; non-interactive mode
  adds without prompting, and prints a manual `usermod` command when no
  invoking user can be determined), so daemon commands like `facelock
  preview`/`test` work after setup without a manual `usermod`. A log-out/log-in
  reminder is printed. Superseded in this release by ADR 010, which retired
  the group.
- **`facelock status` reports whether the PAM oneshot fallback is usable**: a
  new "Oneshot fallback" section says whether root-invoked PAM could
  authenticate via `/usr/bin/facelock auth` with the daemon unreachable — the
  binary, both configured model files, and the database must be present — and
  names whichever prerequisite is missing.
- **`facelock status` explains the camera**: when the configured (or
  auto-detected) device can be interrogated, the report shows its
  evidence-based IR classification and any camera-quirks entries that will
  shape how it is opened, so "why does this camera behave that way" is
  answerable from the report. Auto-detect additionally names the device it
  would select right now.
- **`facelock status` flags a stale `is-enrolled` marker**: running as root it
  compares the target user's marker against the database and prints a
  diagnostic when they disagree (or when the marker is unreadable), pointing
  at `sudo facelock setup` to reconcile. `is-enrolled` itself is unchanged.
- **Opt-in camera hold after a successful authentication**: a new
  `device.camera_release_after_success_secs` (default **0**) keeps the camera
  streaming for that many seconds after a success, the way
  `camera_release_secs` already does after a failure. At its default — which is
  the recommended value and what every install gets without touching the file —
  nothing changes: a success ends the interaction, so the stream (and on IR
  hardware the emitter LED) goes out with the reply. It exists for the one
  shape that really does re-authenticate immediately: privileged actions
  repeated with no authentication caching in front of them, `sudo` with a zero
  `timestamp_timeout` or a polkit action without `auth_admin_keep`, where each
  action is a fresh authentication that would otherwise pay a camera reopen.
  Failed attempts keep using `camera_release_secs`; cancellations and errors
  release the camera at once whatever both keys say. Additive with a serde
  default: no existing config is rejected, and the daemon reads it per request,
  so no restart is needed.
- **`facelock bench camera-reopen`**: measures what it actually costs to go
  from a closed camera to the first frame an authentication can analyze, split
  into device open + format negotiation, `STREAMON` + first frame, warmup
  discard, and first usable frame, over `--iterations` cycles (default 5). This
  is the number `device.camera_release_secs` trades LED-on time against —
  holding the stream warm after a failed attempt buys a retry exactly this
  much (ADR 008) — and it had never been measured: the docs asserted "~400 ms"
  and "~600 ms cold" from nobody's hardware in particular. Those figures are
  gone from the docs in favor of pointing at this subcommand, since the answer
  is a property of the camera and its driver, not of facelock. Needs no
  enrolled face and loads no models.

- **NV12 and Y16 pixel format support** (#89): NV12 (semi-planar 4:2:0, common on
  Intel IPU6/IPU7 processed cameras via v4l2-relayd) and Y16 (16-bit IR grayscale,
  bit-depth-aware conversion) are decoded natively. Negotiation priority is now
  `GREY > Y16 > YUYV > NV12 > MJPG`. IPU6/IPU7 relay cameras can now be opened,
  previewed and enrolled — but their IR sensors have no in-tree Linux driver
  yet, so authentication still fails under the default
  `security.require_ir = true` on stock kernels (experimental community driver
  support is tracked in #101).
- **Intel IPU6/IPU7 + v4l2-relayd compatibility recipe** in `docs/compatibility.md`.

### Changed

- **Face unlock needs no group membership and no re-login** (ADR 010): the
  system-bus policy now admits any local user's `Authenticate` for their own
  account — the daemon already checks that the caller's UID owns the username
  it names, and every other method stays root-only at both the bus and the
  daemon. hyprlock/swaylock/polkit face unlock and the `is-enrolled` face
  icon work the moment enrollment finishes. `/var/lib/facelock` and
  `/var/lib/facelock/enrolled` are `0711 root:root` (traversable by all,
  listable by none; database and markers keep their `0600` modes); the
  `facelock` group is retired: the bus policy no longer names it (signals are
  root-only), packaging no longer creates it, `/run/facelock` is `root:root`,
  and setup, `just install-files` and the package scriptlets remove a leftover
  group best-effort. The CLI's `AccessDenied` hint says "root required"
  instead of "join the group". Upgrades converge through tmpfiles, the package
  scriptlets, and the binary's own layout enforcement; the scriptlets and
  `setup --systemd` also ask the bus to reload its policy. Widened residual,
  accepted: any local user can `stat` a name it guesses under the state
  directory (previously group members only).
- **The setup wizard configures the daemon before enrolling** (#200): the order
  is now Camera → Model quality → Inference device → Model download →
  Encryption → **Daemon → Enrollment → Test** → PAM. On a first install
  enrollment and the recognition test used to run before the daemon existed,
  fell back to direct camera access with a `DirectByFallback` warning, and so
  validated a code path no later authentication takes. The daemon step now
  starts the unit (or restarts it when it is already running, so a re-run
  enrols through the daemon holding the wizard's configuration; a restart can
  interrupt an authentication in flight, which falls back to the password once,
  where a stale daemon would silently hold the wrong key or model) and waits for
  it to answer; `--systemd` and `--systemd --disable` are applied inside the
  daemon step under a wizard or non-interactive base instead of after the whole
  flow, and standalone `setup --systemd` is unchanged. `--no-systemd` and
  oneshot installs enrol exactly as before. Step banners renumbered; PAM stays
  step 9.
- **`facelock status` reports every configured PAM service, not just `sudo`**
  (#203): the report line is `PAM services: sudo, polkit-1 (2 configured, 1
  shadowing a vendor file)`, `none configured` (only when every directory was
  read), or `not checked` with the place that could not be read, built from the
  same scan as `pam status --all`. The old `sudo PAM: configured` line stat-ed
  one hardcoded file and could not tell "not configured" from "could not look".
- **The CLI defaults to `warn`, and the daemon still logs at `info`.** Every
  command used to emit INFO on stderr, so the setup wizard's questions arrived
  interleaved with timestamped log lines and a run that was succeeding looked
  broken. Diagnostics now start at `warn`, and the new global `-v` raises them
  one step per repeat from where the process starts (`-v` info, `-vv` debug,
  `-vvv` trace for the CLI). `facelock daemon run` is unchanged at `info`, because it writes to the journal, where nothing
  competes with it. `RUST_LOG` still outranks both, and unlike the environment
  variable, `-v` survives `sudo`. Exit codes and stdout payloads are untouched
  at every level, and every degradation worth acting on was already WARN or
  above.
- **`--quiet` has one implementation and one meaning** (#179/#193): it
  suppresses informational stdout and, on a command whose stdout is the payload,
  the payload too, so `facelock --quiet devices --json` writes nothing and the
  exit code is the whole answer. `is-enrolled`, `capabilities`, `pam`,
  `list --json` and `devices --json` all follow it; `list --json --quiet` and
  `devices --json --quiet` used to print their payload and no longer do. Errors,
  prompts, exit codes and `pam add`'s rollback advice are unchanged — a silenced
  question is a hang, not a quieter program. The flag is read by the two
  suppressible stdout sinks of the message seam, so no command implements it and
  none can forget it.
  [#140](https://github.com/tyvsmith/facelock/issues/140) tracks the commands
  that still print human text directly and stay noisy under it.
- **PAM writer hardening** (#194). Eight services are gated where three were:
  `common-auth`, `login`, `password-auth`, `password-auth-ac`, `sshd`,
  `system-auth`, `system-auth-ac` and `system-login`. Whether the gate fired
  used to depend on the operator's distribution, and RHEL's older `authconfig`
  leaves the `-ac` names pointing at the real file. `pam add` now refuses a
  service file that is a symlink out of `/etc/pam.d` — on an authselect system
  `system-auth` links into `/etc/authselect`, where the edit is regenerated away
  — or that has more than one hard link, since a link count says another name
  for the inode exists and not where, which is the one question confinement
  answers. A symlink that stays inside the directory is followed, and the
  sensitive gate then runs on the file it reaches rather than only on the typed
  name. `--json` implies `--no-confirm` on `pam add|remove`, because the
  per-file question is drawn on stderr while a parser waits on stdout; it does
  **not** imply `--allow-sensitive`, which is an authorization a machine caller
  has not given. `pam status` gained `--if-present`, so "install the optional
  integrations, then verify" is a pair of commands with the same flag on both.
- **Upgrade note — authselect and authconfig systems.** Where an earlier
  facelock wrote through `/etc/pam.d/system-auth` into `/etc/authselect/…`,
  `facelock pam remove --service system-auth` now refuses rather than following
  the link; confinement applies to every verb. The message names the target file
  so it can be edited by hand, or through `authselect` itself. Nothing is
  removed silently and nothing is left half-written.
- **The hyprlock hint follows only a file facelock would write** (#195): a
  `/etc/pam.d/hyprlock` that is a symlink out of the directory no longer draws
  "your lock screen is wired up", because that is a service the writer refuses
  to touch. The rest of the alias refactor behind it is not observable.
- **`is-enrolled --json` documents `updated` as `null` when the user is not
  enrolled.** There is no marker to read a timestamp from, which is what the
  command has always emitted; only the documentation was wrong.
- **BREAKING: `facelock restart` is now `facelock daemon restart`** (ADR 009).
- **BREAKING: `facelock encrypt` is now `facelock tpm encrypt`** (ADR 009).
  `--generate-key` is unchanged.
- **BREAKING: `facelock decrypt` is now `facelock tpm decrypt`** (ADR 009).
- **BREAKING: `facelock reseal` is now `facelock tpm reseal`** (ADR 009).
- **BREAKING: `facelock config --edit` is now `facelock config edit`** (ADR
  009).
- The old spellings are removed rather than aliased, so they exit 2 with clap's
  unrecognized-subcommand error. Clap has no cross-level alias, and the project
  is pre-1.0 with no external caller of the four names: the byte-coupled
  surfaces in ADR 009 §4 and the Omarchy scripts name none of them. Bare
  `facelock daemon` still runs the daemon and bare `facelock config` still
  shows, so every shipped service unit and the `ExecStart` marker in
  `facelock setup --systemd` keep working unchanged. The privilege split is
  unchanged too: `config show` is unprivileged, `config edit` is root. The rule
  that decides whether a new command is top-level or lives inside a noun group
  is now written into `docs/contracts.md` §CLI Subcommands and pinned by a
  `TOP_LEVEL_COMMANDS` registry test.
- **`facelock tpm status` and `facelock tpm pcr-baseline` now require root**,
  like every other `tpm` verb. Neither checked, which left `status` failing
  with a raw sqlite "unable to open database file" error when it reached the
  root-only face database, and left `pcr-baseline` printing its header
  and PCR values and exiting **0** for an unprivileged caller — that
  invocation now refuses with `Root required.` and exits **1**. `tpm status`
  reports key and embedding state read from the face database, so it belongs
  with `seal-key`/`unseal-key`, not with the world-readable-file probes
  `pam status` and `capabilities`.
- **An authentication at an empty chair ends early and costs no rate-limit
  budget** (ADR 008 §3/§4). A new `recognition.no_face_timeout_secs` (default
  **2**) ends an attempt once that many seconds have passed with no face
  detected at all; `recognition.timeout_secs` still bounds the slower case a
  timeout is actually for — a face was seen and has not matched yet. A laptop
  opened in front of nobody therefore lights its IR emitter for 2 seconds
  instead of 5. The key is additive with a serde default, is clamped to
  `timeout_secs` rather than validated against it, and `0` disables the early
  exit, so no existing `/etc/facelock/config.toml` needs to change. Separately,
  and regardless of that timeout, a failed attempt in which the camera never
  saw a face no longer calls the rate limiter on either the daemon or the
  one-shot path: an empty chair is not a guess, and charging it let a locker
  that starts face auth on every wake spend the user's whole 5-attempt budget
  before they sat down — the real attempt then met a lockout. A face that
  *was* seen and did not match is charged exactly as before. The early ending
  reports the same outcome the full timeout reports, so no client, sentinel or
  audit result gains a case.
- **One-shot `facelock auth` no longer outlives its PAM host or lights the
  camera during the model load** (ADR 008 §7). Three changes, no new exit
  code and no change to the existing one: the ONNX engine now loads *before*
  the camera opens, so the IR LED is lit for the scan rather than for the
  model load as well; SIGTERM, SIGINT and SIGHUP end the scan through the same
  cancel token, which lets `Drop` run STREAMOFF and turn the emitter off
  before exiting 2 with a `cancelled` log line; and the PAM module sets
  `PR_SET_PDEATHSIG = SIGTERM` on the child, so a killed PAM host (an aborted
  locker helper, a killed `sudo`) takes the helper with it instead of leaving
  it scanning, reparented to init, with the camera on. The PAM module's own
  timeout now sends SIGTERM and waits up to 500 ms before SIGKILL; it used to
  SIGKILL immediately, which skips `Drop` and can leave an XU-controlled IR
  emitter lit.
- **An authentication whose caller has gone away now ends within one frame**
  (ADR 008 §5). Nothing could shorten the scan loop before: when a screen
  locker aborted PAM because the password was typed first, or a `sudo` was
  killed, or a client crashed, the daemon kept capturing — and kept the IR
  emitter lit — until `recognition.timeout_secs`. Every in-flight request now
  carries a cancel token, checked once per iteration by the auth loop, the
  enroll loop and both frame-discard loops. It is set when the caller's D-Bus
  connection disappears (a per-request `NameOwnerChanged` watch on the
  caller's bus name), on suspend, on `ReleaseCamera`, and on shutdown — all
  without taking the handler lock, which is what the request being cancelled
  is holding. A cancelled attempt releases the camera immediately, is audited
  as `cancelled` rather than `failure`, and **charges no rate-limit budget**:
  the user never got to make an attempt. On the wire it reuses the
  recoverable-error encoding with the frozen message `cancelled`, which the
  PAM module maps to `PAM_IGNORE` — the stack falls through to the password
  the user was already typing. No new D-Bus method, no signature change. The
  suspend path in particular no longer gives up with a "handler busy" warning
  and leaves the camera streaming into sleep.
- **The daemon holds the camera open only after a failed authentication**
  (ADR 008). Previously every request — success included — left the V4L2 stream
  live for `device.camera_release_secs`, which on IR hardware is a visible
  emitter LED burning for five seconds after the screen had already unlocked.
  Success, cancellation and every error class now release the camera as the
  request returns; only a no-match or timeout keeps it warm, because that is
  the one ending a retry plausibly follows. The default drops from **5 to 3
  seconds**, `0` now means *never hold* instead of being silently substituted
  with 5, and the release is polled every 250 ms against an absolute deadline
  rather than once a second. A warm reuse discards the stale V4L2 buffers
  before analyzing anything, so a fresh attempt can never match on the tail of
  the previous one. Preview frames keep their own floor of
  `max(camera_release_secs, 2s)` so a live preview never reopens per frame.
  **No action required on upgrade**: the key is unchanged in name and type, the
  shipped config template has it commented out, and the daemon re-reads it per
  request.
- **`facelock status` says "cannot determine" instead of guessing**: a section
  whose probe failed — an unreadable database, a config that did not parse —
  now reports exactly that, and a daemon that is unreachable can never render
  as "no faces enrolled". Previously a broken config silently dropped several
  sections from the report; every section now always renders, as its value or
  as honestly unknown. Internally the command is a pure renderer over a
  `Health` fact model, so the report — including the exact bytes the container
  tests grep — is pinned by unit tests.
- **State directory and log permissions tightened — no paths moved, no data
  migration**. The database stays at `/var/lib/facelock/facelock.db` and the
  models at `/var/lib/facelock/models`; what changes are modes and ownership:

  ```
  /var/lib/facelock/            0710 root:facelock   traverse-only, NOT listable
    facelock.db (+-wal/-shm)    0600 root:root       was 0640 root:facelock
    models/                     0755 root:root       unchanged
    enrolled/                   0710 root:facelock   new — is-enrolled markers
      <user>                    0600 <user>:<user>
  /var/log/facelock/            0700 root:root       was 0750 root:facelock
    audit.jsonl                 0600 root:root       was 0640 root:facelock
    snapshots/                  0700 root:root       was 0750 root:facelock
  ```

  **One gate at the top.** The state directory grants "other" nothing, so a
  local user outside the `facelock` group can reach nothing below it. The
  group gets traverse-only: a member can open a path it knows by name — its
  own enrollment marker, a model file — but cannot list the directory or read
  the `0600 root:root` database. The group is a **D-Bus access grant, not a
  file-read grant**: members request authentication through the daemon, which
  reads the templates as root. This also closes the group's direct reads of
  the audit log (per-user auth history) and snapshots (raw face images), both
  strictly more sensitive than the encrypted templates. A guard test walks the
  state directory and fails if any entry but `models/` carries "other" bits.

  **D-Bus is required for user-run screen lockers** (hyprlock/swaylock): their
  PAM stack runs as the user, and no group membership makes the database or
  encryption key readable. Root-invoked PAM (`sudo`, `login`, `sshd`) also has
  the oneshot fallback, which reads the files directly as root.

  For an existing install the entire on-disk change is a `chmod`/`chown` of
  the paths above plus `mkdir enrolled/`, applied idempotently by packaging
  (tmpfiles `z` lines, install scriptlets, OpenRC `start_pre`, the NixOS
  module, `just install-files`) and re-applied by any root invocation of the
  binary. None of it touches the data itself. The places that encode the
  layout and must stay in sync: `dist/facelock.tmpfiles`,
  `dist/facelock.install`, `dist/debian/postinst`, `dist/nix/module.nix`,
  `dist/openrc/facelock-daemon`, the `install-files` recipe in `justfile`,
  `secure_setup_paths()`, the default path constants in
  `crates/facelock-core/src/paths.rs`, and the typed constants plus guard
  tests in `crates/facelock-cli/src/state_layout.rs`. `just test-arch-layout`
  asserts the shipped modes end to end. See `docs/contracts.md` for the
  permission table as a contract change and `docs/security.md` §A2/§A3 for the
  rationale. ADR 010 (above) later changed the two directories to
  `0711 root:root` and removed the group; the database, log and snapshot modes
  here are unchanged.
- **`facelock setup` flags now compose instead of being mutually exclusive**:
  `--pam` and/or `--systemd` on their own still perform just that action and
  touch nothing else, but any flag that only makes sense while the base setup
  runs — `--non-interactive`, a choice flag, or any of `--no-pam` /
  `--no-systemd` / `--enroll` / `--no-enroll` — now forces the base setup, and
  the requested actions run in addition to it. Single-flag behaviour is
  unchanged; see the two silent flag drops under **Fixed**.
- **Direct-mode enrollment unified with the daemon loop** (#89): `facelock
  enroll` in oneshot/direct mode previously ran a drifted copy of the
  enrollment loop that skipped the frame quality gate and the angle-diversity
  check, and lacked the new rejection breakdown. Both modes now share
  `facelock_daemon::enroll` — direct enrollments get the same quality
  enforcement and error reporting as daemon mode.
- **Enroll no longer D-Bus-activates the daemon in direct mode** (#89): label
  auto-generation and the model-count warning used an unconditional D-Bus
  `ListModels` call, which could boot the system daemon via bus activation and
  silently flip the enrollment from direct to daemon mode. They now read the
  store directly when direct mode applies.
- **Direct-mode authentication unified with the daemon loop** (#89): `facelock
  test` in oneshot/direct mode ran a local fork of the auth loop
  (`mod facelock_daemon_auth` in `direct.rs`) that had drifted from
  `facelock_daemon::auth`. It wrote **no audit entries**, ignored
  `[snapshots]` entirely, and never zeroized templates that device coupling had
  filtered out. Both modes now call
  `facelock_daemon::auth::authenticate_with_embeddings`. The PAM path is
  unaffected — `facelock auth` already called the daemon implementation.

- **Auto-detection skips undecodable devices** (#89): devices that advertise no
  decodable pixel format (e.g. raw Bayer sensor nodes like the IPU7's `SGRBG10`)
  are excluded from every auto-detection tier. When no decodable camera exists,
  the error lists every detected device and its formats.
- **Camera open fails fast on undecodable formats** (#89): instead of silently
  negotiating an unsupported format (and then failing every capture), opening a
  device with no decodable format errors immediately with the advertised and
  supported format lists.
- **Pixel-format names lose V4L2's trailing-space padding** (#89): FourCCs are
  normalized where they enter facelock (device enumeration and quirks-file
  parsing), so `facelock devices --json` now carries `"Y16"` where it
  previously carried `"Y16 "`. This applies on the direct backend only, which
  is the one that reports formats at all: the D-Bus `DeviceInfo` carries no
  formats field, so a daemon-backed `--json` reports `"formats": []` either
  way. The human-readable `facelock devices` table already trimmed and is
  unchanged. A consumer that reads formats from the direct backend and matches
  the padded spelling exactly needs updating; one that trims already does not.

### Fixed

- **PAM service files are resolved through the vendor directories** (#201):
  `/etc/pam.d`, then `/usr/lib/pam.d`, first hit wins, configurable with
  `[pam] config_dirs`. On a current Arch install polkit ships its stack at
  `/usr/lib/pam.d/polkit-1` and `/etc/pam.d/polkit-1` does not exist, so
  `setup --pam --service polkit-1` failed with "service file not found" and
  Omarchy's setup aborted before the lock screen's own service got its line.
  A vendor directory is never written: a vendor-only service is copied to
  `/etc/pam.d/<service>` with a provenance header and the facelock line in one
  atomic write, the operator is told the copy now shadows the vendor file, and
  the new `action` words `overridden` and `vendor-only` say which happened;
  `pam remove` on a vendor-only service is a no-op exit 0. Every write the
  module makes (the edit, the copy and the backup) goes through one
  temp-file-fsync-rename primitive that carries mode, owner and the SELinux
  label (POSIX ACLs and other xattrs are not carried; written down). The
  symlink rule is restated per directory (a vendor file symlinked into
  `/etc/pam.d` is refused), the hard-link check now also covers the target of
  an in-directory symlink, and a stat error other than "not found" no longer
  falls through to a later directory. `PamFileNotFound` names every path tried;
  the wizard's PAM menu offers `polkit-1` exactly when `pam add` can configure
  it. Direct service-file editing is the Arch-family path; Debian
  (`pam-auth-update`) and Fedora (`authselect`) are out of scope and say so.
- **The PAM module is probed at `/lib/security`, `/usr/lib/security` and
  `/usr/lib64/security`** (#201, #170): one read-only list shared with the
  health probe, first hit wins, and the refusal names every candidate. The
  single hardcoded `/lib/security/pam_facelock.so` refused every service on
  Fedora, where this repo's own spec file installs to `/usr/lib64/security`.
  `pam status --json` gains a top-level `module_path` (`null` when nothing was
  found).
- **`pam add` refused to install when stderr was redirected** (#194): the
  per-file confirmation tested stdin, but `dialoguer` draws *and* reads the
  prompt on stderr, so `sudo facelock pam add --service sudo 2>install.log` had
  a terminal on stdin, none where the prompt goes, and failed the service having
  written nothing. Redirecting a log is not a reason to refuse to install, so
  the guard now takes both streams and skips the question when either is not a
  terminal. The sensitive gate is decided before any prompt exists and is
  unaffected.
- **`--json` output corrupted by log lines on stdout** (#149): the CLI's tracing
  subscriber inherited `tracing_subscriber`'s default writer, which is *stdout*
  — the same stream `devices --json`, `list --json` and `is-enrolled --json`
  print their payload on. Any diagnostic that passed the `RUST_LOG` filter was
  prepended to the JSON, so with `daemon.mode = "daemon"` and the daemon
  stopped, `facelock devices --json | jq .` failed on the D-Bus fallback WARN.
  All tracing output now goes to stderr, in `facelock`, `facelock-bench` and
  `facelock-polkit` alike; stdout carries the payload and nothing else. The
  three `facelock` init sites were collapsed into one `logging::init_stderr`,
  and the conformance test that already guarded the log *filter* now also
  guards the *writer* by asserting no other file in the crate touches
  `tracing_subscriber` at all. Also: a `RUST_LOG` that cannot be parsed is now
  reported at WARN instead of being silently discarded.
- **One-shot `facelock auth` could not clear a stale enrollment marker**: the
  enrollment pre-flight gate short-circuits above the marker convergence point,
  so on a daemonless install — where daemon startup's `reconcile_all` never runs
  — a marker left behind by a database restored from a pre-enrollment backup (or
  a row removed out of band) was permanent: every later attempt was rejected at
  the gate and returned before converging, and `facelock is-enrolled` reported
  enrolled forever. The one-shot now deletes a marker the database
  authoritatively contradicts, at the gate that observes the contradiction. The
  marker *write* stays below the pre-flight gates exactly as before — this is a
  removal only (one `unlink`, no temp file, no `chown`, no `rename`, no
  directory created), it is idempotent, and it is reachable only when the marker
  is already false, so no attacker-drivable filesystem work is added on the
  wrong side of the rate limiter. `docs/contracts.md` no longer implies the
  one-shot path re-derives every marker; it converges exactly one user's.
- **`setup --systemd --pam` silently dropped `--pam`**: the dispatch was an
  `if systemd {} else if pam {}` chain, so asking for both ran only systemd and
  said nothing about it. It now runs both, systemd first, matching the order the
  wizard uses for steps 8 and 9.
- **`setup --non-interactive --pam` silently dropped `--non-interactive`**: the
  same chain ran PAM only, skipping directory creation, model download and
  verification, encryption and permission hardening. It now runs the base setup
  and then PAM.
- **Action modifiers no longer vanish without their action**: `--remove` and
  `--service` require `--pam`, and `--disable` requires `--systemd`. Passing one
  alone is now a parse error naming the missing flag instead of being ignored.
- **D-Bus Enroll timeout race** (#89): the CLI's fixed 15-second D-Bus method
  timeout was at or below the daemon's enrollment deadline
  (3x `recognition.timeout_secs`, minimum 15s), so `facelock enroll` in daemon
  mode could fail with "I/O error: timed out" while the daemon was still
  enrolling. Enroll now uses a dedicated connection whose timeout is the shared
  server deadline (`Config::enroll_timeout_secs()`) plus a 15-second margin.
- **Bare D-Bus AccessDenied errors** (#89): when the system bus policy rejects
  a caller that is not root or in the `facelock` group, the CLI now appends an
  actionable hint (add user to group, re-login, or re-run setup) instead of a
  bare "AccessDenied". Superseded by ADR 010: the hint is now always "root
  required" and there is no group.
- **Uninstall left `pam_facelock.so` lines behind**: the Arch, RPM and
  `just uninstall` cleanup paths stripped the facelock line from three
  `/etc/pam.d` files (`sudo`, `polkit-1`, `hyprlock`) while `facelock setup`
  offers eight, so `swaylock`, `kscreenlocker_greet`, `gdm-password`, `sddm`
  and `lightdm` kept a line pointing at a module that was no longer installed.
  All four packaging paths now share one list per file covering every service
  setup offers, plus the ones `--service` gates behind a confirmation
  (`system-auth`, `login`, `sshd`). A test in `facelock-cli` reads the
  packaging files and fails if a new PAM candidate is not covered.
- **Debian/Ubuntu uninstall removed no PAM lines at all**: `debian/prerm`
  delegated everything to `pam-auth-update --remove facelock`, which only
  manages the shared `common-auth` profile — it knows nothing about the direct
  `/etc/pam.d/<service>` edits `facelock setup --pam` makes, so *every*
  configured service kept its line. `prerm` now runs the same explicit removal
  loop as the other packagers in addition to the `pam-auth-update` call.

- **Odd-width NV12 frames no longer panic**: a UV row is `2 * ceil(width/2)` bytes,
  not `width`, so the short-buffer guard accepted frames that the row indexing then
  ran past the end of.
- **Padded camera strides are rejected at open**: devices whose `bytesperline`
  exceeds the row size for the negotiated format (ISP hardware) now fail with an
  error naming both values instead of decoding sheared frames.
- **Quirk `format_preference` naming an undecodable format is ignored** with a
  warning, rather than winning negotiation and failing every subsequent capture.
- **IR cameras excluded from auto-detection for lacking a decodable format** are
  logged with their path and advertised formats, so syslog explains a later
  "not an IR camera" failure.

### Security

- **`CAP_CHOWN` added to the daemon's capability bounding set, for startup
  only** (#137). Root without `CAP_CHOWN` cannot `chown(2)` at all, and two
  startup steps need it on an *upgraded* install: `ensure_state_layout` (which
  chowns `/var/lib/facelock` to `root:facelock`, and whose failure is fatal —
  the daemon exits 1) and the enrollment-marker reconcile (which chowns each
  marker to its user). The capability is deliberately **not** ambient and is
  cleared by the in-process capability drop as soon as those two steps are done
  and before the daemon creates its first thread, so no thread of the process
  holds it while anyone is being authenticated and no exec'd child inherits it.
  `systemd-analyze security` moves 2.6 → 2.8 (still OK); it scores the bounding
  set and cannot see the in-process drop. Since ADR 010 the target is
  `root:root`.
- **The daemon's capability drop is now verified, and refuses to serve if it did
  not happen.** It used to log `failed to drop capabilities (continuing)` and
  carry on, which made "narrowed after initialization" a best-effort claim. That
  was defensible while the dropped set held nothing the security model had
  promised to remove; with `CAP_CHOWN` in the bounding set (above) it is not — a
  failed drop would leave the daemon serving every authentication with
  `chown(2)` in reach. The daemon now reads its capabilities back with `capget`
  and exits before answering a single call if anything beyond
  `CAP_SETUID`+`CAP_SETGID` survived. Refusing is not a lockout: PAM degrades to
  the password exactly as it does when the daemon is not running. A daemon
  started under a *narrower* set than the shipped unit grants still runs (it
  holds nothing extra) with a warning that notifications may not work — the drop
  requests only capabilities the process already has, so a narrower start can no
  longer fail the drop wholesale and trip the refusal.
- **The capability drop happens while the daemon is still single-threaded.**
  Capabilities and `PR_SET_NO_NEW_PRIVS` are per-*thread* and inherited only
  forwards, so a drop performed once the ONNX Runtime pools and the tokio
  runtime existed narrowed only the calling thread — leaving every thread that
  actually serves `Authenticate` holding `CAP_CHOWN`, with a per-thread `capget`
  read-back that could not see it. `test/pkg-validate.sh` now walks
  `/proc/<pid>/task/*/status` on the running daemon and asserts `CAP_CHOWN` is
  clear on every thread.

- **Y16 8-bit scaling is pinned at camera open**: the bit-depth shift is derived
  once and reused for the whole session. Deriving it per frame was contrast
  normalization upstream of the IR texture check — it moved the scale
  `security.ir_texture_min_stddev` is calibrated against in response to scene
  illumination, and a single saturated pixel blacked out whole frames. The shift
  comes from the new quirk key `y16_bit_depth` when a device declares one
  (hardware truth, no frame inspected); otherwise it is calibrated from the peak
  of a short burst of frames rather than a single frame, so a dark pre-AGC frame
  at open no longer pins an 8-bit scale that clips the rest of the session.
  "The session" is the lifetime of one open camera: the daemon's warm camera
  hold keeps the scale it pinned, and a reopen recalibrates — no scale is
  carried across a reopen onto a device that did not produce it. On IR hardware
  the burst is emitter-LED-on time inside `Camera::open` (bounded by one
  second); declaring `y16_bit_depth` skips it.

## [0.1.4] - 2026-05-31

Robustness pass: setup-wizard UX improvements, an `facelock-bin` AUR package, and a sweep of workspace dependency bumps with the cross-cutting API-change fixes they required.

### Added

- **AUR `facelock-bin` package**: prebuilt-binary AUR variant alongside source-build `facelock` and VCS `facelock-git`. The release workflow now publishes all three on tag push.
- **Setup wizard: PAM edit preview and confirmation**: setup shows exactly which lines will be added to each PAM service file (with top-of-file fallback described) and asks for confirmation before mutating anything on disk.
- **Setup wizard: display-manager and screen-locker detection**: setup detects installed display managers and lockers (Hyprland, sway, SDDM, GDM, etc.) and offers per-service opt-in via multi-select. SDDM and GDM integrations are marked experimental.

### Changed

- **Workspace dependency bumps**: clap 4.5→4.6, dialoguer 0.11→0.12, indicatif 0.17→0.18, ndarray 0.16→0.17, nix 0.29→0.31, rand 0.9→0.10, reqwest 0.12→0.13, rusqlite 0.32→0.40, sha2 0.10→0.11, signal-hook 0.3→0.4, tokio 1.50→1.52, tracing-subscriber 0.3.22→0.3.23, wayland-client 0.31.13→0.31.14, xkbcommon 0.8→0.9, plus libc, serde_json, toml, and `trixie` container patches.
- **CI: GitHub Release published via PAT**: switched from `GITHUB_TOKEN` to a PAT so Packit's release-event listener fires reliably for COPR builds.
- **GitHub Actions versions**: `actions/checkout@v6`, `actions/upload-pages-artifact@v5`, `actions/deploy-pages@v5`, `softprops/action-gh-release@v3`, `cachix/install-nix-action@v31`, plus consolidated GitHub artifact actions.

### Fixed

- **Uninstall cleanup**: closed gaps across all four uninstall paths (deb purge, rpm erase, makepkg/AUR remove, `just uninstall`). The installed systemd unit name is now captured *before* deletion, and user-data handling messaging is clearer.
- **`facelock clear` requires root before prompting**, not after — previously asked the confirmation question and then errored on missing privileges.
- **AUR publish script**: distinguishes "AUR repo doesn't exist yet" from other clone failures (previously masked real errors with a fresh `git init`), and derives the GitHub repo name from `GITHUB_REPOSITORY` instead of hardcoding it.
- **`facelock-git` AUR version display**: bumped the static `pkgver=` (used by AUR's web page because `pkgver()` doesn't run in the SRCINFO container) and extended `just release` to keep it in sync going forward.
- **Cross-version dependency portability**: SHA256 hex encoding rewritten by-byte (in both `facelock-face::models` and `facelock-cli::commands::setup`) so the code compiles against `sha2 0.10`'s `GenericArray` and `0.11`'s `hybrid_array::Array`. SQLite timestamps in `facelock-store` cast through `i64` to avoid rusqlite 0.40 type mismatches. CLI setup wizard uses `&options[..]` so `Select::items` is correct under both `dialoguer 0.11` (slice arg) and `0.12` (generic `IntoIterator` arg, clippy-clean). TPM crate migrated to `rand` 0.10's `thread_rng → rng` and `RngCore → Rng` rename.

## [0.1.3] - 2026-05-20

### Changed

- **COPR publishing**: migrated from the GitHub webhook to [Packit](https://packit.dev). The `publish-copr` job and the `COPR_WEBHOOK_URL` secret are removed; COPR builds are now driven by `.packit.yaml` on GitHub Release publish. The COPR RPM is built from source and depends on Fedora's system `onnxruntime` package.

### Fixed

- **ONNX Runtime API floor**: lowered the `ort` crate API feature from `api-24` to `api-20`. `api-24` required ONNX Runtime 1.24+ at runtime, which no shipped or bundled runtime provided (the bundled CPU ORT and Fedora's `onnxruntime` are 1.20.x–1.22.x), so face inference would fail to initialize. facelock uses only baseline ONNX Runtime APIs, so `api-20` loses no functionality.

## [0.1.2] - 2026-05-17

Patch release fixing the AUR publish job. No runtime code changes.

### Fixed

- **AUR publish**: `publish-aur.sh` now runs `makepkg --printsrcinfo` as a non-root `builder` user inside the Arch container (makepkg refuses to run as root). Host-runner ownership is restored after the container exits.

## [0.1.1] - 2026-05-17

Patch release fixing publish-job failures from the v0.1.0 release workflow run. No runtime code changes.

### Fixed

- **APT publish**: `publish-apt.sh` no longer exits when a `gpg-agent` is already running on the GitHub runner — falls back to `gpgconf --launch gpg-agent`
- **COPR publish**: added `.copr/Makefile` so the COPR `make srpm` build method can produce the source RPM from `dist/facelock.spec` via `git archive` + `rpmbuild -bs`

## [0.1.0] - 2026-05-17

Initial open-source release.

### Added

- **Core pipeline**: SCRFD face detection + ArcFace 512-dim embedding with ONNX Runtime
- **PAM module**: Thin cdylib with D-Bus daemon and oneshot subprocess modes
- **Daemon**: Persistent process with model caching, ~200ms warm auth latency
- **CLI**: Unified `facelock` binary — setup, enroll, test, preview, bench, audit, and more
- **Anti-spoofing**: IR camera enforcement, frame variance checks, landmark liveness detection
- **D-Bus**: System bus interface (`org.facelock.Daemon`) with deny-all policy and caller UID verification
- **GPU**: Runtime-selectable execution providers (CPU, CUDA, ROCm, OpenVINO) via `execution_provider` config — no compile-time flags
- **Setup wizard**: Interactive model-quality and inference-device selection, streaming download progress bar, only downloads the models actually selected in config
- **Status command**: Reports inference provider and ORT library location, enrolled face count for the current user, security posture (IR enforcement, liveness, `min_auth_frames`), and notification state (`73a5c00`)
- **Models**: Self-hosted ONNX assets distributed via GitHub release downloads (no third-party model fetches in the auth path)
- **Packaging**: deb, rpm, PKGBUILD (`facelock` and `facelock-git`), Nix flake, signed APT repository with two channels — `main` (TPM-enabled, Debian trixie+ / Ubuntu 25.04+) and `legacy` (non-TPM, Debian bookworm / Ubuntu 24.04) — systemd/D-Bus activation, OpenRC/runit/s6 (`c70999b`)
- **CI/CD**: Build/test/lint pipeline, TPM tests via swtpm, container PAM smoke tests, end-to-end `.deb` and `.rpm` package install validation
- **Documentation**: mdBook, man pages, ADRs, security posture assessment, threat model

### Security

- **Constant-time matching**: Embedding comparison via `subtle` crate to prevent timing side-channels
- **Encryption at rest**: AES-256-GCM software encryption for stored face embeddings
- **TPM key sealing**: Optional TPM-backed key protection for the encryption key
- **Model integrity**: SHA256 verification of ONNX model files at load time
- **Rate limiting**: 5 auth attempts per user per 60 seconds (default), enforced in daemon
- **D-Bus authorization**: Daemon verifies caller UID via `GetConnectionUnixUser` before executing methods
- **Enrollment restriction**: Root-required enrollment enforced in auth paths (`c01a655`)
- **PAM env hardening**: Hardened PAM environment handling to prevent injection (`c01a655`)
- **systemd hardening**: `ProtectSystem=strict`, `NoNewPrivileges`, `InaccessiblePaths`, and related service restrictions

### Fixed

- **PAM install output**: Conditional install messages — suppressed when PAM entry already present (`c12a970`)
- **PAM uninstall**: Uninstall now removes entries from all relevant PAM services, not just the primary one (`c12a970`)

[0.1.3]: https://github.com/tyvsmith/facelock/releases/tag/v0.1.3
[0.1.0]: https://github.com/tyvsmith/facelock/releases/tag/v0.1.0
