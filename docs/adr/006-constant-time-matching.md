# ADR 006: Constant-Time Embedding Comparison via `subtle`

## Status

Accepted

## Date

2026-03-14

## Context

Face authentication compares a probe embedding (from the camera) against all
stored embeddings for the enrolling user. The standard approach — iterate
through stored embeddings and return early on the first match above the
similarity threshold — introduces timing side channels.

An attacker who can measure authentication timing (e.g., by observing PAM
response latency over many attempts) can infer:

- **How many embeddings are stored** for a user (more embeddings = longer
  comparison time).
- **Which stored embedding matched**, by correlating probe variations with
  response time changes.
- **Similarity proximity**, since floating-point comparison branches can vary
  with operand values.

While exploiting these side channels requires many observations and precise
timing, the attack surface is unnecessary. Constant-time comparison eliminates
it entirely with negligible performance cost.

## Decision

Use the `subtle` crate's `ConditionallySelectable` trait to implement
branchless embedding comparison. The matching algorithm:

1. Always compares the probe against **every** stored embedding, regardless of
   whether a match is found early.
2. Uses `subtle::ConditionallySelectable` to accumulate the best-match result
   without branching on comparison outcomes.
3. The final match/no-match decision is made only after all embeddings have been
   processed.

The implementation resides in `facelock-core`, where embedding comparison is a
core type operation. The `subtle` crate is already a dependency of
`facelock-core`.

## Alternatives Considered

### Standard comparison with artificial delay

Iterate normally with early exit, then pad the response time to a fixed duration
(e.g., always wait 200ms regardless of when a match is found). Rejected because:

- Artificial delays are imprecise. OS scheduling jitter, system load, and timer
  resolution create measurable variation around the target delay.
- The fixed delay must be set conservatively high to accommodate the worst case,
  penalizing every authentication attempt.
- An attacker with sufficient observations can still extract signal from the
  noise around the artificial delay.

### Always iterate without constant-time primitives

Compare all embeddings without early exit, but use standard floating-point
operations and branching for the best-match selection. Rejected because:

- The compiler may optimize branches into conditional moves or vice versa
  unpredictably across optimization levels and target architectures.
- Branch prediction side channels (Spectre-class) could leak comparison
  outcomes even without early exit.
- The `subtle` crate explicitly prevents these optimizations, providing a
  stronger guarantee.

## Consequences

- **Negligible overhead.** Constant-time selection adds roughly 1-2 microseconds
  per embedding comparison. With typical enrollment counts (1-5 embeddings per
  user), the total overhead is under 10 microseconds — invisible against the
  ~500ms camera capture and ~50ms inference times.
- **Timing uniformity.** Authentication response time depends only on the number
  of stored embeddings (public information bounded by configuration), not on
  which embedding matched or the similarity scores.
- **Compiler cooperation.** The `subtle` crate uses inline assembly barriers to
  prevent the compiler from introducing branches. This is architecture-specific
  but covers x86_64 and aarch64, which are the target platforms.
- **Auditability.** Using a well-known crate (`subtle`, maintained by the
  dalek-cryptography team) for constant-time operations is easier to audit than
  hand-rolled branchless code.

## References

- `crates/facelock-core/` — Embedding comparison implementation
- [`subtle` crate documentation](https://docs.rs/subtle/)
- [A Lesson in Timing Attacks](https://codahale.com/a-lesson-in-timing-attacks/)
