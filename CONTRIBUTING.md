# Contributing

## Prerequisites

- Rust 1.88+ (`rustup update`)
- Linux with V4L2 support
- A webcam (IR recommended; RGB works for development)
- Podman (for container tests)

## Building

```bash
cargo build --workspace
```

This leaves the unified development binary at `target/debug/facelock`; it does
not install it on `PATH`. The complete recipe inventory and prerequisites are
in [Developer Commands](docs/developer-commands.md).

## Workspace structure

Facelock is a Cargo workspace with 11 crates:

| Crate | Type | Purpose |
|-------|------|---------|
| `facelock-core` | lib | Config, types, errors, D-Bus interface, traits |
| `facelock-camera` | lib | V4L2 capture, auto-detection, preprocessing |
| `facelock-face` | lib | ONNX inference (SCRFD + ArcFace) |
| `facelock-store` | lib | SQLite face embedding storage |
| `facelock-daemon` | lib | Auth/enroll logic, rate limiting, liveness, audit |
| `facelock-cli` | bin | Unified CLI (`facelock` binary, includes `bench` subcommand) |
| `facelock-bench` | bin | Source-only standalone benchmark utility; see [Auxiliary Commands](docs/auxiliary-commands.md) |
| `pam-facelock` | cdylib | PAM module (libc, toml, serde, zbus only) |
| `facelock-tpm` | lib | Optional TPM encryption |
| `facelock-polkit` | bin | Polkit face authentication agent |
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

Each crate has a defined dependency boundary. See each crate's `Cargo.toml` for its actual dependencies.

### Supply-chain auditing

```bash
just audit  # cargo audit --deny unmaintained --deny unsound
```

`just audit` scans the full `Cargo.lock` for RustSec advisories and mirrors the
CI `cargo-audit` job. It fails on any vulnerability, unmaintained, or unsound
advisory that is not explicitly ignored. The ignore list, with a justification
per entry, lives in `.cargo/audit.toml`; add a new entry (with a reason) only
when an advisory is genuinely non-exploitable here and cannot yet be fixed by a
dependency bump. Requires `cargo install cargo-audit --locked`.

## Testing

### Tier 1: Unit tests (no hardware)

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Run these before every commit. They require no camera or models.

### Models

`models/*.onnx` is gitignored, so a fresh clone -- and every `git worktree add`
-- starts without models, while tiers 2 and 3 need them. If another checkout on
the machine has them, or `sudo facelock setup` has already downloaded them to
`/var/lib/facelock/models`, populate the new checkout from there instead of
downloading 435MB again:

```bash
just link-models                  # main checkout first, then the install tree
just link-models /path/to/models  # or say where to look
```

It hardlinks where it can (a worktree shares a filesystem with its main
checkout, so that is free and instant), copies where it cannot, and verifies
every file against the sha256 in `models/manifest.toml`. The camera tiers run
it themselves, hardlink-only, so a fresh worktree usually provisions itself
before you notice it was empty.

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

Use an explicitly marked disposable guest with snapshots for PAM, package and
login-flow testing. The evidence-producing runner does not provision a VM; see
[Testing Walkthrough](docs/testing-walkthrough.md).

### Tier 5: Host PAM testing

Only after tiers 3--4 pass. Always keep a root shell open. Start with `sudo` only -- never add Facelock to `login` or display manager PAM until `sudo` works reliably.

### All checks at once

```bash
just check  # runs test + clippy + fmt + audit + agent-doc consistency
```

`check-agent-docs` verifies that `.claude/rules/` and `.claude/skills/` still
describe the tree: that every `paths:` glob matches something, that referenced
`just` recipes and file paths exist, and that lists copied out of the justfile or
a workflow still match their source. A rule scoped to a path that no longer
exists fails silently otherwise -- it simply never loads.

`cargo test` also holds the user-facing documentation to the tree, in
`crates/facelock-cli/src/conformance/`. `docs.rs` and `man_pam.rs` check that
the CLI reference and both man pages describe the binary and the PAM module
that shipped. `packages.rs` checks that every package name a document tells a
reader to install is declared by `dist/PKGBUILD*`, `debian/control` or
`dist/facelock.spec`; if you add an install instruction, declare the dependency
in the packaging manifest that should already have had it. `pairs.rs` checks
that the six pages carried in both `docs/` and `book/src/` do not contradict
each other on package names, command names or file paths.

None of these reach the network. `just check-package-names-live` does, and asks
Arch, the AUR, Debian and Fedora whether the names still exist. Run it when you
touch a dependency; it is deliberately outside `just check`.

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

Read `docs/security.md` before implementing any auth-related code. Key rules:

- `security.require_ir` defaults to **true**. Never weaken this default.
- Frame variance checks must remain in the auth path.
- Model files are SHA256-verified at load time.
- IPC messages have size limits enforced by the D-Bus daemon (see `dbus/org.facelock.Daemon.conf`). Never allocate unbounded buffers.
- D-Bus system bus policy restricts daemon access.
- The PAM module logs all auth attempts to syslog.
- Rate limiting is enforced in the daemon (5 attempts/user/60s default).

## Contracts

Do not change binary names, paths, config keys, database schema, or auth semantics without updating `docs/contracts.md`.

## Submitting changes

1. Run `just check` (or at minimum `cargo test --workspace && cargo clippy --workspace -- -D warnings`).
2. Run container tests if your change touches PAM, daemon, or IPC code.
3. Keep commits focused. Separate refactoring from behavioral changes.
4. Write clear commit messages that explain *why*, not just *what*.
