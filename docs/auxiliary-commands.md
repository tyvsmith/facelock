# Auxiliary Commands

The workspace builds two executables outside the unified `facelock` command
tree. They do not inherit `facelock`'s global flags, privilege dispatcher, or
output contracts.

## `facelock-bench`

`facelock-bench` is a source-only benchmark binary. Current Debian, RPM, Arch,
and release artifact paths do not install or publish it. Build it from a
checkout and run the resulting path explicitly:

```bash
cargo build --release --bin facelock-bench
target/release/facelock-bench camera-reopen --iterations 10
```

It has exactly eight verbs:

| Command | Measurement | Additional prerequisite |
|---------|-------------|-------------------------|
| `facelock-bench cold-auth` | config, model load, camera open, first authentication | plaintext enrolled templates |
| `facelock-bench warm-auth` | ten captures and matches with models loaded | plaintext enrolled templates |
| `facelock-bench preview` | camera capture and face detection | camera and models |
| `facelock-bench enrollment` | five capture-and-embed snapshots, without storing them | camera and models |
| `facelock-bench model-load` | detector and embedder load | models |
| `facelock-bench calibrate` | pairwise genuine/impostor threshold sweep | plaintext enrolled templates for at least two users |
| `facelock-bench camera-reopen` | open, STREAMON, warm-up and total reopen latency | camera; optional `--iterations <N>`, default `5` |
| `facelock-bench report` | combined environment and benchmark report | camera, models and plaintext enrolled templates |

There are no `--config`, `--quiet`, `--verbose`, `--json`, or `--user` options
and no automatic root gate. `FACELOCK_CONFIG` selects the configuration only
for a non-root process; effective-UID-0 processes ignore it and read the fixed
default. Access to the default root-owned database and many camera devices may
still require privileges.

This older standalone path reads only plaintext embedding rows. It cannot
benchmark the current default encrypted (`keyfile`) store. Do not turn off
encryption on a real enrollment merely to use it; prefer the supported
`sudo facelock bench ...` commands, which understand the configured store and
apply a consistent root gate.

Where a verb needs a user, `facelock-bench` reads `USER`, then `LOGNAME`, then
uses the literal name `unknown`. It does not use `--user`, `SUDO_USER`,
`DOAS_USER`, or a UID lookup. Running it through `sudo` therefore commonly
selects `root`, not the invoking desktop user. Measurement reports go to
stdout; diagnostics go to stderr. `RUST_LOG` controls diagnostic filtering.

## `facelock-polkit-agent`

`facelock-polkit-agent` is an experimental session service, not a CLI. It has
no options or subcommands. In particular, do **not** run it with `--help` or
`--version`: those strings are not parsed and the process will instead connect
to D-Bus and start the agent.

The binary needs all of the following:

- a working system D-Bus and polkit authority
- the user's session D-Bus
- a usable Facelock daemon and enrollment
- a real local session (`XDG_SESSION_ID`, or polkit's `auto` lookup)
- an action ID listed in `[polkit].face_eligible_actions`

It reads the ordinary configuration as the session user. If that load fails,
it uses the restrictive default allowlist containing only
`org.freedesktop.login1.lock-sessions`. `LANG` supplies the registration
locale, with `en_US.UTF-8` as the fallback.

Packages may install the executable, but they intentionally do not install an
autostart entry. A desktop-session integrator that has tested the agent can use
this shape in `~/.config/autostart/org.facelock.AuthAgent.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=Facelock polkit authentication agent
Exec=facelock-polkit-agent
OnlyShowIn=ExampleDesktop;
X-GNOME-Autostart-enabled=true
```

Replace `ExampleDesktop` with the desktop identifier or remove `OnlyShowIn`
only after testing the session's agent selection. Polkit permits one
authentication agent per session. Registering this experimental agent can
displace the desktop's password agent; when Facelock declines or fails, that
can produce a denial instead of a password dialog. Keep a recovery path and do
not deploy it as a universal replacement. The internal
`FACELOCK_POLKIT_SKIP_REGISTER` test hook is not a supported user setting.

For per-action policy and the fallback limitation, see
[`security.md`](security.md#7-polkit--sudo-face-auth-implemented). For build, test and maintenance
commands, see [`developer-commands.md`](developer-commands.md).
