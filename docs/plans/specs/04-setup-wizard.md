# WS4: Interactive Setup Wizard — Spec

**Status:** Complete

## Changes Made

- Added `dialoguer = "0.11"` to facelock-cli deps
- Added `--non-interactive` flag to setup subcommand
- Refactored `setup.rs`:
  - `is_interactive()` — checks stdin TTY
  - `run_wizard()` — 6-step interactive flow:
    1. Camera selection (auto-select single IR, Select dialog for multiple)
    2. Model download (Confirm + progress bars)
    3. Face enrollment (Confirm + inline enroll)
    4. Test recognition (Confirm + inline test)
    5. Systemd setup (Confirm + systemd install)
    6. PAM configuration (MultiSelect for services)
  - Summary output at end
  - Graceful error handling per step

## Behavior

- `sudo facelock setup` — interactive wizard (TTY)
- `sudo facelock setup --non-interactive` — original behavior
- Non-TTY stdin — automatic fallback to non-interactive

## Verification

Run `sudo facelock setup` interactively, complete all steps.
