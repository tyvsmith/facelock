# System Contracts

Stable contracts. Do not change without updating this document.

## Binaries

| Binary | Crate | Purpose |
|--------|-------|---------|
| `facelock` | facelock-cli | Unified CLI (daemon, auth, enroll, test, setup, etc.) |
| `pam_facelock.so` | pam-facelock | PAM authentication module |
| `facelock-polkit-agent` | facelock-polkit | Polkit face authentication agent |

## CLI Subcommands

| Command | Purpose |
|---------|---------|
| `facelock setup` | Interactive setup wizard (camera, models, inference device, encryption, enrollment, PAM); also manages `facelock` group membership (creates the group if missing, adds the invoking user) |
| `facelock setup --systemd` | Install/enable systemd units |
| `facelock setup --pam` | Install PAM module to `/etc/pam.d/` |
| `facelock setup` choice flags | `--camera <PATH\|auto>`, `--models <standard\|balanced\|high>`, `--execution-provider <cpu\|cuda\|rocm\|openvino\|auto>`, `--encryption <tpm\|keyfile\|none\|auto>`. Precedence: CLI flag > config file > built-in default |
| `facelock setup` action opt-outs | `--no-pam`, `--no-systemd`, `--no-enroll` decline an action outright (and their `--pam`/`--systemd`/`--enroll` counterparts force it). Later flag wins |
| `facelock is-enrolled` | Report whether face auth is operational for a user. Exit code is the contract; no daemon activation, no camera. Requires `facelock` group membership to answer `enrolled` — a caller outside the group reports `not-enrolled`, which is correct: the group is required to reach the daemon at all |
| `facelock enroll` | Capture and store a face |
| `facelock test` | Test face recognition |
| `facelock list` | List enrolled face models |
| `facelock remove <id>` | Remove a specific model |
| `facelock clear` | Remove all models for a user |
| `facelock preview` | Live camera preview |
| `facelock devices` | List V4L2 cameras |
| `facelock status` | Check system status |
| `facelock config` | Show/edit configuration |
| `facelock daemon` | Run persistent daemon |
| `facelock auth --user X` | One-shot auth (PAM helper) |
| `facelock tpm status` | TPM status |
| `facelock hyprlock enable\|disable\|status` | Manage hyprlock lock-screen integration (user, no root); `enable` accepts `--no-icon` to skip the cosmetic face glyph |
| `facelock encrypt` | Encrypt face database |
| `facelock decrypt` | Decrypt face database |
| `facelock reseal` | Re-seal the TPM AES key under current PCRs (recovery after a firmware/kernel change) |
| `facelock tpm seal-key` / `unseal-key` | Migrate keyfile↔tpm key protection |
| `facelock tpm unseal-check` | Read-only: verify the sealed key still unseals (PCR policy satisfied) |
| `facelock audit` | View audit log |
| `facelock bench` | Benchmarks |
| `facelock restart` | Restart daemon |

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

`--if-present` requires `--remove` (and therefore `--pam`). It changes only a
missing target service file from an error into a successful no-op; read, parse
and write failures remain fatal, and `--remove` without the flag retains its
historical missing-file error.

Supplying a choice flag suppresses the corresponding wizard step. `auto` means
"re-derive from hardware", **not** "use the default" — omitting the flag already
gives the default. Under `--non-interactive`, an unresolvable choice is an error,
never a prompt.

### CLI Privilege Model (DEC-6)

The CLI is root by default: every subcommand requires root except the four
listed below, which are unprivileged by design, not by omission.

| Command | Why unprivileged |
|---------|-------------------|
| `facelock is-enrolled` | Answers from the caller's own `0600` marker file; the unprivileged integration point (see Exit Codes above). Never probes D-Bus |
| `facelock hyprlock …` | Edits the user's own dotfile — root would write root-owned files into `$HOME`, which is wrong, not just unnecessary |
| `facelock config` (display, no `--edit`) | Reads a `0644` file |
| `--help`, `--version` | — |

