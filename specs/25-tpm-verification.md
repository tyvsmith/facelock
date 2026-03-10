# Spec 25: TPM Verification

## Scope

Keep TPM stubs, add status command, plan for future `tss-esapi` integration.

## Changes

### `visage tpm` subcommand

```
visage tpm status    — report TPM availability and config
visage tpm seal      — (future) seal existing database
visage tpm unseal    — (future) unseal database
```

### `visage tpm status` implementation

1. Read config `[tpm]` section
2. Check if `/dev/tpmrm0` (or configured TCTI device) exists
3. Report:
   - TPM device: found / not found
   - seal_database: enabled / disabled
   - pcr_binding: enabled / disabled
   - Status: "TPM support is not yet implemented (stubs only)"

### Documentation

Add `docs/tpm.md` explaining:
- Current status (stubs, passthrough mode)
- Planned features (embedding encryption, PCR binding)
- Hardware requirements
- How to test with `swtpm` (software TPM emulator)

### Do NOT implement

- Real TPM operations (deferred to when `tss-esapi` is validated)
- Automatic sealing on first run

## Acceptance

- `visage tpm status` runs and reports current state
- Documentation explains what works and what doesn't
- No functional change to existing TPM stubs
- No new system dependencies
