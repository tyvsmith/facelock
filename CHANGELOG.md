# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
  reminder is printed.
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

### Changed

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
  rationale.
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

### Fixed

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
  bare "AccessDenied".
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
  set and cannot see the in-process drop.
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