Every other command requires root. Two escalation behaviors apply, and each
command uses exactly one:

- **Interactive prompt.** `setup`, `enroll`, `test`, `preview`, `bench`,
  `encrypt`, `decrypt`, `tpm`, `reseal`, `restart`, `config --edit`, `remove`,
  `clear`, `list`, `status`, `devices`. Run as non-root with a TTY attached,
  these ask `Root required. Re-run with sudo? [Y/n]` and re-exec via `sudo`
  on yes. Run as non-root with no TTY (scripted, piped, or closed stdin),
  they hard-error instead — `Root required.\n  Run: sudo facelock <cmd>` —
  rather than hang waiting for input that will never arrive
  (`ipc_client::require_root`).
- **Hard error only.** `facelock audit` (and `facelock daemon`, whose own
  root check predates this table) never offer the interactive prompt at all,
  even with a TTY attached — both are typically invoked non-interactively or
  as a long-running service, where a stray confirmation prompt is a hang, not
  a convenience (`ipc_client::require_root_scripted`).

`facelock auth` is not user-facing — PAM spawns it directly, and it is not
part of this table.

**Ordering guarantee (C6).** Every command that prompts for confirmation or
runs an interactive question runs its root check **first**, before that
prompt or any other output or side effect. `remove` and `clear` both ask a
Y/N confirmation before deleting a face model; historically `remove`'s root
check ran *after* that confirmation, so a facelock-group member (who lacks
root) would confirm a destructive action and only then discover it was
refused — this is fixed. The same ordering applies to `status`, `devices`,
`preview`, `test`, `audit`, `bench`, and `config --edit`: the root check is
the first statement in each command's entry point, before `Config::load()`
or any `println!`.

**AccessDenied hint.** A D-Bus `AccessDenied` reply carries an actionable
hint (`ipc_client::add_access_denied_hint`) that distinguishes two causes: the
daemon's own `require_root` rejection (most methods now, since almost every
D-Bus method is root-only — see IPC Protocol below) gets a root-specific
hint; a bus-policy rejection (caller is neither root nor in the `facelock`
group) gets the group-membership hint. Telling a root-only rejection to
"join the facelock group" is wrong — joining the group does not grant root.

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

Default stdout is `enrolled` / `not-enrolled` — the state word, as `systemctl
is-active` prints `active`. `--quiet` suppresses stdout and leaves only the exit
code. `--json` emits `{"enrolled": bool, "models": N, "updated": "<ISO8601>"}`.

`is-enrolled` answers from `/var/lib/facelock/enrolled/<user>` alone. It never
activates the daemon over D-Bus, never opens a camera, and never reads the
database — so it is safe to call repeatedly from a lock screen as an
unprivileged user. The marker is a hint that can drift from the database; **PAM
at auth time remains authoritative** and nothing in the auth path consults it.

Markers are written by `enroll`, `remove` and `clear`, and converged from the
database by `setup`, by daemon startup, and by the one-shot `facelock auth` path
for the user being authenticated. Convergence re-derives every marker from the
database, so it is idempotent and there is no migration state to keep. An
install upgraded from a release that predates markers backfills itself on the
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

### facelock auth Exit Codes

| Code | Meaning | PAM Code |
|------|---------|----------|
| 0 | Face matched | PAM_SUCCESS |
| 1 | No match / timeout / dark | PAM_AUTH_ERR |
| 2 | Error / no enrolled faces | PAM_IGNORE |

## Filesystem Paths

