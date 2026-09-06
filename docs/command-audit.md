# Command Documentation Audit

This audit supports [issue #211](https://github.com/tyvsmith/facelock/issues/211).
Its deliverable is accurate, current documentation, not a rerun of the project's
behavioral or release qualification suites. Review commands against their
parsers and implementations, package instructions against manifests and the
relevant distribution repositories, and descriptions against the existing
contracts and behavioral tests. Use a disposable container to settle an
installation-specific uncertainty; use a VM or dedicated hardware only when a
particular documentation claim cannot otherwise be established.

Documentation conformance and clean-system execution are separate evidence.
A passing parser or source review does not establish a new hardware result;
conversely, an example need not be rerun on hardware to correct its syntax or
explain already-tested behavior accurately.

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
dispatches a documented command. Unsupported expressions need source/context
review; their optional walkthrough records remain pending instead of silently
becoming successful runtime tests.

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

## Dated channel observations

Observed on 2026-09-05:

- The `v0.2.0-alpha.1` tag exists, but its GitHub Release was unavailable; no alpha release asset installation was established
- A fresh pinned Debian 13 container reached the public APT `trixie` suite and received HTTP 404/no Release file; the documentation now identifies this suite as planned, not currently installable
- Fresh pinned Fedora 43, 44 and 45 containers installed production COPR packages and ran their CLI; the observed packages were `0.1.3-1.fc43`, `0.1.3-1.fc44` and `0.1.3-1.fc45`, not the alpha or latest stable `0.1.4`
- Stable APT/AUR/production COPR channels cannot establish prerelease coverage; the release matrix separates these channels

Those earlier observations established publication limitations, not a prerequisite to
reviewing the alpha.1 source. Its tag resolved to
`53385c53b9c4b2e0f83368797c14599dc2c61485`, and GitHub serves the tagged source
archive without a GitHub Release at that checkpoint. Keep source-review results distinct from
claims about uploaded release binaries or public repository packages.

See [the walkthrough protocol](testing-walkthrough.md) when new runtime evidence
is needed. Its catalog records route tests separately from exact documentation
occurrences. Mechanically generated cases are execution candidates, not a
documentation-accuracy score or a requirement to repeat established tests.

## Documentation-accuracy follow-up

The 2026-09-05 follow-up reviewed the CLI, auxiliary executables, public Just
recipes, configuration, contracts, man pages, Markdown guides and rendered
site against their source definitions. It corrected privilege requirements,
exit-status caveats, TPM recovery instructions, JSON examples, setup behavior,
rate-limit accounting and claims about hardware, performance and security.
The book now includes the canonical Quick Start instead of maintaining a
second installation guide.

Distribution metadata checks covered source prerequisites on Arch, Debian 13,
Ubuntu 26.04 and Fedora 43. The instructions distinguish native Rust versions
from the pinned rustup toolchain, build dependencies from ONNX Runtime's shared
library, and official Arch CPU/GPU packages from virtual package names. Nix
instructions explicitly describe the current privileged-loader and model/key
provisioning limitations; they do not present the module interface as a
validated working installation. Model licensing refers to upstream terms and
does not infer permission for personal desktop authentication.

Verification for this follow-up is deliberately documentation-focused:
`just check-docs`, the assembled-site link/anchor/asset check, man-page lint,
release-matrix documentation checks and focused offline regressions for package
instruction extraction and lookup. It does not repeat the full behavioral
suite below, install host authentication configuration, or claim new camera,
VM, GPU or TPM results. Published alpha assets are not required for these
source and documentation checks.

### Published alpha.4 follow-up

The follow-up rebased the documentation onto
`8fa96c9179a7d14a7c0ca1b74b686672335a8841`, the commit identified by the
`v0.2.0-alpha.4` tag, its release manifest and the successful
[release run](https://github.com/tyvsmith/facelock/actions/runs/34004251451).
The [prerelease](https://github.com/tyvsmith/facelock/releases/tag/v0.2.0-alpha.4)
was published at 2026-09-06 02:07:12 UTC (September 5 in Pacific time).

- Downloaded all nine published assets and matched their sizes and SHA256 digests with the release metadata; the eight payloads also matched `MANIFEST.json`
- Matched the separately downloaded tag archive to the manifest's source digest, `28d35c51b4c0f3702291c526174e1bbb4d249daf86b017c0d652a4b30cda7053`
- Inspected both Debian control archives and file inventories: native versions use tildes, but GitHub download filenames use dots; the dotted URLs work and the tilde URLs return 404
- Inspected the direct Fedora 44 RPM's version, dependencies, file inventory and scriptlets; it bundles ONNX Runtime, unlike the system-runtime COPR build
- Confirmed both Debian packages contain the manifest-pinned CPU ONNX Runtime 1.20.1 under `/usr/lib/facelock`; the direct RPM places it under `/usr/lib64/facelock`
- Ran only the released main executable's help and version in a disposable Ubuntu 24.04 container with `libxkbcommon0`; it reports `facelock 0.2.0-alpha.4`

The manifest itself has SHA256
`b7f9fbc5df710ea3facb894e5ed4d67d2a4aaa8808261ecc15aada4dcfa7eb5e`.
Matching downloaded bytes to release metadata is an integrity check, not an
independent reproducible-build or signing attestation. The tag is unsigned,
the release is not immutable, and the direct RPM has no package signature.
The downloaded files are retained locally in
`/tmp/facelock-alpha4-audit.6w9Zm1`; that temporary directory is not shipped.

This check corrected the current release links and exact package commands in
Quick Start, the README, the book and the HTML website. It also found redundant
`setup`-then-`enroll` instructions in the released Debian/RPM descriptions and
post-install prompts. The source package text now directs users to the setup
wizard; Quick Start explains the redundant alpha.4 prompt. Published alpha.4
bytes were not replaced.

The documentation audit and published-alpha inspection do not require stable
0.2.0 publication. Stable AUR, codenamed APT and production COPR still need a
separate post-publication check of their actual packages and metadata. The
live `facelock-git` recipe also still lacks the runtime dependency already
declared in this source tree; the install guides explicitly warn about it.
No host installation or camera, GPU, physical TPM or authentication retest was
performed for this follow-up. Pending walkthrough execution records remain
pending; refreshing their source pins does not convert them to evidence.

### Earlier checkpoint: evidence identity and scope

The earlier audit checkpoint inventoried 69 instructional files, three executable
targets and 77 public recipes. Its isolated workspace run passed 1,713 tests with 12 ignored
hardware tests; all-target Clippy, formatting, source-archive extraction, site
links, mandoc and package-lifecycle documentation checks were also exercised.
These are local verification results, not release or hardware attestations.
The full `just check` gate completed; the final documentation/safety subset was
rerun after the last isolation fixes. Native Debian/RPM version-ordering checks
were skipped because `dpkg` and `rpmdev-vercmp` were unavailable. Dependency
audit completed with three yanked-package warnings allowed by existing policy.
That checkpoint's extractor recorded 1,350 occurrences: 417 executable, 21 manual-only,
733 schematic references, 177 historical and two intentional negative examples.
The catalog has 28 route/hardware definitions and 134 manual section candidates;
all 438 executable/manual occurrences had pending definitions, with none
unmapped. These are the counts at the preceding audit checkpoint; edits change
them. Use the inventory and walkthrough report for current counts. Pending
execution does not mean that the documentation assertion is unreviewed or
incorrect. Definition coverage is not execution completion.

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

Published alpha assets are needed only to check their actual download URLs,
metadata, hashes and delivered contents. Public-package instructions depend on
the respective repository, and stable channels do not carry prereleases.
Neither publication nor full booted/hardware walkthrough coverage is a gate
for documentation-only review. Any unresolved claim should identify its
specific missing evidence instead of blocking unrelated documentation work.
No host PAM, desktop or device configuration is changed by the static audit.
