# CLI Reference

All commands are subcommands of the `facelock` binary.

## Global flags

The following flag is accepted by every subcommand (declared `global = true`):

| Flag | Description |
|------|-------------|
| `--config <PATH>` | Override the config file path. Takes precedence over `FACELOCK_CONFIG`. |

## facelock setup

Interactive setup wizard. Walks through camera selection, model quality, inference device (CPU / CUDA / ROCm / OpenVINO), model downloads, encryption, enrollment, and PAM configuration. Can also be run with flags for individual setup tasks.

```bash
facelock setup                          # interactive wizard
facelock setup --non-interactive        # run wizard without prompts
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
facelock daemon -c /path/to/config.toml # short alias for --config
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

## facelock tpm

TPM integration status and management.

### facelock tpm status

Report TPM availability and configuration.

```bash
facelock tpm status
```

### facelock tpm seal-key

Seal the AES encryption key with the TPM, migrating from a plaintext keyfile to TPM-backed storage.

```bash
facelock tpm seal-key
```

### facelock tpm unseal-key

Unseal the AES key from the TPM back to a plaintext keyfile, migrating from TPM-backed to keyfile storage.

```bash
facelock tpm unseal-key
```

### facelock tpm pcr-baseline

Display the current PCR values for all configured PCR indices.

```bash
facelock tpm pcr-baseline
```

## facelock bench

Benchmark and calibration tools.

```bash
facelock bench cold-auth                # cold start authentication latency (model load + first auth)
facelock bench warm-auth                # warm authentication latency (pre-loaded models, 10 iterations)
facelock bench preview                  # frame capture + face detection latency
facelock bench enrollment               # time to capture and embed snapshots (dry run, embeddings not stored)
facelock bench model-load               # ONNX model load time (SCRFD + ArcFace)
facelock bench calibrate                # sweep FAR/FRR thresholds and recommend optimal value
facelock bench report                   # full benchmark report
```

`cold-auth`, `warm-auth`, `calibrate`, and `report` require enrolled faces. When encryption method is `tpm`, these subcommands require root.

## facelock encrypt

Encrypt all unencrypted embeddings in the database with AES-256-GCM.

```bash
facelock encrypt                        # encrypt using the configured key
facelock encrypt --generate-key         # generate a new key file (or seal a new TPM key) WITHOUT re-encrypting embeddings
```

`--generate-key` only creates the key material. Run `facelock encrypt` (without the flag) afterwards to encrypt the embeddings.

## facelock decrypt

Decrypt all software-encrypted embeddings in the database (reverting AES-256-GCM encryption).

```bash
facelock decrypt
```

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

## facelock restart

Restart the persistent daemon. On systemd systems, runs `systemctl restart facelock-daemon.service`. Otherwise, sends a D-Bus shutdown request and the daemon restarts on next use via D-Bus activation.

Requires root. If run interactively as a non-root user, the CLI prompts to re-run via `sudo`.

```bash
facelock restart
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
| `FACELOCK_CONFIG` | Override config file path for unprivileged CLI commands. Ignored by privileged PAM/root auth flows; use `--config` there. |
| `RUST_LOG` | Control log verbosity (e.g., `facelock_daemon=debug`) |
