# CLI Reference

All commands are subcommands of the `visage` binary.

## visage setup

Download models and create directories.

```bash
visage setup                          # download models
visage setup --systemd                # install systemd units
visage setup --systemd --disable      # disable systemd units
visage setup --pam                    # install to /etc/pam.d/sudo
visage setup --pam --service login    # install to specific service
visage setup --pam --remove           # remove PAM line
visage setup --pam --service sshd -y  # skip confirmation for sensitive services
```

## visage enroll

Capture and store a face model.

```bash
visage enroll                         # current user, auto-label
visage enroll --user alice            # specific user
visage enroll --label "office"        # specific label
```

Captures 3-10 frames over ~15 seconds. Requires exactly one face per frame. Re-enrolling with the same label replaces the previous model.

## visage test

Test face recognition against enrolled models.

```bash
visage test                           # current user
visage test --user alice              # specific user
```

Reports match similarity and latency.

## visage list

List enrolled face models.

```bash
visage list                           # current user
visage list --user alice              # specific user
visage list --json                    # JSON output
```

## visage remove

Remove a specific face model by ID.

```bash
visage remove 3                       # remove model #3
visage remove 3 --user alice          # for specific user
visage remove 3 --yes                 # skip confirmation
```

## visage clear

Remove all face models for a user.

```bash
visage clear                          # current user
visage clear --user alice --yes       # skip confirmation
```

## visage preview

Live camera preview with face detection overlay.

```bash
visage preview                        # Wayland graphical window
visage preview --text-only            # JSON output to stdout
visage preview --user alice           # match against specific user
```

Text-only mode outputs one JSON object per frame:
```json
{"frame":1,"fps":15.2,"width":640,"height":480,"recognized":1,"unrecognized":0,"faces":[...]}
```

## visage devices

List available V4L2 video capture devices.

```bash
visage devices
```

Shows device path, name, driver, formats, resolutions, and IR status.

## visage status

Check system status — config, daemon, camera, models.

```bash
visage status
```

## visage config

Show or edit the configuration file.

```bash
visage config                         # show config path and contents
visage config --edit                  # open in $EDITOR
```

## visage daemon

Run the persistent authentication daemon.

```bash
visage daemon                         # use default config
visage daemon --config /path/to/config.toml
```

Normally managed by systemd, not run manually.

## visage auth

One-shot authentication. Used by the PAM module in oneshot mode.

```bash
visage auth --user alice              # authenticate
visage auth --user alice --config /etc/visage/config.toml
```

Exit codes: 0 = matched, 1 = no match, 2 = error.

## visage tpm status

Report TPM availability and configuration.

```bash
visage tpm status
```

## visage bench

Benchmark and calibration tools.

```bash
visage bench cold-auth                # cold start authentication latency
visage bench warm-auth                # warm authentication latency
visage bench model-load               # model loading time
visage bench report                   # full benchmark report
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
| `VISAGE_CONFIG` | Override config file path |
| `RUST_LOG` | Control log verbosity (e.g., `visage_daemon=debug`) |
