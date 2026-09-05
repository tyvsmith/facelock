# Command Documentation Audit

This audit supports [issue #211](https://github.com/tyvsmith/facelock/issues/211).
It separates documentation conformance from actually executing instructions on
clean systems. A passing parser, local build, or container install does not
establish working PAM authentication on a booted machine.

## Authoritative surfaces

Cargo metadata discovers executable targets; the recursive Clap tree supplies
the unified CLI's commands, aliases, scoped options, positionals, defaults,
required arguments, value choices and conflicts. Public Just metadata supplies
the developer recipe index. The exact file classification lives in
`test/docs-corpus.json`; new instructional files and stale classifications fail
the inventory check, including in distribution source archives without `.git`.

- [CLI reference](cli.md): every public command and its scoped flags; the book includes this source
- [Auxiliary commands](auxiliary-commands.md): standalone benchmark verbs and the non-CLI polkit session executable
- [Developer commands](developer-commands.md): generated public recipe signatures and executable destinations
- [Configuration](configuration.md): defaults compared with the serialized Rust configuration and PAM-only policy declarations
- [Contracts](contracts.md): privilege, paths, state ownership, output and integration behavior
- Man pages: scoped options checked against the parser; roff syntax checked separately
- Website and book: command extraction plus link, fragment and asset checks against assembled HTML

Historical proposals, intentional invalid examples, syntax metavariables and
unsupported shell expressions have explicit classifications. Parsing never
dispatches a documented command. Unsupported expressions remain outstanding
walkthrough obligations instead of silently becoming successful tests.

## Reproduce the deterministic checks

Use the development prerequisites in [Quick Start](quickstart.md), including
Python 3, Bash, Just and the Rust toolchain. The site additionally requires the
same mdBook 0.4.44 used by Pages; CI also runs mandoc.

```bash
just check-docs
just docs-site-check
```

The CLI inventory can be exported without accessing cameras or the system bus:

```bash
FACELOCK_DOCS_SURFACE_OUTPUT=/tmp/facelock-cli-surface.json cargo test -p facelock-cli --bin facelock conformance::surface::export_cli_surface_if_requested
```

Mutation fixtures cover invented commands/options, missing required arguments,
stale security defaults, shell quoting/redirection, missing documentation,
broken site links, changed source hashes and invalid walkthrough evidence.
The source archive test path must remain usable by Arch's package `check()`;
Python is a check dependency, not an added runtime dependency.

## Existing behavioral evidence boundaries

The audit supplements rather than replaces the existing semantic suites:

- `cli_smoke` checks early privilege refusals, including PAM changes, state removal, TPM commands, and daemon startup; it also checks help/version and config-independent capabilities
- `json_stream_split` checks machine output and diagnostic stream separation
- `facelock-core` path tests check privileged configuration overrides and protected path identity
- The camera-free container suite checks real D-Bus/PAM preflight, authorization, and output behavior
- Package lifecycle/source-install suites check owned paths, install/removal behavior and service handling
- Physical camera, desktop, TPM, GPU and alternative-init walkthroughs still require the corresponding disposable environment and observations

A suite's presence in this list does not claim it was executed for a particular
release. Keep each run's revision, exact command, environment and outcome with
its evidence.

## Clean-system findings and remaining work

Observed on 2026-09-05:

- The `v0.2.0-alpha.1` tag exists, but its GitHub Release was unavailable; no alpha release asset installation was established
- A fresh pinned Debian 13 container reached the public APT `trixie` suite and received HTTP 404/no Release file; the documentation now identifies this suite as planned, not currently installable
- A fresh pinned Fedora 44 container installed the production COPR package and ran its CLI; the observed package was `0.1.3-1.fc44`, not the alpha or latest stable `0.1.4`
- Stable APT/AUR/production COPR channels cannot establish prerelease coverage; the release matrix separates these channels

These results do not close #211. See [the walkthrough protocol](testing-walkthrough.md)
for release identity, sanitized logs, source hashes and evidence levels. The
catalog records route tests separately from exact documentation occurrences;
mechanically generated section cases are candidates awaiting human review,
fixture bindings and execution, not proof of coverage.

Remaining requirements include published alpha assets, applicable current
public-package identities, booted distro/alternative-init/NixOS guests, and
explicit camera/desktop/TPM/GPU access with a recovery path. No host PAM,
desktop or device configuration is changed by the static audit.
