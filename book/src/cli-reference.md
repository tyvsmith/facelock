# CLI Reference

All commands are subcommands of the `facelock` binary.

## facelock setup

Download models and create directories.

```bash
facelock setup                          # download models
facelock setup --systemd                # install systemd units
facelock setup --systemd --disable      # disable systemd units
facelock setup --pam                    # install to /etc/pam.d/sudo
facelock setup --pam --service login    # install to specific service
facelock setup --pam --remove           # remove PAM line
facelock setup --pam --service sshd -y  # skip confirmation for sensitive services
```

## facelock enroll

Capture and store a face model.

```bash
facelock enroll                         # current user, auto-label
facelock enroll --user alice            # specific user
facelock enroll --label "office"        # specific label
```

Captures 3-10 frames over ~15 seconds. Requires exactly one face per frame. Re-enrolling with the same label replaces the previous model.

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
facelock preview --text-only            # JSON output to stdout
facelock preview --user alice           # match against specific user
```

Text-only mode outputs one JSON object per frame:
```json
{"frame":1,"fps":15.2,"width":640,"height":480,"recognized":1,"unrecognized":0,"faces":[...]}
```

## facelock devices

List available V4L2 video capture devices.

```bash
facelock devices
```

Shows device path, name, driver, formats, resolutions, and IR status.

## facelock status

Check system status -- config, daemon, camera, models.

```bash
facelock status
```

## facelock config

Show or edit the configuration file.

```bash
facelock config                         # show config path and contents
facelock config --edit                  # open in $EDITOR
```

## facelock daemon

Run the persistent authentication daemon.

```bash
facelock daemon                         # use default config
facelock daemon --config /path/to/config.toml
```

Normally managed by systemd, not run manually.

## facelock auth

One-shot authentication. Used by the PAM module in oneshot mode.

```bash
facelock auth --user alice              # authenticate
facelock auth --user alice --config /etc/facelock/config.toml
```

Exit codes: 0 = matched, 1 = no match, 2 = error.

## facelock tpm status

Report TPM availability and configuration.

```bash
facelock tpm status
```

## facelock bench

Benchmark and calibration tools.

```bash
facelock bench cold-auth                # cold start authentication latency
facelock bench warm-auth                # warm authentication latency
facelock bench model-load               # model loading time
facelock bench report                   # full benchmark report
```

## User Resolution

For commands that accept `--user`:
1. Explicit `--user` flag (highest priority)
2. `SUDO_USER` environment variable
3. `DOAS_USER` environment variable
4. Current user (`$USER` or `getpwuid`)

## Environment Variables

| Variable | Purpose |
|----------|---------|
| `FACELOCK_CONFIG` | Override config file path |
| `RUST_LOG` | Control log verbosity (e.g., `facelock_daemon=debug`) |
