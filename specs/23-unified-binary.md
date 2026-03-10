# Spec 23: Unified Binary

## Scope

Merge `visage-daemon`, `visage-auth`, and `visage-bench` into the `visage` CLI as subcommands. One binary for everything.

## Binary Changes

| Before | After |
|--------|-------|
| `visage-daemon` | `visage daemon` |
| `visage-auth --user X` | `visage auth --user X` |
| `visage-bench report` | `visage bench report` |

## Crate Changes

### Remove `crates/visage-daemon/src/auth_oneshot.rs`

The `[[bin]] visage-auth` target is removed. The auth oneshot logic moves into the CLI.

### Remove `crates/visage-bench/` crate

Bench subcommands move into `visage-cli` behind a `bench` feature or unconditionally.

### `crates/visage-cli/src/main.rs`

Add new subcommands:
```rust
Commands::Daemon { config: Option<String> },
Commands::Auth { user: String, config: Option<String> },
Commands::Bench { subcommand: BenchCommand },
```

### `crates/visage-daemon/`

Keep as a library-ish crate (handler, auth, enroll, rate_limit) but remove the `[[bin]]` targets. The `visage-cli` crate depends on it and calls `daemon::run(config)`.

Alternatively, move daemon logic into `visage-cli` directly since the CLI already has all the deps.

### PAM module update

`VISAGE_AUTH_BIN` default changes from `/usr/bin/visage-auth` to `/usr/bin/visage` with args `["auth", "--user", user]`.

### systemd unit update

`ExecStart=/usr/bin/visage daemon`

### PKGBUILD / justfile

Remove `visage-daemon` and `visage-auth` binary installs. Only install `visage`.

### Backward compat

None needed — this is pre-1.0. Clean break.

## Acceptance

- Single `visage` binary handles all subcommands
- `visage daemon` runs the persistent daemon
- `visage auth --user X` does one-shot auth (exit codes 0/1/2)
- `visage bench` runs benchmarks
- systemd unit uses `visage daemon`
- PAM module calls `visage auth`
- Container tests pass
- No separate `visage-daemon`, `visage-auth`, or `visage-bench` binaries
