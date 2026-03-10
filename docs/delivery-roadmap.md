# Delivery Roadmap

## Execution Strategy

Phased implementation following dependency order. Each phase has clear entry/exit criteria. Parallel work uses git worktrees for isolation.

## Phase Diagram

```
Phase 1 (Foundation)     Phase 2 (Components)       Phase 3 (Integration)
┌──────────────────┐     ┌──────────────────┐       ┌──────────────────┐
│ 00-workspace     │     │ 02-camera        │       │                  │
│ 01-core-types    │────>│ 03-face-engine   │──────>│ 05-daemon        │
│   (sequential)   │     │ 04-face-store    │       │   (sequential)   │
└──────────────────┘     │   (parallel)     │       └────────┬─────────┘
                          └──────────────────┘                │
                                                              v
Phase 4 (Interfaces)     Phase 5 (Polish)           Phase 6 (Validation)
┌──────────────────┐     ┌──────────────────┐       ┌──────────────────┐
│ 06-pam-module    │     │ 08-preview       │       │ 11-benchmarks    │
│ 07-cli           │────>│ 09-notifications │──────>│ 12-integration   │
│   (parallel)     │     │ 10-build-install │       │   (sequential)   │
└──────────────────┘     │   (parallel)     │       └──────────────────┘
                          └──────────────────┘
```

## Phase Details

### Phase 1: Foundation (Sequential, Single Agent)

| Spec | Description | Depends On |
|------|-------------|------------|
| 00-workspace | Cargo workspace, crate scaffolds, dev config | -- |
| 01-core-types | Config, types, errors, IPC protocol | 00 |

**Entry**: Clean repo
**Exit**: `cargo build --workspace` succeeds, all crates have stubs

### Phase 2: Components (Parallel, 3 Agents in Worktrees)

| Spec | Description | Depends On |
|------|-------------|------------|
| 02-camera | V4L2 capture, format conversion, CLAHE | 01 |
| 03-face-engine | SCRFD detection, ArcFace embedding, alignment | 01 |
| 04-face-store | SQLite storage, CRUD operations | 01 |

**Entry**: Phase 1 complete
**Exit**: Each crate builds and passes unit tests independently
**Why parallel**: These three crates are independent -- they only depend on visage-core types.

### Phase 3: Integration (Sequential, Single Agent)

| Spec | Description | Depends On |
|------|-------------|------------|
| 05-daemon | Persistent daemon, auth flow, enrollment flow | 02, 03, 04 |

**Entry**: All Phase 2 crates merged and building
**Exit**: Daemon starts, handles Ping, basic auth/enroll flow works

### Phase 4: Interfaces (Parallel, 2 Agents in Worktrees)

| Spec | Description | Depends On |
|------|-------------|------------|
| 06-pam-module | Thin IPC client, PAM FFI, PAM_IGNORE fallback | 01 (IPC protocol) |
| 07-cli | All user-facing commands, model download | 01 (IPC protocol) |

**Entry**: Phase 3 complete (daemon API stable)
**Exit**: PAM module builds, CLI builds, both can communicate with daemon
**Why parallel**: PAM and CLI are both IPC clients, independent of each other.

### Phase 5: Polish (Parallel, 3 Agents)

| Spec | Description | Depends On |
|------|-------------|------------|
| 08-preview | Wayland layer-shell camera preview | 07 (CLI framework) |
| 09-notifications | D-Bus notifications for auth events | 07 (CLI) |
| 10-build-install | justfile, PKGBUILD, install paths | all prior |

**Entry**: Phase 4 complete
**Exit**: Preview window works, notifications fire, `just install` works

### Phase 6: Validation (Sequential, Single Agent)

| Spec | Description | Depends On |
|------|-------------|------------|
| 11-benchmarks | Latency measurement, threshold calibration | all prior |
| 12-integration-tests | Full system tests, PAM container tests | all prior |

**Entry**: All features complete
**Exit**: Benchmarks meet targets, all tiers of testing pass

## Critical Path

```
00-workspace -> 01-core-types -> 03-face-engine -> 05-daemon -> 07-cli -> 12-integration-tests
```

Face engine is on the critical path because the daemon can't do meaningful work without it.

## Recalibration Gates

Pause and reassess at these points:
1. **After Phase 2**: Are ONNX models loading and producing valid embeddings? Is camera capture reliable?
2. **After Phase 3**: Is daemon auth latency acceptable (<450ms warm)?
3. **After Phase 4**: Does PAM return correct codes? Does CLI complete full workflows?
4. **After Phase 6**: Do benchmarks meet targets? Are shipped defaults evidence-backed?

## Concurrency Rules

- Max 2-3 active implementation agents at any time
- Never run two agents editing the same crate simultaneously
- Worktree isolation required for all parallel phases
- Orchestrator merges worktree results between phases
