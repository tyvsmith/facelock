# Clean-System Testing Walkthrough

This workflow records reviewable evidence for documentation commands on a
clean system. It never turns the documentation inventory into a script to run
blindly. Only explicit cases in `test/docs-walkthrough/cases.json` may execute;
unmapped executable and manual-only inventory rows remain visible as pending.

## Safety boundary

Booted scenarios run only in a disposable guest you provision and snapshot.
The runner does not create a VM. Before it will mutate the guest, it requires
the regular root-owned mode-0644 marker
`/etc/facelock-walkthrough-guest.json` with this shape:

```json
{
  "disposable": true,
  "guest_id": "unique-per-guest-id",
  "os": "debian-13",
  "image": "exact-matrix-image@sha256:full-image-digest",
  "init": "systemd",
  "snapshot": "pristine-snapshot-id",
  "level": "booted-vm",
  "hardware": []
}
```

Match `os`, `image`, and `init` to the selected scenario. The runner also
verifies virtualization and refuses shared 9p, virtiofs, NFS, or CIFS mounts;
protected PAM/bus mounts; and unexpected camera or TPM devices. Hardware
scenarios must declare intentionally attached devices instead of inheriting
them accidentally. A marker is an authorization for this disposable guest,
not something to place on a workstation.

## Inspect and validate the catalog

These repository-local commands do not run documented install/auth commands:

```bash
python3 test/docs-walkthrough/run.py list
python3 test/docs-walkthrough/run.py check
python3 test/docs-walkthrough/run.py report
```

`list` shows scenario IDs, `check` validates definitions, source pins, and total
mapping, and `report` compares explicit case mappings with the complete
documentation inventory. The separate walkthrough unit tests exercise safety
guards. Use `refresh` only when intentionally refreshing the checked-in
inventory and generated manual sections derived from documentation:

```bash
python3 test/docs-walkthrough/run.py refresh
git diff -- test/docs-walkthrough/cases.json
git diff -- test/docs-walkthrough/manual-sections.json
```

Review changed expectations and manual gate classifications before committing
either generated file. A refreshed mapping is not evidence that any command
was executed or reviewed.

The catalog includes repository and direct-package cases for APT, RPM/COPR,
AUR, source, NixOS, OpenRC, runit, and s6, plus first setup, daemon/oneshot
auth, desktop lock, physical TPM, GPU, and Y16 cases. Fixed adapters are
reviewed route probes with source-context references, not literal replay of
the referenced line. Literal source rows become ordered manual-section
candidates. Listing or generating a case is not a claim that its expectations
were reviewed or that it passed.

## Pin release identity

Every run consumes an identity JSON rather than inferring publication from a
tag or local build. Generate the readiness report for the intended release and
channel:

```bash
RELEASE_TAG=v0.2.0-alpha.1
CHANNEL=github-alpha
READINESS_FILE=/tmp/facelock-walkthrough-readiness.json
python3 test/docs-walkthrough/run.py readiness --release "$RELEASE_TAG" --channel "$CHANNEL" --output "$READINESS_FILE"
```

The identity must bind `release`, normalized `version`, exact native package
version, a 40-hex `artifact_commit`, channel, runtime policy, and immutable
artifact evidence: positive asset ID and size, release asset name and URL, and
64-hex SHA256. The evidence record separately binds `harness_sha256` and
`harness_tree_dirty`. Public-repository identities additionally bind the
downloaded package digest and repository URL, plus APT suite/signing-key
digest, COPR chroot, or AUR commit as applicable. An AUR source-built package
may omit the expected package digest; its evidence instead records the built
payload digest and the verified recipe commit. Every other repository channel
requires the expected package digest. `artifact_commit` is an asserted input
unless the installed record's `source_commit_verification` proves tag/build
linkage. Do not substitute a source checkout, staged build, or successful
rebuild for published-asset identity.

## Run one explicit case

Copy the repository into the disposable guest without a shared host mount,
install the marker, and use a new evidence directory:

```bash
SCENARIO=apt-trixie
IDENTITY_FILE=/root/facelock-walkthrough-identity.json
EVIDENCE_DIR=/root/facelock-evidence/apt-trixie
python3 test/docs-walkthrough/run.py run --scenario "$SCENARIO" --identity "$IDENTITY_FILE" --output "$EVIDENCE_DIR"
python3 test/docs-walkthrough/evidence.py validate "$EVIDENCE_DIR/evidence.json"
```

Use `--require-pass` only when a passing outcome is required. A real
publication absence is evidence, not a reason to rewrite the record as a pass.
For example, the current clean Debian 13 `apt-trixie` run records that the
future suite's Release URL returns 404; it does not establish a clean install.

When an environmental prerequisite is deliberately unavailable, record an
explicit blocked result rather than skipping silently. The generic runner
always rejects camera and TPM devices; intentional hardware work uses the
separate manual protocol.

```bash
SCENARIO=physical-tpm
IDENTITY_FILE=/root/facelock-walkthrough-identity.json
EVIDENCE_DIR=/root/facelock-evidence/physical-tpm
python3 test/docs-walkthrough/run.py blocked --scenario "$SCENARIO" --identity "$IDENTITY_FILE" --reason "no dedicated TPM passthrough guest available" --output "$EVIDENCE_DIR"
```

## Rootless container launcher

Container-eligible cases can use the guarded launcher with the exact image
from the release matrix. It creates a named, UUID-scoped, rootless container
with no mounts; it is not a substitute for booted systemd/PAM evidence:

```bash
SCENARIO=deb-trixie-direct
IDENTITY_FILE=/tmp/facelock-walkthrough-identity.json
CONTAINER_IMAGE=debian:13@sha256:full-image-digest
EVIDENCE_DIR=/tmp/facelock-evidence/deb-trixie-direct
python3 test/docs-walkthrough/run.py launch-container --scenario "$SCENARIO" --identity "$IDENTITY_FILE" --image "$CONTAINER_IMAGE" --output "$EVIDENCE_DIR"
```

## Aggregate evidence

Validate a collection and require all mapped required cases to pass only at
the release gate that actually has all prerequisites:

```bash
EVIDENCE_ROOT=/root/facelock-evidence
python3 test/docs-walkthrough/evidence.py aggregate "$EVIDENCE_ROOT"
python3 test/docs-walkthrough/evidence.py aggregate --require-pass "$EVIDENCE_ROOT"
```

Aggregation reports missing cases and unmapped documentation inventory rows.
It does not convert manual-only commands into executed coverage or let one
distribution/channel stand in for another.

## Manual evidence

`manual-sections.json` presents remaining manual commands as ordered steps,
including the exact documentation text, source location, and source hash.
Manual review is not a shortcut around that binding. A passing manual record
must include `manual_review` with the operator, notes,
`expectations_reviewed: true`, and the fixture bindings used. Each passing step
must preserve the exact `documented_command`, record the actual argv, expected
exit, actual output and observed state, and reference a sanitized, hashed log.
If a required expectation cannot be observed, record the case as blocked or
failed rather than marking the section complete.