| Path | Owner | Mode | Purpose |
|------|-------|------|---------|
| `/etc/facelock/config.toml` | root:root | 644 | Configuration |
| `/var/lib/facelock/` | root:facelock | 710 | State dir. Traverse-only for the `facelock` group, nothing for anyone else: a group member can open a path it knows by name but cannot list the directory, and users outside the group reach nothing below this point |
| `/var/lib/facelock/facelock.db` | root:root | 600 | Face embeddings. Read by the daemon (root) only; the `facelock` group requests authentication through the daemon, it does not read templates |
| `/var/lib/facelock/models/` | root:root | 755 | ONNX models — public, SHA256-verified downloads; the `710` parent is the gate |
| `/var/lib/facelock/enrolled/` | root:facelock | 710 | Enrollment markers; group-traversable but not listable |
| `/var/lib/facelock/enrolled/<user>` | \<user\>:\<user\> | 600 | `{"models": N, "updated": "<ISO8601>"}` — a hint for `is-enrolled`, never authoritative |
| `/var/log/facelock/` | root:root | 700 | Log dir — per-user auth history and raw face snapshots are root-only |
| `/var/log/facelock/audit.jsonl` | root:root | 600 | Structured audit log |
| `/var/log/facelock/snapshots/` | root:root | 700 | Auth snapshots (raw face images) |
| `/usr/bin/facelock` | root:root | 755 | CLI binary |
| `/lib/security/pam_facelock.so` | root:root | 755 | PAM module |

All paths overridable via config. `FACELOCK_CONFIG` is honored for unprivileged processes, but privileged PAM/root auth flows ignore the environment and use either an explicit `--config` path or `/etc/facelock/config.toml`.
Runtime-created DB sidecars (`-wal`, `-shm`), audit logs, and snapshots are created with explicit restrictive modes. The packaged systemd unit also sets `UMask=0027`.

#### One gate at the top

The state directory is `0710 root:facelock`: no permission bits for "other"
at all, and traverse-only for the `facelock` group. That single gate is what
protects everything below it — a local user outside the group cannot reach
the database, the markers, or even the world-readable models, whatever their
own modes say. Every entry below the gate is still locked down in its own
right (`0600` database, `0710` markers directory) as defense in depth;
`models/` is the one entry that carries "other" bits of its own, because its
contents are public, SHA256-verified downloads.

The `facelock` group is a **D-Bus access grant, not a file-read grant**: a
member can request authentication through the daemon and open its own
enrollment marker by name, but cannot list the state directory or read the
database. D-Bus is therefore required for user-run screen lockers
(hyprlock/swaylock) — their PAM stack runs as the user, and no group
membership makes the `0600 root:root` database or encryption key readable.
Root-invoked PAM (`sudo`, `login`, `sshd`) can also use the oneshot fallback,
which reads the files directly as root.

Known residual: a group member can `stat` a path it can guess by name —
`facelock.db` (size, mtime) or `enrolled/<user>` (existence) — because
traversal permits exactly that. Closing it would mean denying the group the
traversal that `is-enrolled` and model loading depend on. Accepted.

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
`facelock reseal` re-seals the key under the current PCRs (recovery path).

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
   has an IR-like format (GREY/Y16 or the quirk's `format_preference`), only the
   format-bearing node(s) are IR
4. Prefer a quirks-confirmed IR node with a native IR format, then any
   quirks-confirmed IR node, then an evidence-classified IR node (breaking ties
   toward one whose name also carries an `ir`/`infrared` token — a hint only,
   never a promotion of a node that lacks format evidence)
5. Fall back to first available device

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
  `Ping`, `Shutdown` — is root only. The bus policy
  (`dbus/org.facelock.Daemon.conf`) stays interface-scoped (grants send
  access to root and the `facelock` group for the whole interface, so adding
  a method needs no policy edit); the per-method root/user-scoped decision is
  the in-daemon check on the caller UID from `GetConnectionUnixUser`, keyed
  by a table-driven scope (`authorize_method` in `facelock_daemon::server`)
  so a new method is root-only by default until deliberately opened up.

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
  by default; only root and members of the `facelock` group may receive it.

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
an IR-like format, only the format-bearing node(s) classify IR (see
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
