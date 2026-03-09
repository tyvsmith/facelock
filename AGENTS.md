# AGENTS.md - howdy-rust

## Project Overview

Rust rewrite of howdy (Linux face authentication via PAM). Read `README.md` for architecture. Implementation specs are in `specs/`.

## Repository Structure

Cargo workspace with 8 crates:
- `crates/howdy-core/` -- shared library (config, types, errors, IPC protocol)
- `crates/howdy-camera/` -- V4L2 camera capture and preprocessing
- `crates/howdy-face/` -- ONNX inference pipeline (SCRFD + ArcFace)
- `crates/howdy-store/` -- SQLite face embedding storage
- `crates/howdy-daemon/` -- persistent daemon (camera + models + auth)
- `crates/howdy-cli/` -- user-facing CLI tool
- `crates/pam-howdy/` -- PAM module (cdylib, thin IPC client)
- `crates/howdy-bench/` -- benchmark and calibration tooling

## Build & Verify

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo run --bin howdy -- --help
```

## Mandatory Reading Order

Before making product changes, read:
1. `AGENTS.md` (this file)
2. `docs/contracts.md`
3. `docs/security.md`
4. `docs/risk-register.md`
5. `docs/delivery-roadmap.md`
6. Your assigned `specs/XX-name.md`

## Core Rules

- Do not invent architecture outside documented contracts.
- Do not silently change binary names, paths, config keys, database schema, or auth semantics.
- If a cross-spec contract must change, update `docs/contracts.md` in the same change and explain why.
- Keep the PAM module free of heavy dependencies (no ort, no v4l, no howdy-core).
- Keep all inference local. No cloud services, no runtime model downloads in auth path.
- Prefer minimal dependencies and clear crate boundaries.

## Security Rules

- **Read `docs/security.md`** before implementing any auth-related code.
- IR camera enforcement (`security.require_ir`) must default to **true**. Never weaken this default.
- Frame variance checks must be implemented in the auth path — static photo attacks are trivial otherwise.
- Model files must be SHA256-verified at load time, not just download time.
- All IPC messages must have size limits. Never allocate unbounded buffers from network input.
- Socket access must verify peer credentials via `SO_PEERCRED`, not just filesystem permissions.
- PAM module must log all auth attempts to syslog (success, failure, error, timeout).
- Database and model files must have restrictive permissions (640/644, root:howdy ownership).
- Never store embeddings in world-readable locations. Embeddings are biometric data.
- Rate limiting must be enforced in the daemon (default: 5 attempts per user per 60 seconds).

## Code Style

- Use `thiserror` for error types in library crates, `anyhow` in binary crates.
- Prefer returning `Result<T>` over panicking. Never `unwrap()` in library code.
- Use `tracing` for structured logging.
- Keep functions small. Write unit tests for all non-trivial logic.
- Use `#[cfg(test)]` modules in each source file for tests.

## Dependency Rules

- **howdy-core**: `serde`, `toml`, `bincode`, `thiserror`, `tracing`
- **howdy-camera**: `howdy-core`, `v4l`, `image`
- **howdy-face**: `howdy-core`, `ort`, `ndarray`, `image`
- **howdy-store**: `howdy-core`, `rusqlite` (bundled), `bytemuck`
- **howdy-daemon**: `howdy-core`, `howdy-camera`, `howdy-face`, `howdy-store`, `signal-hook`, `tracing-subscriber`
- **howdy-cli**: `howdy-core`, `clap`, `smithay-client-toolkit`, `indicatif`, `reqwest` (blocking), `anyhow`, `notify-rust`, `tracing-subscriber`
- **pam-howdy**: `libc`, `toml`, `serde` ONLY. Must stay tiny. No howdy-core dependency.
- **howdy-bench**: `howdy-core`, `howdy-camera`, `howdy-face`, `howdy-store`

## Implementation Order

Specs must be implemented in dependency order. See `docs/delivery-roadmap.md`.

**Phase 1** (Foundation): specs 00 -> 01 (sequential)
**Phase 2** (Components): specs 02, 03, 04 (parallel, each only depends on 01)
**Phase 3** (Integration): spec 05 (integrates Phase 2)
**Phase 4** (Interfaces): specs 06, 07 (parallel, each depends on 05)
**Phase 5** (Polish): specs 08, 09, 10 (parallel)
**Phase 6** (Validation): specs 11, 12 (sequential)

