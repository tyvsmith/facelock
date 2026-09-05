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
- Fresh pinned Fedora 43, 44 and 45 containers installed production COPR packages and ran their CLI; the observed packages were `0.1.3-1.fc43`, `0.1.3-1.fc44` and `0.1.3-1.fc45`, not the alpha or latest stable `0.1.4`
- Stable APT/AUR/production COPR channels cannot establish prerelease coverage; the release matrix separates these channels

These results do not close #211. See [the walkthrough protocol](testing-walkthrough.md)
for release identity, sanitized logs, source hashes and evidence levels. The
catalog records route tests separately from exact documentation occurrences;
mechanically generated section cases are candidates awaiting human review,
fixture bindings and execution, not proof of coverage.

### Evidence identity and scope

The audit inventories 69 instructional files, three executable targets and 77
public recipes. The isolated workspace run passed 1,713 tests with 12 ignored
hardware tests; all-target Clippy, formatting, source-archive extraction, site
links, mandoc and package-lifecycle documentation checks were also exercised.
These are local verification results, not release or hardware attestations.
The full `just check` gate completed; the final documentation/safety subset was
rerun after the last isolation fixes. Native Debian/RPM version-ordering checks
were skipped because `dpkg` and `rpmdev-vercmp` were unavailable. Dependency
audit completed with three yanked-package warnings allowed by existing policy.
The final extractor records 1,350 occurrences: 417 executable, 21 manual-only,
733 schematic references, 177 historical and two intentional negative examples.
The catalog has 28 route/hardware definitions and 134 manual section candidates;
all 438 executable/manual occurrences have pending definitions, with none
unmapped. Definition coverage is not reviewed-scenario or execution completion.

The production COPR observations used build `10489915` and these retained
package hashes:

| Chroot | Native package version | SHA256 |
|---|---|---|
| Fedora 43 x86_64 | `0.1.3-1.fc43` | `66061f0d239a4ac58cd37b8f49a1189977bd34aaed7fb9ed4e6a849bcf01f2a3` |
| Fedora 44 x86_64 | `0.1.3-1.fc44` | `469fabc1c8678bccb48342d36107a3dd67429b0a8ac1406e742ed0d8f5884697` |
| Fedora 45 x86_64 | `0.1.3-1.fc45` | `521a7abbdf266bea57f1c8031912734c4968b57251ba9f9b1cec4e44f97f58b0` |

The tag commit `24aed886a71a3ed904f3232a1776a460f81b7d85` is an identity
assertion, not independently verified binary build provenance. Matching a
retained same-version cache payload does not exclude same-version repository
replacement. Repository completion therefore additionally requires verified
transaction-bound bytes; these container observations do not qualify.

Local, uncommitted artifacts are retained under `target/docs-audit/` in the
audit worktree. `inventory.json`, `examples.json` and `coverage.json` contain
the final machine-readable inventories. `evidence/apt-trixie-final`,
`evidence/copr-43-final`, `evidence/copr-44-final`, `evidence/copr-45-final`
and `evidence/alpha-blocked-final` contain sanitized logs and JSON records.
These paths are build artifacts, not files shipped in a source checkout.

The APT/Fedora 44/alpha records name harness revision
`1f20684f294697584c8cea9b000edb82c7d6c201`; Fedora 43/45 name
`628e2b83ff303d0c1cd2b8e3a932d375d42f0139`. All were clean committed harness
runs at their recorded revision. Later documentation and guard changes are
not retroactively credited to these records: old source pins must not be
rewritten to qualify against the final inventory.

A further Fedora 44 probe at clean harness revision
`8f4253f4c892719b594995286ceec6920130812c` exercised the final isolation guards.
Its record and logs are in `evidence/copr-44-guarded`. All six pristine-state
observations were checked, including PAM and service assets. Validation accepts
it as a container observation and correctly rejects it as walkthrough
completion; transaction-byte and source-build provenance remain unestablished.

Remaining requirements include published alpha assets, applicable current
public-package identities, booted distro/alternative-init/NixOS guests, and
explicit camera/desktop/TPM/GPU access with a recovery path. No host PAM,
desktop or device configuration is changed by the static audit.
