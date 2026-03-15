# ADR 005: Layered Passive Anti-Spoofing Defense

## Status

Accepted

## Date

2026-03-14

## Context

Photo and video replay attacks are the primary threat against face
authentication systems. An attacker holding a printed photo or playing a video
of the enrolled user's face in front of the camera can bypass naive
face-matching systems. Facelock must defend against these attacks without
degrading the authentication experience.

The threat model assumes an attacker with:

- A high-resolution photo of the enrolled user (readily available from social
  media).
- A video of the enrolled user displayed on a phone or tablet.
- Physical access to the machine's camera.

Consumer-grade depth cameras (Intel RealSense, Apple TrueDepth) provide strong
anti-spoofing but are not widely available on Linux laptops. Most Linux-supported
hardware ships with standard RGB cameras, and a subset includes infrared (IR)
cameras (common on business laptops with Windows Hello support).

## Decision

Implement a three-layer passive anti-spoofing defense, all enabled by default:

1. **IR camera enforcement** (`security.require_ir = true`). The daemon
   preferentially selects an IR camera during auto-detection. IR imaging defeats
   printed photo attacks because paper and screens have fundamentally different
   IR reflectance properties compared to human skin. When no IR camera is
   available and `require_ir` is true, authentication is denied.

2. **Frame variance analysis**. During each authentication attempt, the daemon
   captures multiple frames (default: 5 over ~500ms) and computes inter-frame
   variance. Static images — whether printed photos or paused video — produce
   near-zero variance. A minimum variance threshold must be exceeded for
   authentication to proceed.

3. **Landmark liveness detection**. Facial landmark positions are tracked across
   the captured frame sequence. Natural micro-movements (eye blinks, slight head
   shifts, breathing) produce measurable landmark displacement that static and
   many video attacks cannot replicate. The detector requires minimum
   displacement above a configurable threshold.

All three layers run in the authentication path in `facelock-daemon`'s handler.
Each layer can be independently disabled via configuration for testing or
hardware-specific reasons.

## Alternatives Considered

### Active challenge-response

Prompt the user to perform an action: nod, blink, turn their head, or press a
hotkey. Rejected for several reasons:

- **Poor UX.** Face authentication should be passive and fast. Requiring user
  action negates the convenience advantage over password entry.
- **Latency.** Active challenges add 2-5 seconds to each authentication attempt.
- **Accessibility.** Users with limited mobility may be unable to perform
  physical challenges.
- **Bypassable.** Sophisticated video attacks can replay recorded challenge
  responses if the challenge set is small.

### Single-layer IR only

Rely exclusively on IR camera enforcement. Rejected because:

- IR alone does not defend against video replay on an IR-transparent display.
- Provides no defense when `require_ir` is disabled (e.g., on hardware without
  an IR camera).
- Defense in depth is a core security principle; a single layer is insufficient.

### Depth camera requirement

Require a depth-sensing camera (RealSense, ToF sensor). Rejected because depth
cameras are rare on Linux laptops. This would exclude the majority of target
hardware. Depth support may be added as an optional fourth layer in the future.

## Consequences

- **Strong defaults.** All three layers active out of the box. Attackers must
  defeat IR filtering, frame variance, and landmark analysis simultaneously.
- **Hardware flexibility.** Each layer degrades gracefully. On RGB-only hardware,
  `require_ir` can be set to false, and the remaining two layers still provide
  meaningful protection.
- **Independent disabling.** Each layer can be toggled individually
  (`security.require_ir`, `security.require_frame_variance`,
  `security.require_liveness`), enabling incremental testing and
  hardware-specific tuning.
- **Latency budget.** The multi-frame capture adds ~500ms to authentication.
  This is within the 2-second target for the full auth path.
- **False rejection risk.** Aggressive thresholds may reject legitimate users
  who are very still. Default thresholds are tuned conservatively, and all are
  configurable.

## References

- `docs/security.md` — Security model and threat analysis
- `crates/facelock-daemon/src/handler.rs` — Authentication handler with liveness checks
- `crates/facelock-camera/` — IR camera detection and frame capture