## Agent Orchestration

### Recommended Model: Phased with Parallel Worktree Agents

- Use one orchestrator agent.
- For sequential phases: single agent processes specs in order.
- For parallel phases: launch one agent per spec in isolated git worktrees.
- Max 2-3 active implementation agents concurrently.
- Each agent owns exactly one spec and the files it primarily affects.
- Cross-cutting edits require orchestrator approval and contract doc updates.

### Prompts

**Phase 1 (single agent):**
```
Read specs/00-workspace.md and specs/01-core-types.md. Execute them in order.
The project root is /home/ty/Code/howdy-rust. Create the Cargo workspace and
all crate scaffolds first, then implement core types, config (with HOWDY_CONFIG
env var support), and IPC protocol. Run `cargo build --workspace` to verify.
```

**Phase 2 (3 parallel agents in worktrees):**
```
Agent A: "Read specs/02-camera.md. Implement howdy-camera. Run cargo build -p howdy-camera && cargo test -p howdy-camera."
Agent B: "Read specs/03-face-engine.md. Implement howdy-face. Run cargo build -p howdy-face && cargo test -p howdy-face."
Agent C: "Read specs/04-face-store.md. Implement howdy-store. Run cargo build -p howdy-store && cargo test -p howdy-store."
```

**Phase 3 (single agent):**
```
Read specs/05-daemon.md and all prior specs for context. Implement howdy-daemon.
Run cargo build -p howdy-daemon to verify.
```

**Phase 4 (2 parallel agents in worktrees):**
```
Agent A: "Read specs/06-pam-module.md. Implement pam-howdy. Run cargo build -p pam-howdy."
Agent B: "Read specs/07-cli.md. Implement howdy-cli. Run cargo build -p howdy-cli."
```

### Blocking and Escalation

Stop and escalate to the orchestrator if:
- The spec cannot be implemented without changing a documented contract
- An acceptance criterion is impossible under current constraints
- Benchmark targets are missed by a meaningful margin
- Another agent has modified the same area in a conflicting way
- PAM behavior would deviate from the documented auth model

## Testing Strategy

**READ `docs/testing-safety.md` BEFORE implementing anything PAM-related.**

### Tier 1 -- Unit Tests (Host, Safe)
Config parsing, face matching math, storage CRUD, alignment transforms, IPC serialization.
```bash
cargo test --workspace
```

### Tier 2 -- Integration Tests with Hardware (Host, Marked Ignored)
Camera capture, ONNX model loading, full pipeline inference.
```bash
cargo test --workspace -- --ignored
```

### Tier 3 -- PAM Module Testing (Container Only)
Build container, install PAM module, test with `pamtester`.
**NEVER** install `pam_howdy.so` or edit `/etc/pam.d/*` on the host until validated.

### Tier 4 -- Host PAM Installation (After Tier 3 Passes)
Keep a root shell open. Start with `/etc/pam.d/sudo` only. Back up before editing.

### General Rules
- Mark hardware-dependent tests with `#[ignore]` so `cargo test` passes without devices
- PAM tests are NEVER automated on the host -- always containerized first
- `HOWDY_CONFIG` env var must be supported for rootless development

## Ownership Model

- One implementation agent owns one spec at a time.
- An agent should primarily edit files for the spec it owns.
- Cross-cutting edits are allowed only when required and must be reflected in contract docs.
- If two specs need the same files, the orchestrator decides sequencing.

## Evidence Requirements

Every implementation agent must provide with completion:
- Tests added or updated
- Commands run and their output
- Benchmark results if performance-sensitive code changed
- Risks or follow-up work identified
- Whether any contract files changed

Use `templates/spec-execution-report.md` for completion reports.

## Non-Goals for MVP

Do not add these unless the owning spec explicitly says otherwise:
- Cloud services or runtime model downloads
- Compositor-specific integrations
- Automatic PAM file mutation during installation
- Advanced liveness detection
- GPU/CUDA inference (CPU-only for MVP)
