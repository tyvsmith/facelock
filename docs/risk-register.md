# Risk Register

## R1: Daemon Startup Latency

**Risk**: Cold daemon startup (loading ONNX models) may exceed acceptable latency for first auth after boot.

**Impact**: High -- first `sudo` after boot could take 3-5 seconds.

**Mitigation**:
- systemd service with `Type=notify` starts daemon at boot
- Socket activation: daemon pre-starts when socket is first connected
- Benchmark cold startup separately from warm auth

**Exit Criteria**: Cold auth (daemon not running) < 3 seconds. Warm auth < 450ms.

## R2: Default Model Accuracy

**Risk**: Default SCRFD 2.5G + ArcFace R50 may not be accurate enough for all lighting/camera conditions.

**Mitigation**:
- Configurable models (swap to SCRFD 10G + ArcFace R100 for higher accuracy)
- Calibration tool to sweep thresholds and measure FAR/FRR
- CLAHE preprocessing for IR cameras improves detection in low light
- Allow multiple face snapshots per user (different angles/lighting)

**Exit Criteria**: Shipped defaults backed by measured benchmark results.

## R3: V4L2 Device Diversity

**Risk**: IR cameras vary wildly in supported formats, resolutions, and pixel formats.

**Mitigation**:
- Format preference chain: GREY > YUYV > MJPG > RGB (covers most IR cameras)
- Explicit device path in config (don't auto-pick)
- `howdy devices` command to enumerate and diagnose
- Graceful error messages for unsupported formats

**Exit Criteria**: Works with at least 2 different IR camera models.

## R4: PAM Module Safety

**Risk**: A broken PAM module can lock users out of sudo, login, and su.

**Mitigation**:
- 4-tier testing strategy (unit -> hardware -> container -> VM -> host)
- PAM module is thin IPC client -- no ONNX, no camera
- Returns PAM_IGNORE on any error (graceful fallback to password)
- Container testing with pamtester before any host install
- VM testing with snapshots before host PAM changes
- Document rollback procedures

**Exit Criteria**: Full PAM test suite passes in container and VM before host install.

## R5: Permission Model Complexity

**Risk**: Camera access requires `video` group, daemon needs root for `/var/lib`, PAM runs as root.

**Mitigation**:
- Daemon runs as root (or dedicated `howdy` user with video group + directory access)
- PAM module runs in PAM context (already root)
- CLI enrollment requires root (or sudo) for daemon communication
- `HOWDY_CONFIG` env var for rootless development
- Document permission model clearly

**Exit Criteria**: Installation docs cover permissions. Dev workflow doesn't require root.

## R6: Contract Drift Across Agents

**Risk**: Parallel agents modify shared types or paths inconsistently.

**Mitigation**:
- Centralized contracts doc (`docs/contracts.md`)
- Orchestrator approves cross-spec changes
- Completion reports document any contract changes
- Sequential phases for integration points (daemon, tests)

**Exit Criteria**: No undocumented contract changes in final codebase.

## R7: Socket Security

**Risk**: Unix socket permissions could allow unauthorized users to trigger auth or enroll faces.

**Mitigation**:
- Socket permissions: 0o660, owned by root:howdy group
- Peer credential verification via SO_PEERCRED on every connection
- PAM module connects as root (PAM context)
- CLI requires root/sudo for write operations
- Message size limits (10MB max) prevent memory exhaustion
- Rate limiting prevents brute-force (5 attempts/user/60s)

**Exit Criteria**: Documented permission model. Socket not world-accessible. Peer creds verified.

## R8: Photo/Video Spoofing

**Risk**: Attacker holds a photo or video of the enrolled user in front of the camera, bypassing authentication entirely.

**Impact**: Critical -- complete authentication bypass with trivially available attack materials.

**Mitigation**:
- IR camera enforcement: `security.require_ir = true` (default). RGB cameras rejected for auth.
- Frame variance check: require embedding variance across min 3 frames (static photos produce identical embeddings)
- IR texture validation: verify face region has expected IR micro-texture (real skin vs flat surface)
- These are layered defenses -- each independently raises the attack bar

**Exit Criteria**: IR enforcement enabled by default. Photo attack tested and rejected. Config flag documented with security warning if overridden.

## R9: Biometric Data Exposure

**Risk**: Face embeddings in SQLite database are irreversible biometric data. Unlike passwords, they cannot be rotated if compromised.

**Impact**: High -- permanent compromise of biometric identity.

**Mitigation**:
- Database file permissions: 640, root:howdy ownership
- Embeddings never transmitted over network
- No cloud services, all processing local
- Document biometric data handling in user-facing docs
- Future: consider encryption at rest with TPM-backed key

**Exit Criteria**: Database permissions verified in install. Biometric data warning in docs.

## R10: Model Integrity

**Risk**: Tampered ONNX model files could always-match (backdoor) or always-reject (DoS).

**Impact**: High -- silent authentication bypass or denial of service.

**Mitigation**:
- SHA256 verification at model load time (not just download)
- Model files owned by root (644 permissions)
- Manifest with checksums compiled into binary (can't be tampered without recompiling)
- `howdy status` reports model integrity

**Exit Criteria**: SHA256 verified on every daemon start. Tampered model test in integration suite.
