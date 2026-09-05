# Contributing

## Prerequisites

- Rust 1.88+ (`rustup update`)
- Linux and the native build dependencies listed in [Quickstart](quickstart.md#build-from-source)
- A camera only for live capture/authentication work; IR is required by the default configuration
- Podman (for container tests)

## Building

```bash
cargo build --workspace
```

The unified binary is then `target/debug/facelock`; it is not installed on
`PATH`. See [Developer Commands](developer-commands.md) for the full inventory.

## Workspace structure

Facelock is a Cargo workspace with 11 crates:

| Crate | Type | Purpose |
|-------|------|---------|
| `facelock-core` | lib | Config, types, errors, D-Bus interface, traits |
| `facelock-camera` | lib | V4L2 capture, auto-detection, preprocessing |
| `facelock-face` | lib | ONNX inference (SCRFD + ArcFace) |
| `facelock-store` | lib | SQLite face embedding storage |
| `facelock-daemon` | lib | Auth/enroll logic, liveness, audit, rate limiting, handler |
| `facelock-cli` | bin | Unified CLI (`facelock` binary, includes `bench` subcommand) |
| `facelock-bench` | bin | Developer standalone benchmark utility; see [Auxiliary Commands](auxiliary-commands.md) |
| `pam-facelock` | cdylib | PAM module (libc + toml + serde + zbus only) |
| `facelock-tpm` | lib | Optional TPM-bound encryption for embeddings at rest |
| `facelock-polkit` | bin | Polkit authentication agent for face auth |
| `facelock-test-support` | lib | Mocks and fixtures for testing |

Version is declared once in the root `Cargo.toml` and inherited via `version.workspace = true`. Inter-crate dependencies use relative paths.

## Code style

- **Error handling**: `thiserror` for library error types, `anyhow` in binaries. Return `Result<T>` over panicking. Never `unwrap()` in library code.
- **Logging**: `tracing` for structured logging. Control verbosity via `RUST_LOG` env filter.
- **Tests**: `#[cfg(test)]` modules in each source file.
- **Formatting**: `cargo fmt` (default rustfmt settings).
- **Linting**: `cargo clippy --workspace -- -D warnings` must pass with zero warnings.

## Dependency rules

The PAM module (`pam-facelock`) must stay lightweight: **libc, toml, serde, zbus only**. No ort, no v4l, no facelock-core. This keeps the shared library small and avoids dragging heavy dependencies into every PAM-using process.

Each crate has a defined dependency boundary. See the [Contracts](contracts.md) chapter for the full table.

## Testing

### Tier 1: Unit tests (no hardware)

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Run these before every commit. They require no camera or models.

### Tier 2: Hardware tests (camera + models)

```bash
cargo test --workspace -- --ignored
```

Requires a connected camera and downloaded models. These tests are marked `#[ignore]` and skipped by default.

### Tier 3: Container tests (requires podman)

```bash
just test-arch-pam          # Arch PAM smoke tests (no camera)
just test-arch-integration  # end-to-end with camera (daemon mode)
just test-arch-oneshot      # end-to-end with camera (no daemon)
just test-arch-dev-shell    # interactive container shell for debugging
```

Container tests validate PAM integration without risking host lockout.

### Tier 4: VM testing

Use a disposable VM with snapshots for testing PAM changes against real login flows.

### Tier 5: Host PAM testing

Only after tiers 3--4 pass. Always keep a root shell open. Start with `sudo` only -- never add Facelock to `login` or display manager PAM until `sudo` works reliably.

### All checks at once

```bash
just check  # full local validation aggregate, including audit and docs/contracts
```

`just check` does not run the full packaging matrix or camera-required lanes.
For documentation-only changes, start with `just check-docs` and
`just docs-site-check`; use source review and the established behavior tests
to check meaning, and a targeted container probe for uncertain distro commands.

## Translations

Facelock is wired for gettext but not yet translated. `po/` holds only the two
`.pot` templates, and that is the intended state -- there are no `.po` files to
review, and no language is shipped.

Two catalogs, deliberately separate and never merged: `facelock` for the CLI
(extracted from the message seam in `crates/facelock-cli/src/message/`) and
`pam_facelock` for the PAM module, which has its own hard dependency ceiling.

```bash
just pot   # regenerate po/*.pot from source (translators and CI only)
just mo    # compile po/<lang>/*.po into target/locale for local verification
```

gettext is a build dependency of every package but stays optional for a source
install: English is compiled in as the fallback, so `just install-files` on a
machine without `msgfmt` installs untranslated rather than failing.

Installing a catalog is `scripts/install-locale-catalogs.sh`, and **every**
install path calls it -- deb, rpm, the three PKGBUILDs, Nix, and the source
install that OpenRC, runit and s6 systems use. `just test-locale-install-contract`
is what holds that together; it builds a throwaway pseudo-locale, because with
no `.po` in the tree nothing else would notice a broken install path until the
first translation landed. Add a packaging path, wire it there too.

Two things are still missing, both tracked in
[#140](https://github.com/tyvsmith/facelock/issues/140):

- a long tail of CLI print sites (counted per domain in #140) still writes
  English directly instead of going through the message seam, so a translation
  would cover the converted subset only. The conversion pattern is documented
  in `crates/facelock-cli/src/message/mod.rs` and is best done one domain at a
  time.
- no translation has been accepted yet. Start one with
  `mkdir -p po/de && msginit -i po/facelock.pot -o po/de/facelock.po -l de`.

## Security considerations

Read the [Security](security.md) chapter before implementing any auth-related code. Key rules:

- `security.require_ir` defaults to **true**. Never weaken this default.
- Frame variance checks must remain in the auth path.
- Model files are SHA256-verified at load time.
- D-Bus message size limits are enforced by the bus daemon. Never allocate unbounded buffers.
- D-Bus system bus policy restricts daemon access.
- The PAM module logs all auth attempts to syslog.
- Daemon and oneshot authentication limit face-detected failures (5/user/60s by default); successful and no-face attempts do not consume this budget.

## Contracts

Do not change binary names, paths, config keys, database schema, or auth semantics without updating the [Contracts](contracts.md) chapter.

## Submitting changes

1. Run `just check` (or at minimum `cargo test --workspace && cargo clippy --workspace -- -D warnings`).
2. Run container tests if your change touches PAM, daemon, or IPC code.
3. Keep commits focused. Separate refactoring from behavioral changes.
4. Write clear commit messages that explain *why*, not just *what*.
