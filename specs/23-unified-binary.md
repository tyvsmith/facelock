# Spec 23: Unified Binary

## Scope

Merge `facelock-daemon`, `facelock-auth`, and `facelock-bench` into the `facelock` CLI as subcommands. One binary for everything.

## Binary Changes

| Before | After |
|--------|-------|
| `facelock-daemon` | `facelock daemon` |
| `facelock-auth --user X` | `facelock auth --user X` |
| `facelock-bench report` | `facelock bench report` |

## Crate Changes

### Remove `crates/facelock-daemon/src/auth_oneshot.rs`

The `[[bin]] facelock-auth` target is removed. The auth oneshot logic moves into the CLI.

### Remove `crates/facelock-bench/` crate

Bench subcommands move into `facelock-cli` behind a `bench` feature or unconditionally.

### `crates/facelock-cli/src/main.rs`

Add new subcommands:
```rust
Commands::Daemon { config: Option<String> },
Commands::Auth { user: String, config: Option<String> },
Commands::Bench { subcommand: BenchCommand },
```

### `crates/facelock-daemon/`

Keep as a library-ish crate (handler, auth, enroll, rate_limit) but remove the `[[bin]]` targets. The `facelock-cli` crate depends on it and calls `daemon::run(config)`.

Alternatively, move daemon logic into `facelock-cli` directly since the CLI already has all the deps.

### PAM module update

`FACELOCK_AUTH_BIN` default changes from `/usr/bin/facelock-auth` to `/usr/bin/facelock` with args `["auth", "--user", user]`.

### systemd unit update

`ExecStart=/usr/bin/facelock daemon`

### PKGBUILD / justfile

Remove `facelock-daemon` and `facelock-auth` binary installs. Only install `facelock`.

### Backward compat

None needed — this is pre-1.0. Clean break.

## Acceptance

- Single `facelock` binary handles all subcommands
- `facelock daemon` runs the persistent daemon
- `facelock auth --user X` does one-shot auth (exit codes 0/1/2)
- `facelock bench` runs benchmarks
- systemd unit uses `facelock daemon`
- PAM module calls `facelock auth`
- Container tests pass
- No separate `facelock-daemon`, `facelock-auth`, or `facelock-bench` binaries
