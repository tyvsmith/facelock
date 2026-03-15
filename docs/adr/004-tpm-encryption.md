# ADR 004: Software AES-256-GCM with Optional TPM Key Sealing

## Status

Accepted

## Date

2026-03-14

## Context

Face embeddings stored by Facelock are 512-dimensional float32 vectors, each
requiring 2048 bytes of raw storage (more with metadata framing). These
embeddings are biometric data that must be encrypted at rest to prevent
extraction and misuse.

TPM 2.0 provides hardware-backed key sealing with PCR-based access policies,
making it an attractive option for protecting encryption keys. However, TPM 2.0's
`TPM2_Seal` command is limited to approximately 256 bytes of sealed data,
depending on the hash algorithm and key size. A single face embedding exceeds
this limit by an order of magnitude, and users may enroll multiple faces.

Direct TPM sealing of each embedding would require chunking data across multiple
sealed objects, introducing complexity and multiplying TPM round-trips during
authentication — unacceptable for a sub-second auth target.

Not all target systems have TPM hardware. Laptops generally do; many desktops
and all VMs do not.

## Decision

Use software AES-256-GCM for bulk encryption of face embeddings. The symmetric
key is generated in software and stored locally. When TPM hardware is available
and the user opts in, the symmetric key is sealed to the TPM bound to the
current PCR state (PCRs 0, 2, 7 by default — firmware, kernel, and Secure Boot
policy).

The encryption flow:

1. Generate a 256-bit AES key in software.
2. Encrypt each embedding with AES-256-GCM (unique nonce per record).
3. Optionally seal the AES key to the TPM via `TPM2_Seal`.
4. Store the sealed key blob alongside the encrypted database.

On authentication, the flow reverses: unseal the key (or read it directly if no
TPM), then decrypt embeddings in software.

This is implemented in the `facelock-tpm` crate, which provides a unified
`KeyProvider` trait with `SoftwareKeyProvider` and `TpmKeyProvider`
implementations.

## Alternatives Considered

### Direct TPM sealing of embeddings

Seal each embedding directly into the TPM. Rejected because the TPM 2.0 seal
limit (~256 bytes) is far smaller than a single embedding (2048 bytes). Chunking
across multiple sealed objects would require 8+ TPM operations per embedding per
auth attempt, adding hundreds of milliseconds of latency and significant
implementation complexity.

### Pure software encryption only

Use AES-256-GCM with a software-managed key and no TPM integration. This is the
default configuration and works on all hardware. The TPM integration is strictly
opt-in. This alternative is not so much rejected as it is the baseline — TPM
sealing is layered on top.

### TPM-backed HMAC key for key derivation

Use a TPM-resident HMAC key to derive the AES key rather than sealing it.
Rejected because HMAC-based derivation requires the TPM on every key load (no
offline fallback), and the derivation input must itself be stored securely,
creating a circular problem.

## Consequences

- **Works everywhere by default.** No TPM hardware required for the base
  encryption guarantee.
- **TPM adds hardware binding.** When enabled, the encryption key cannot be
  extracted or used on a different machine or after firmware changes.
- **PCR brittleness.** Firmware or kernel updates change PCR values, which
  invalidates the sealed key. The `facelock tpm reseal` command handles
  re-sealing after legitimate updates. Documentation must make this clear.
- **Two code paths.** The `KeyProvider` trait abstracts the difference, but both
  paths need testing. The `facelock-tpm` crate includes mock TPM support for CI.

## References

- `crates/facelock-tpm/` — TPM key sealing and software AES-256-GCM implementation
- [TCG TPM 2.0 Library Specification](https://trustedcomputinggroup.org/resource/tpm-library-specification/)
- [AES-GCM (NIST SP 800-38D)](https://csrc.nist.gov/publications/detail/sp/800-38d/final)
