# WS3: TPM 2.0 Integration — Spec

**Status:** Complete

## Changes Made

### Dependencies
- `tss-esapi = "8.0.0-alpha.2"` in workspace deps (behind `tpm` feature)

### Core error type
- Added `Tpm(String)` variant to `FacelockError`

### facelock-tpm crate
- `sealing.rs` — full TpmSealer with two compile-time variants:
  - `tpm` feature: real seal/unseal with ECC P-256 primary key, PCR policy, version-byte format
  - No feature: passthrough mode, rejects sealed blobs
- `pcr.rs` — PcrBaseline, read_current, capture_baseline, verify_against_baseline

### facelock-store
- Migration V3: `ALTER TABLE face_embeddings ADD COLUMN sealed INTEGER NOT NULL DEFAULT 0`
- New methods: get_user_embeddings_raw, add_model_raw, update_embedding_sealed, count_sealed, get_all_embeddings_raw

### facelock-daemon
- Handler gains `Option<TpmSealer>` (feature-gated), auto-init from config
- `load_user_embeddings()` transparently unseals TPM blobs

### facelock-cli
- `facelock tpm status` — device info, config, sealed/unsealed counts
- `facelock tpm seal-db` — seal all raw embeddings
- `facelock tpm unseal-db` — unseal all sealed embeddings
- `facelock tpm pcr-baseline` — display current PCR values

## Verification

```bash
cargo test --workspace --features tpm  # with swtpm running
cargo build --workspace --features tpm
```
