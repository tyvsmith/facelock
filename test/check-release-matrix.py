#!/usr/bin/env python3
"""Fail closed when release target declarations drift from the alpha matrix."""

from __future__ import annotations

import json
import os
import re
import sys
from datetime import date
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "dist" / "release-matrix.json"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def rust_array_body(source: str, name: str) -> str:
    match = re.search(
        rf"(?ms)^(?:pub )?const {re.escape(name)}:[^=]+?=\s*&\[(.*?)^\];",
        source,
    )
    require(match is not None, f"Rust source must define parseable {name}")
    return match.group(1)


try:
    matrix = json.loads(MATRIX_PATH.read_text())
except FileNotFoundError:
    fail(f"missing checked-in release matrix: {MATRIX_PATH.relative_to(ROOT)}")
except json.JSONDecodeError as error:
    fail(f"invalid release matrix JSON: {error}")

expected_rows = [
    ("debian-13", "Debian 13 trixie", "amd64", "bundled ORT 1.20.1", "supported", True, False, "full"),
    ("ubuntu-26.04", "Ubuntu 26.04 LTS", "amd64", "bundled ORT 1.20.1", "supported", True, False, "full"),
    ("fedora-43", "Fedora 43", "x86_64", "system ORT", "supported", True, False, "full"),
    ("fedora-44-copr", "Fedora 44", "x86_64", "system ORT", "supported", True, False, "full"),
    ("fedora-45", "Fedora 45 branched", "x86_64", "system ORT", "supported", True, False, "build/runtime smoke"),
    (
        "fedora-rawhide",
        "Fedora Rawhide (Fedora 46 development)",
        "x86_64",
        "system ORT",
        "experimental",
        False,
        True,
        "best-effort pinned Track D smoke only",
    ),
    ("fedora-44-direct", "Fedora 44", "x86_64", "bundled ORT 1.20.1", "supported", True, False, "full"),
    ("arch-2026-08-18", "Arch Linux Archive snapshot 2026-08-18", "x86_64", "system ORT", "supported", True, False, "full"),
]
actual_rows = [
    (
        row["id"],
        row["platform"],
        row["architecture"],
        row.get("runtime"),
        row.get("support_tier"),
        row.get("release_target"),
        row.get("optional"),
        row["lifecycle_depth"],
    )
    for row in matrix.get("platforms", [])
]
require(
    actual_rows == expected_rows,
    "platform/architecture/runtime/support-tier/release-target/optional/lifecycle rows differ from issue #234",
)
expected_debian_packaging = {
    "debian-13": ("facelock", ["tpm"], "staged APT/direct deb"),
    "ubuntu-26.04": ("facelock", ["tpm"], "staged APT/direct deb"),
}
for platform_id, expected_packaging in expected_debian_packaging.items():
    row = next((candidate for candidate in matrix.get("platforms", []) if candidate.get("id") == platform_id), {})
    require("variant" not in row, f"{platform_id} retains a Debian package variant axis")
    actual_packaging = (row.get("package"), row.get("required_capabilities"), row.get("channel"))
    require(
        actual_packaging == expected_packaging,
        f"{platform_id} package/capability/channel contract drifted: {actual_packaging!r}",
    )
expected_non_debian_variants = {
    "fedora-43": "staging COPR",
    "fedora-44-copr": "staging COPR",
    "fedora-45": "staging COPR",
    "fedora-rawhide": "optional experimental production COPR chroot",
    "fedora-44-direct": "direct RPM",
    "arch-2026-08-18": "PKGBUILD and binary recipe",
}
for platform_id, expected_variant in expected_non_debian_variants.items():
    row = next((candidate for candidate in matrix.get("platforms", []) if candidate.get("id") == platform_id), {})
    require(row.get("variant") == expected_variant, f"{platform_id} non-Debian variant/channel drifted")
require(matrix.get("reviewed_on") == "2026-08-18", "matrix review date must be 2026-08-18")
require(matrix.get("fedora", {}).get("43_eol_gate") == "2026-12-02", "Fedora 43 EOL gate drifted")
today = date.fromisoformat(os.environ.get("RELEASE_MATRIX_TODAY", date.today().isoformat()))
fedora_43_eol = date.fromisoformat(matrix["fedora"]["43_eol_gate"])
require(today < fedora_43_eol, f"Fedora 43 reached its {fedora_43_eol.isoformat()} EOL gate; revise the matrix")
require(matrix.get("fedora", {}).get("branched") == "45", "Fedora 45 must remain a separate branched target")
require(matrix.get("fedora", {}).get("rawhide_development_release") == "46", "Rawhide must identify Fedora 46 development")
copr_channels = matrix.get("copr_channels", {})
production_copr = copr_channels.get("production", {})
staging_copr = copr_channels.get("staging", {})
expected_copr_targets = {"fedora-43-x86_64", "fedora-44-x86_64", "fedora-45-x86_64"}
expected_experimental_chroots = {"fedora-rawhide-x86_64"}


def require_string_set(value: object, expected: set[str], description: str) -> set[str]:
    require(
        isinstance(value, list) and all(isinstance(item, str) for item in value),
        f"{description} must be a list of strings",
    )
    actual = set(value)
    require(len(value) == len(actual), f"{description} must not contain duplicates")
    require(actual == expected, f"{description} drifted: {sorted(actual)}")
    return actual


require(production_copr.get("owner") == "tyvsmith", "production COPR owner drifted")
require(production_copr.get("project") == "facelock", "production COPR project drifted")
require(
    production_copr.get("api_url")
    == "https://copr.fedorainfracloud.org/api_3/project?ownername=tyvsmith&projectname=facelock",
    "production COPR public API drifted",
)
required_supported_chroots = require_string_set(
    production_copr.get("required_supported_chroots"),
    expected_copr_targets,
    "production COPR required supported chroots",
)
optional_experimental_chroots = require_string_set(
    production_copr.get("optional_experimental_chroots"),
    expected_experimental_chroots,
    "production COPR optional experimental chroots",
)
require(
    required_supported_chroots.isdisjoint(optional_experimental_chroots),
    "production COPR required supported and optional experimental chroots must be disjoint",
)
require(
    "expected_enabled_chroots" not in production_copr,
    "production COPR authority must separate required supported and optional experimental chroots",
)
require(production_copr.get("prerelease_publication") is False, "production COPR must exclude prereleases")
require(staging_copr.get("project") == "facelock-testing", "staging COPR identity drifted")
require(staging_copr.get("provisioning_issue") == 236, "staging COPR provisioning must remain owned by issue #236")
require(staging_copr.get("managed_by_this_change") is False, "issue #234 cannot provision staging COPR")
staging_copr_targets = require_string_set(
    matrix.get("fedora", {}).get("staging_copr_targets"),
    expected_copr_targets,
    "staging COPR target authority",
)
packit_release_targets = require_string_set(
    matrix.get("fedora", {}).get("packit_release_targets"),
    expected_copr_targets,
    "Packit release target authority",
)
require(matrix.get("arch", {}).get("snapshot") == "2026-08-18", "Arch snapshot drifted")
arch_repository = matrix.get("arch", {}).get("repository")
require(
    arch_repository == "https://archive.archlinux.org/repos/2026/08/18/$repo/os/$arch",
    "Arch archive repository drifted",
)
for row in matrix.get("platforms", []):
    image = row.get("image")
    if image is not None:
        require(
            re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", image) is not None,
            f"{row['id']} image is not digest-pinned: {image!r}",
        )

release_identity = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-(alpha|beta|rc)\.(0|[1-9][0-9]*))?")
release_version = os.environ.get("RELEASE_MATRIX_VERSION")
if release_version is not None:
    require(
        release_identity.fullmatch(release_version) is not None,
        f"invalid RELEASE_MATRIX_VERSION: {release_version!r}",
    )


def workspace_version() -> str:
    match = re.search(r'(?m)^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text())
    require(match is not None, "Cargo.toml must declare the workspace version")
    version = match.group(1)
    require(release_identity.fullmatch(version) is not None, f"invalid Cargo.toml workspace version: {version!r}")
    return version


def version_triple(version: str) -> tuple[int, int, int]:
    major, minor, patch = version.split("-", 1)[0].split(".")
    return (int(major), int(minor), int(patch))


# The version the tree is judged as: the tag under release, otherwise the
# version this tree would tag. Time-boxed contracts measure against it.
gate_version = release_version if release_version is not None else workspace_version()

expected_suites = {"trixie", "resolute"}
apt_suites = matrix.get("apt_suites", {})
suite_map = {suite: details for suite, details in apt_suites.items() if suite != "compat"}
require(set(suite_map) == expected_suites, "canonical APT suite set drifted")
expected_suite_contracts = {
    "trixie": {
        "platform_id": "debian-13",
        "platform": "Debian 13",
        "architecture": "amd64",
        "revision_suffix": "~deb13u1",
    },
    "resolute": {
        "platform_id": "ubuntu-26.04",
        "platform": "Ubuntu 26.04",
        "architecture": "amd64",
        "revision_suffix": "~ubuntu26.04.1",
    },
}
platforms_by_id = {row["id"]: row for row in matrix.get("platforms", [])}
rawhide = platforms_by_id.get("fedora-rawhide", {})
require(rawhide.get("smoke_track") == "Track D", "Rawhide must remain on pinned Track D smoke")
require(rawhide.get("smoke_policy") == "best-effort", "Rawhide smoke must remain best-effort")
require(rawhide.get("alpha_blocking") is False, "a Rawhide-only failure cannot block an alpha")
require(rawhide.get("prerelease_publication") is False, "no prerelease may publish to Rawhide")
require(
    rawhide.get("evidence_eligibility")
    == {
        "lifecycle": False,
        "artifact": False,
        "upgrade": False,
        "rollback": False,
        "served_version": False,
        "availability": False,
    },
    "Rawhide cannot supply release evidence",
)
require(
    rawhide.get("promotion_requires") == "separately reviewed amendment and full Fedora gates",
    "Rawhide promotion contract drifted",
)
# Only Rawhide may carry evidence_eligibility: test/packaging-evidence.py drops
# a row whose lifecycle eligibility is false from the required lane set, so the
# key on any other row would quietly shrink the release gate.
for row in matrix.get("platforms", []):
    require(
        row.get("id") == "fedora-rawhide" or "evidence_eligibility" not in row,
        f"{row.get('id')} declares evidence_eligibility; only the Rawhide row may",
    )
for suite, expected in expected_suite_contracts.items():
    details = suite_map[suite]
    for field, value in expected.items():
        require(details.get(field) == value, f"APT suite {suite} {field} drifted: {details.get(field)!r}")
    platform_row = platforms_by_id.get(expected["platform_id"])
    require(platform_row is not None, f"APT suite {suite} references a missing platform row")
    require(platform_row["architecture"] == expected["architecture"], f"APT suite {suite} architecture disagrees with its platform row")
    require(platform_row["image"] == details.get("image"), f"APT suite {suite} image disagrees with its platform row")
    require("variant" not in details, f"APT suite {suite} retains a Debian package variant axis")
    require("variant" not in platform_row, f"APT suite {suite} platform retains a Debian package variant axis")
    require(platform_row.get("package") == "facelock", f"APT suite {suite} must map to the facelock package")
    require(platform_row.get("required_capabilities") == ["tpm"], f"APT suite {suite} must require TPM")

# The v0.1.4 suite names stay published as compatibility suites so a source
# entry written for 0.1.4 keeps updating: `main` carries trixie's exact package
# set, `legacy` carries signed empty indexes. The window closes at `retire_at`;
# the first tree at or past that version, prerelease included, ships without
# them (#310).
expected_compat_suites = {"main": "trixie", "legacy": "none"}
compat_map = apt_suites.get("compat", {})
require(
    set(compat_map) == set(expected_compat_suites),
    f"compatibility APT suite set drifted: {sorted(compat_map)}",
)
compat_retire_versions = set()
for suite, source in expected_compat_suites.items():
    details = compat_map[suite]
    require(details.get("source") == source, f"compatibility APT suite {suite} source drifted: {details.get('source')!r}")
    retire_at = details.get("retire_at")
    require(
        isinstance(retire_at, str)
        and re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", retire_at) is not None,
        f"compatibility APT suite {suite} has no stable retire_at version: {retire_at!r}",
    )
    compat_retire_versions.add(retire_at)
require(len(compat_retire_versions) == 1, "compatibility APT suites must share one retirement version")
compat_retire_at = compat_retire_versions.pop()
compat_active = version_triple(gate_version) < version_triple(compat_retire_at)
active_compat_suites = set(expected_compat_suites) if compat_active else set()

apt_config = (ROOT / "dist/apt/conf/distributions").read_text()
apt_stanzas: dict[str, dict[str, str]] = {}
for stanza in re.split(r"\n\s*\n", apt_config.strip()):
    fields = dict(re.findall(r"(?m)^([A-Za-z]+):[ \t]*(.*?)[ \t]*$", stanza))
    require("Codename" in fields, f"APT config stanza without a Codename: {stanza!r}")
    require(fields["Codename"] not in apt_stanzas, f"duplicate APT config stanza {fields['Codename']}")
    apt_stanzas[fields["Codename"]] = fields
declared_suites = set(apt_stanzas)
for suite in sorted(set(expected_compat_suites) - active_compat_suites):
    require(
        suite not in declared_suites,
        f"compatibility APT suite {suite} reached retire_at {compat_retire_at}; remove its stanza from dist/apt/conf/distributions",
    )
require(
    declared_suites == expected_suites | active_compat_suites,
    f"APT config suites {sorted(declared_suites)} != {sorted(expected_suites | active_compat_suites)}",
)
for suite, fields in apt_stanzas.items():
    # Origin and Label are compared by every installed client: a change is an
    # apt update failure, not a warning. Suite is not pinned; adding one where
    # none existed passes apt.
    for field, value in (
        ("Origin", "tysmith"),
        ("Label", "Ty Smith Packages"),
        ("Architectures", "amd64"),
        ("Components", "facelock"),
        ("SignWith", "default"),
    ):
        require(fields.get(field) == value, f"APT suite {suite} {field} drifted: {fields.get(field)!r}")
for suite in active_compat_suites:
    description = apt_stanzas[suite].get("Description", "")
    source = expected_compat_suites[suite]
    replacements = {source} if source != "none" else expected_suites
    require(
        "deprecated" in description.lower() and all(name in description for name in replacements),
        f"compatibility APT suite {suite} description must name the deprecation and the codenamed replacement: {description!r}",
    )

try:
    packit = json.loads((ROOT / ".packit.yaml").read_text())
    packit_jobs = packit["jobs"]
    require(isinstance(packit_jobs, list), "Packit jobs must be a list")
except (KeyError, TypeError, json.JSONDecodeError) as error:
    fail(f"Packit config must remain valid JSON-subset YAML: {error}")
for job_index, job in enumerate(packit_jobs):
    if not isinstance(job, dict) or job.get("job") != "copr_build":
        continue
    target_list = job.get("targets")
    target_description = f"Packit copr_build job {job_index} targets"
    require(
        isinstance(target_list, list)
        and bool(target_list)
        and all(isinstance(target, str) and bool(target) for target in target_list),
        f"{target_description} must be a non-empty list of non-empty strings",
    )
    targets = set(target_list)
    require(len(target_list) == len(targets), f"{target_description} must be unique")
    undeclared_targets = targets - packit_release_targets
    require(
        not undeclared_targets,
        f"{target_description} contain undeclared targets outside the explicit allowlist: {sorted(undeclared_targets)}",
    )
    if job.get("project") == staging_copr["project"]:
        require(
            targets == staging_copr_targets,
            f"Packit staging COPR targets drifted: {sorted(targets)}",
        )
production_jobs = [
    job
    for job in packit_jobs
    if isinstance(job, dict) and job.get("job") == "copr_build" and job.get("project") == "facelock"
]
require(len(production_jobs) == 1, f"Packit must define exactly one production COPR job, found {len(production_jobs)}")
production_job = production_jobs[0]
require(
    production_job.get("owner") == production_copr["owner"],
    "Packit production COPR owner disagrees with the release matrix",
)
production_trigger = production_job.get("trigger")
require(
    production_trigger in {"ignore", "release"},
    f"Packit production COPR trigger must be 'ignore' or 'release', got {production_trigger!r}",
)
if release_version is not None and "-" not in release_version:
    require(
        production_trigger == "release",
        "stable release config must deliberately restore the production COPR release job",
    )
elif release_version is not None:
    require(
        production_trigger == "ignore",
        "prerelease-tagged Packit config can select a release-triggered production COPR job",
    )
packit_target_list = production_job.get("targets")
require(
    isinstance(packit_target_list, list) and all(isinstance(target, str) for target in packit_target_list),
    "Packit production COPR targets must be a list of strings",
)
packit_targets = set(packit_target_list)
require(len(packit_target_list) == len(packit_targets), "Packit production COPR targets must be unique")
require(packit_targets == packit_release_targets, f"Packit targets drifted: {sorted(packit_targets)}")

workflow = (ROOT / ".github/workflows/release.yml").read_text()
rpm_spec = (ROOT / "dist/facelock.spec").read_text()
rpm_validator = (ROOT / ".github/workflows/scripts/validate-rpm.sh").read_text()
rpm_package_fixture = (ROOT / "test/Containerfile.rpm-e2e").read_text()
rpm_package_runner = (ROOT / "test/run-pkg-validate-systemd.sh").read_text()
pam_source = (ROOT / "crates/facelock-cli/src/commands/pam.rs").read_text()
setup_source = (ROOT / "crates/facelock-cli/src/commands/setup.rs").read_text()
default_pam_service = re.search(
    r'(?m)^pub const DEFAULT_PAM_SERVICE:\s*&str\s*=\s*"([^"]+)";',
    pam_source,
)
require(default_pam_service is not None, "PAM source must define DEFAULT_PAM_SERVICE")
require(
    default_pam_service.group(1) == "sudo",
    f"bare PAM operations must continue to default to sudo, got {default_pam_service.group(1)!r}",
)
require(
    "facelock-authselect-retirement-guard" in rpm_spec,
    "RPM release spec must install the retired-profile upgrade guard",
)
authselect_dependency = re.compile(
    r"(?mi)^(?:Requires|Recommends)(?:\([^\n)]*\))?:[^\n]*"
    r"(?<![A-Za-z0-9_.+-])authselect(?![A-Za-z0-9_.+-])"
)
require(
    authselect_dependency.search(rpm_spec) is None,
    "RPM release spec must not depend on or recommend authselect",
)
require(
    "authselect/vendor/facelock" not in rpm_spec,
    "RPM release spec must not ship the retired authselect profile",
)
require(
    "authselect/vendor/facelock" not in rpm_validator,
    "RPM release validator must not require the retired authselect profile",
)
require(
    re.search(
        r"(?m)^COPY test/rpm-service-pam-lifecycle\.sh /rpm-service-pam-lifecycle\.sh$",
        rpm_package_fixture,
    )
    is not None,
    "RPM package fixture must include the service-scoped PAM lifecycle",
)
require(
    re.search(
        r'(?m)^\s*podman exec "\$cid" /rpm-service-pam-lifecycle\.sh$',
        rpm_package_runner,
    )
    is not None,
    "booted RPM package runner must execute the service-scoped PAM lifecycle",
)
sensitive_services = set(
    re.findall(r'(?m)^\s*"([^"]+)",\s*$', rust_array_body(pam_source, "SENSITIVE_SERVICES"))
)
candidate_body = rust_array_body(setup_source, "PAM_CANDIDATES")
candidate_services = set(re.findall(r'\bservice:\s*"([^"]+)"', candidate_body))
require(sensitive_services, "PAM sensitive-service authority must not be empty")
require(
    len(candidate_services) == candidate_body.count("PamCandidate {"),
    "every setup PAM candidate must have one parseable service name",
)
require(
    candidate_services.isdisjoint(sensitive_services),
    f"setup PAM candidates include sensitive services: {sorted(candidate_services & sensitive_services)}",
)
for suite, details in suite_map.items():
    expected_block = re.compile(
        rf"(?m)^\s+- suite: {re.escape(suite)}\s*$\n"
        rf"^\s+architecture: {re.escape(details['architecture'])}\s*$\n"
        rf"^\s+image: {re.escape(details['image'])}\s*$"
    )
    require(expected_block.search(workflow) is not None, f"release workflow suite/architecture/image drifted for {suite}")
require("matrix.variant" not in workflow, "release workflow retains the Debian package variant axis")
publication_inputs = re.findall(
    r'"(trixie|resolute)=\$\(exact_deb_from_manifest (trixie|resolute)\)"',
    workflow,
)
require(
    len(publication_inputs) == 2 and set(publication_inputs) == {(suite, suite) for suite in expected_suites},
    f"stable APT publication inputs drifted or duplicated: {publication_inputs}",
)
require(
    workflow.count("exact_deb_from_manifest() {") == 1,
    "stable APT publication must resolve each suite from one exact-manifest helper",
)
deb_builder = (ROOT / ".github/workflows/scripts/build-deb.sh").read_text()
require(
    'ARCHITECTURE="$(dpkg --print-architecture)"' in deb_builder,
    "Debian artifact architecture is not derived from the native suite build",
)
apt_publisher = (ROOT / ".github/workflows/scripts/publish-apt.sh").read_text()
require(
    'source "$SCRIPT_DIR/../../../scripts/release-versions.sh"' in apt_publisher,
    "APT publisher does not source the central release version contract",
)
require(
    'EXPECTED_SUFFIX="$(release_debian_suite_suffix "$SUITE")"' in apt_publisher,
    "APT publisher does not derive suite suffixes from the central release version contract",
)
for suffix in ("~deb13u1", "~ubuntu26.04.1"):
    require(suffix not in apt_publisher, f"APT publisher duplicates the central suite suffix {suffix}")
# The publisher's compatibility steps run only when their stanza is declared,
# so retirement is the stanza deletion and the steps may stay.
for suite in active_compat_suites:
    step = f"includedeb {suite}" if expected_compat_suites[suite] != "none" else f"export {suite}"
    require(
        re.search(rf'(?m)^\s*reprepro -b "\$\{{REPO_DIR\}}" {re.escape(step)}\b', apt_publisher) is not None,
        f"APT publisher does not populate compatibility suite {suite} ({step})",
    )
require(
    "dists pool tysmith-archive-keyring.gpg" in workflow,
    "APT repo artifact must carry the whole dists tree, every published suite included",
)
direct_fedora_image = next(row["image"] for row in matrix["platforms"] if row["id"] == "fedora-44-direct")
require(f"image: {direct_fedora_image}" in workflow, "direct RPM workflow must pin Fedora 44 by digest")
require("prerelease: ${{ needs.metadata.outputs.prerelease }}" in workflow, "GitHub Release prerelease output is not wired")
require(workflow.count("needs.metadata.outputs.prerelease == 'false'") >= 2, "stable APT/AUR guards are not derived from validated metadata")
require("project: facelock" not in workflow, "workflow contains a selectable production COPR project")

arch_image = next(row["image"] for row in matrix["platforms"] if row["id"] == "arch-2026-08-18")
aur_publisher = (ROOT / ".github/workflows/scripts/publish-aur.sh").read_text()
require(arch_image in aur_publisher, "AUR publication helper does not use the immutable Arch matrix image")


def require_arch_mirror_before_each_pacman(relative_path: str) -> None:
    content = (ROOT / relative_path).read_text()
    pacman_commands = list(re.finditer(r"(?m)^\s*(?:RUN\s+)?pacman\s", content))
    require(pacman_commands, f"expected an Arch package invocation in {relative_path}")
    boundary = 0
    for command in pacman_commands:
        require(
            arch_repository in content[boundary : command.start()].replace(r"\$", "$"),
            f"{relative_path} does not configure the exact Arch snapshot before every pacman invocation",
        )
        boundary = command.end()


for arch_consumer in (".github/workflows/ci.yml", ".github/workflows/scripts/publish-aur.sh", "test/Containerfile"):
    require_arch_mirror_before_each_pacman(arch_consumer)
ci_workflow = (ROOT / ".github/workflows/ci.yml").read_text()
justfile = (ROOT / "justfile").read_text()


def workflow_job_body(content: str, job_name: str) -> str:
    match = re.search(
        rf"(?ms)^  {re.escape(job_name)}:\n(?P<body>.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)",
        content,
    )
    require(match is not None, f"workflow omits required job: {job_name}")
    return match.group("body")


ci_agent_docs_job = workflow_job_body(ci_workflow, "agent-docs")
release_deb_job = workflow_job_body(workflow, "build-deb")
for package in ("just", "python", "ripgrep"):
    require(
        re.search(rf"(?m)^\s+{re.escape(package)}(?: \\)?\s*$", ci_agent_docs_job)
        is not None,
        f"CI agent-docs job must install {package} for Debian source-contract validation",
    )
for package in ("just", "python3", "ripgrep"):
    require(
        re.search(rf"(?m)^\s+{re.escape(package)}(?: \\)?\s*$", release_deb_job)
        is not None,
        f"release build-deb job must install {package} for Debian source-contract validation",
    )
require(
    len(
        re.findall(
            r"(?m)^\s+run:\s+just test-deb-source-contract\s*$",
            ci_agent_docs_job,
        )
    )
    == 1,
    "CI agent-docs job must run exactly one combined Debian source-contract target",
)
require(
    len(
        re.findall(
            r"(?m)^\s+just test-deb-source-contract\s*$",
            release_deb_job,
        )
    )
    == 1,
    "release build-deb job must run exactly one combined Debian source-contract target",
)
deb_source_contract_recipe = re.search(
    r"(?m)^test-deb-source-contract:\s*\n(?P<body>(?:^[ \t].*(?:\n|\Z))*)",
    justfile,
)
require(
    deb_source_contract_recipe is not None,
    "justfile omits the Debian source-contract target",
)
deb_source_contract_commands = [
    line.strip()
    for line in deb_source_contract_recipe.group("body").splitlines()
    if line.strip() and not line.lstrip().startswith("#")
]
require(
    deb_source_contract_commands
    == [
        "bash test/deb-source-contract.sh",
        "python3 test/deb-source-contract-test.py",
    ],
    "just Debian source-contract target must run the exact shell and mutation commands",
)
live_channel_command = "python3 test/check-live-release-channels.py"
require(live_channel_command in ci_workflow, "CI does not compare live release channels with the checked-in authority")
require(live_channel_command in justfile, "release preflight does not compare live release channels with the checked-in authority")
require("ARCH_SNAPSHOT" not in justfile, "unused ARCH_SNAPSHOT signaling remains")

# Fedora lifecycle lanes take their base image from this matrix rather than
# carrying a digest each. That is what stops a declared release from going
# unbuilt, stops an undeclared one (Rawhide) from acquiring a lane, and puts
# every lane behind the same EOL gate.
for relative_path in (
    "test/Containerfile.rpm-e2e",
    "test/Containerfile.rpm-authselect",
    "test/Containerfile.copr",
    "test/Containerfile.fedora",
):
    fedora_containerfile = (ROOT / relative_path).read_text()
    require(
        re.search(r"(?m)^ARG BASE_IMAGE\s*$\n^FROM \$\{BASE_IMAGE\}\s*$", fedora_containerfile)
        is not None,
        f"{relative_path} does not take its base image from the release matrix",
    )
    require(
        "registry.fedoraproject.org/fedora:" not in fedora_containerfile,
        f"{relative_path} pins a Fedora image outside the release matrix",
    )
apt_client_containerfile = (ROOT / "test/Containerfile.apt-client").read_text()
require(
    re.search(r"(?m)^ARG BASE_IMAGE\s*$\n^FROM \$\{BASE_IMAGE\}\s*$", apt_client_containerfile) is not None,
    "test/Containerfile.apt-client does not take its base image from the release matrix",
)
require(
    "docker.io/library/debian:" not in apt_client_containerfile,
    "test/Containerfile.apt-client pins a Debian image outside the release matrix",
)
lane_resolver = (ROOT / "test/fedora-lane-image.sh").read_text()
for phrase in ("RELEASE_MATRIX_TODAY", "_eol_gate", "staging_copr_targets"):
    require(phrase in lane_resolver, f"Fedora lane resolver omits matrix authority: {phrase}")
require(
    "bash test/fedora-lane-image.sh" in justfile,
    "justfile Fedora lanes do not resolve their image through the matrix",
)
lanes_recipe = re.search(r"(?m)^test-rpm-lanes:(?P<dependencies>[^\n]*)$", justfile)
require(lanes_recipe is not None, "justfile omits the aggregate Fedora lifecycle lane target")
lane_invocations = re.findall(
    r'\((test-rpm-[a-z-]+)\s+"([^"]+)"\)', lanes_recipe.group("dependencies")
)
lanes_by_release = {release: recipe for recipe, release in lane_invocations}
require(
    len(lane_invocations) == len(lanes_by_release),
    "just test-rpm-lanes invokes a Fedora release more than once",
)
# The matrix declares how deeply each release is tested, so the gate has to
# require that exact recipe. Accepting either one lets a full lifecycle lane be
# downgraded to a smoke lane without anything noticing, which is the regression
# these lanes exist to prevent.
lane_recipe_by_depth = {"full": "test-rpm-pkg", "build/runtime smoke": "test-rpm-smoke"}
declared_fedora_releases = {target.split("-")[1] for target in expected_copr_targets}
require(
    set(lanes_by_release) == declared_fedora_releases,
    f"just test-rpm-lanes covers {sorted(lanes_by_release)}, "
    f"not the declared release targets {sorted(declared_fedora_releases)}",
)
for fedora_release in sorted(declared_fedora_releases):
    lane_depths = {
        row["lifecycle_depth"]
        for row in matrix.get("platforms", [])
        if row.get("release_target") is True
        and re.fullmatch(rf"Fedora {fedora_release}(?: .*)?", row.get("platform", ""))
    }
    require(lane_depths, f"release matrix declares no Fedora {fedora_release} platform row")
    require(
        len(lane_depths) == 1,
        f"Fedora {fedora_release} platform rows disagree on lifecycle depth: {sorted(lane_depths)}",
    )
    lane_depth = lane_depths.pop()
    require(
        lane_depth in lane_recipe_by_depth,
        f"Fedora {fedora_release} lifecycle depth {lane_depth!r} has no mapped lane recipe",
    )
    require(
        lanes_by_release[fedora_release] == lane_recipe_by_depth[lane_depth],
        f"just test-rpm-lanes runs Fedora {fedora_release} through "
        f"{lanes_by_release[fedora_release]}, but its declared lifecycle depth "
        f"{lane_depth!r} requires {lane_recipe_by_depth[lane_depth]}",
    )
# The packaging workflow runs the same Fedora lanes as `just test-rpm-lanes`,
# one job each so they run in parallel rather than end to end. That parallelism
# is the only reason CI restates the list, so it has to restate it exactly --
# otherwise a new Fedora target lands in the matrix and the justfile and is
# quietly absent from the gate that actually runs before a release.
packaging_workflow = (ROOT / ".github/workflows/packaging.yml").read_text()
workflow_fedora_lanes = {
    release: recipe
    for recipe, release in re.findall(
        r"(?m)^\s+recipe: (test-rpm-[a-z-]+) (\d+)\s*$", packaging_workflow
    )
}
require(
    workflow_fedora_lanes == lanes_by_release,
    f"packaging workflow Fedora lanes {workflow_fedora_lanes} differ from "
    f"just test-rpm-lanes {lanes_by_release}",
)
workflow_deb_lanes = set(re.findall(r"(?m)^\s+recipe: test-deb-([a-z]+)-pkg\s*$", packaging_workflow))
require(
    workflow_deb_lanes == expected_suites,
    f"packaging workflow Debian lanes {sorted(workflow_deb_lanes)} are not the "
    f"declared APT suites {sorted(expected_suites)}",
)
# #313: a green packaging.yml conclusion is not release evidence; the lane
# records its jobs upload are. Every lane job uploads them and fails when its
# lane recorded nothing, or `just release-preflight` has nothing to validate
# and quietly falls back to the local marker. The checks read the upload steps
# themselves and the executable lines of the recipe bodies, full-line and
# trailing comments dropped, so text that survives only in a comment or an
# unrelated step satisfies nothing.


def workflow_steps(workflow: str) -> list[str]:
    """Each `- name:`/`- uses:` step of a workflow, comment lines removed."""
    steps: list[list[str]] = []
    current: list[str] | None = None
    step_indent = 0
    for line in workflow.splitlines():
        stripped = line.lstrip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(stripped)
        if re.match(r"- (?:name|uses): ", stripped):
            current = [line]
            step_indent = indent
            steps.append(current)
        elif current is not None and indent > step_indent:
            current.append(line)
        else:
            current = None
    return ["\n".join(step) for step in steps]


def just_recipe_body(source: str, name: str) -> str:
    """The indented body of one justfile recipe."""
    match = re.search(
        rf"(?m)^{re.escape(name)}(?=[\s:])[^\n]*:[^\n]*\n(?P<body>(?:[ \t]+[^\n]*\n|\n)*)", source
    )
    require(match is not None, f"justfile omits recipe {name}")
    return match.group("body")


def shell_command_text(line: str) -> str:
    """One recipe line with its trailing `#` comment removed, quotes respected."""
    quote = None
    for index, char in enumerate(line):
        if quote:
            if char == quote:
                quote = None
        elif char in ("'", '"'):
            quote = char
        elif char == "#" and (index == 0 or line[index - 1].isspace()):
            return line[:index]
    return line


def recipe_commands(body: str) -> list[str]:
    """The executable lines of a recipe body, each stripped of leading
    whitespace, a `@`/`-` recipe prefix, and a leading shell keyword."""
    commands: list[str] = []
    for line in body.splitlines():
        executable = shell_command_text(line).strip()
        if not executable:
            continue
        executable = re.sub(r"^[@-]\s*", "", executable)
        executable = re.sub(r"^(?:if|elif|while|until)\s+", "", executable)
        commands.append(executable)
    return commands


def recipe_runs(commands: list[str], command: str) -> bool:
    """Whether a recipe line starts with `command` as the thing it executes."""
    return any(
        line == command or line.startswith(command + " ") or line.startswith(command + ";")
        for line in commands
    )


# A write into the marker by anything but the evidence script: `> file`,
# `>> file`, or `tee file`.
MARKER_WRITE = re.compile(r"""(?:>{1,2}\s*|\btee\s+(?:-[a-z]+\s+)*)["']?(?:\./)?\.packaging-matrix-verified\b""")


evidence_uploads = [
    step
    for step in workflow_steps(packaging_workflow)
    if re.search(r"(?m)^\s+uses: actions/upload-artifact@", step)
    and re.search(r"(?m)^\s+path: \.packaging-evidence/\s*$", step)
]
# One upload step per lane job: deb, rpm, arch. The two matrix jobs run the
# same step once per entry, so the step count stays three. A new lane job (the
# copr job #230 adds) brings its own upload step and bumps this count.
EVIDENCE_UPLOAD_STEPS = 3
require(
    len(evidence_uploads) == EVIDENCE_UPLOAD_STEPS,
    f"packaging workflow must carry exactly {EVIDENCE_UPLOAD_STEPS} evidence upload steps "
    f"(found {len(evidence_uploads)}); a new lane job adds one and bumps the count",
)
for artifact in ("deb-${{ matrix.suite }}", "rpm-${{ matrix.release }}", "arch"):
    matching = [
        step
        for step in evidence_uploads
        if re.search(rf"(?m)^\s+name: packaging-evidence-{re.escape(artifact)}\s*$", step)
    ]
    require(
        len(matching) == 1,
        f"packaging workflow must upload the packaging-evidence-{artifact} artifact from exactly one step",
    )
    require(
        re.search(r"(?m)^\s+if-no-files-found: error\s*$", matching[0]) is not None,
        f"the packaging-evidence-{artifact} upload must fail the job when its lane recorded nothing",
    )
    # .packaging-evidence/ is dot-prefixed; upload-artifact skips hidden paths
    # unless told otherwise, and then "no files" fails the job.
    require(
        re.search(r"(?m)^\s+include-hidden-files: true\s*$", matching[0]) is not None,
        f"the packaging-evidence-{artifact} upload must set include-hidden-files: true for the dot-prefixed directory",
    )
preflight_commands = recipe_commands(just_recipe_body(justfile, "release-preflight"))
require(
    recipe_runs(preflight_commands, 'python3 test/packaging-evidence.py ci-run --commit "$HEAD_SHA" --run "$run_id"')
    and recipe_runs(
        preflight_commands,
        'python3 test/packaging-evidence.py validate --commit "$HEAD_SHA" .packaging-matrix-verified',
    ),
    "release-preflight does not run test/packaging-evidence.py ci-run and validate as its own commands",
)
matrix_commands = recipe_commands(just_recipe_body(justfile, "test-packaging-matrix"))
require(
    recipe_runs(matrix_commands, 'python3 test/packaging-evidence.py aggregate --commit "$commit" --tree-clean'),
    "test-packaging-matrix does not run test/packaging-evidence.py aggregate as its own command",
)
for command in matrix_commands + preflight_commands:
    require(
        "test/packaging-evidence.py" in command or MARKER_WRITE.search(command) is None,
        f"only test/packaging-evidence.py may write .packaging-matrix-verified: {command!r}",
    )
require(
    "rawhide" not in packaging_workflow.lower(),
    "packaging workflow declares a Rawhide lane; Rawhide is experimental and never a gate",
)
# GitHub's own path filter skips the whole workflow, and a required check that
# never runs reports "pending" forever, so the gate classifies in a job instead.
require(
    re.search(r"(?m)^\s+paths(?:-ignore)?:", packaging_workflow) is None,
    "packaging workflow uses GitHub's paths: filter; a skipped required check never reports",
)
require(
    "classify-changes.sh" in packaging_workflow,
    "packaging workflow does not derive its path filter from classify-changes.sh",
)
require(
    platforms_by_id.get("fedora-43", {}).get("eol_gate") == matrix["fedora"]["43_eol_gate"],
    "Fedora 43 platform row EOL gate disagrees with the matrix-wide gate",
)

for pkgbuild_name in ("PKGBUILD", "PKGBUILD-bin"):
    pkgbuild = (ROOT / "dist" / pkgbuild_name).read_text()
    require(re.search(r"^_tag=", pkgbuild, re.MULTILINE) is not None, f"dist/{pkgbuild_name} has no upstream _tag")
    require("v$_tag" in pkgbuild, f"dist/{pkgbuild_name} does not fetch the upstream _tag")

# Every Arch recipe installs a binary that dlopens libonnxruntime at runtime
# (ort's load-dynamic feature), and nothing else in the declared dependency
# closure provides it, so each must name it in depends (#284).
for pkgbuild_name in ("PKGBUILD", "PKGBUILD-bin", "PKGBUILD-git"):
    pkgbuild = (ROOT / "dist" / pkgbuild_name).read_text()
    depends = re.search(r"^depends=\(([^)]*)\)", pkgbuild, re.MULTILINE)
    require(depends is not None, f"dist/{pkgbuild_name} has no depends array")
    require(
        "'onnxruntime'" in depends.group(1),
        f"dist/{pkgbuild_name} does not declare the onnxruntime runtime dependency",
    )

# dist/PKGBUILD fetches a fixed release tarball, so its digest line must be
# the publish-time placeholder or a pinned digest — never fail-open SKIP (#283).
pkgbuild_source = (ROOT / "dist" / "PKGBUILD").read_text()
require(
    re.search(r"^sha256sums=\('(?:__SRC_SHA256__|[0-9a-f]{64})'\)$", pkgbuild_source, re.MULTILINE) is not None,
    "dist/PKGBUILD does not pin its source tarball digest",
)

package_assemblers = (
    "dist/PKGBUILD",
    "dist/PKGBUILD-bin",
    "dist/PKGBUILD-git",
    "dist/facelock.spec",
    "debian/rules",
    ".github/workflows/scripts/build-deb.sh",
    "dist/nix/default.nix",
)
retired_downstream_components = (
    ("omarchy", "omarchy"),
    ("setup-security-face", "setupsecurityface"),
    ("remove-security-face", "removesecurityface"),
    ("security-face", "securityface"),
)
for package_assembler in package_assemblers:
    assembler = (ROOT / package_assembler).read_text().casefold()
    if package_assembler == "dist/facelock.spec":
        # RPM scriptlets own lifecycle cleanup, not payload assembly. Scan the
        # source declarations, %install commands, and %files manifest; this
        # keeps the active downstream PAM service in %preun outside #173 while
        # making every way the spec can ship a helper part of this guard.
        spec_header, separator, _ = assembler.partition("\n%prep\n")
        require(bool(separator), "dist/facelock.spec has no %prep boundary")
        install_section = re.search(r"(?ms)^%install\n(.*?)(?=^%check\n)", assembler)
        # `%files` carries the %%find_lang manifest argument, so the section
        # header is not a bare directive any more.
        files_section = re.search(r"(?ms)^%files\b[^\n]*\n(.*?)(?=^%changelog\n)", assembler)
        require(install_section is not None, "dist/facelock.spec has no bounded %install section")
        require(files_section is not None, "dist/facelock.spec has no bounded %files section")
        assembler = spec_header + install_section.group(1) + files_section.group(1)
    normalized_assembler = re.sub(r"[^a-z0-9]+", "", assembler)
    for component_name, normalized_component in retired_downstream_components:
        require(
            normalized_component not in normalized_assembler,
            f"{package_assembler} still contains retired downstream-integration component {component_name}",
        )

current_integration_docs = (
    "README.md",
    "docs/contracts.md",
    "docs/integrating.md",
    "docs/adr/009-cli-verb-noun-shape.md",
)
retired_helper_references = (
    "dist/omarchy/",
    "omarchy-setup-security-face",
    "omarchy-remove-security-face",
)
for integration_doc in current_integration_docs:
    doc_content = (ROOT / integration_doc).read_text()
    for retired_reference in retired_helper_references:
        require(
            retired_reference not in doc_content,
            f"{integration_doc} still presents retired downstream-integration helper {retired_reference}",
        )

integration_doc = (ROOT / "docs/integrating.md").read_text()
normalized_integration_doc = re.sub(r"\\\s*\n\s*", " ", integration_doc)
capability_gate_match = re.search(r"^for required in ([^;]+); do$", integration_doc, re.MULTILINE)
require(capability_gate_match is not None, "docs/integrating.md has no capability gate")
capability_gate = set(capability_gate_match.group(1).split())
command_capabilities = (
    (r"facelock is-enrolled\b", "is-enrolled"),
    (r"facelock pam status\b", "pam-status"),
    (r"facelock pam status[^\n]*--json\b", "pam-json"),
    (r"facelock pam (?:add|remove)[^\n]*(?:--service[^\n]*){2}", "pam-multi-service"),
    (r"facelock pam (?:add|remove|status)[^\n]*--if-present\b", "pam-if-present"),
    (r"facelock setup[^\n]*--no-pam\b", "setup-no-pam"),
    (r"facelock setup[^\n]*--systemd\b", "setup-systemd"),
)
invoked_capabilities = {
    capability
    for command_pattern, capability in command_capabilities
    if re.search(command_pattern, normalized_integration_doc) is not None
}
missing_capabilities = sorted(invoked_capabilities - capability_gate)
extra_capabilities = sorted(capability_gate - invoked_capabilities)
require(
    not missing_capabilities and not extra_capabilities,
    "docs/integrating.md capability gate does not match invoked commands: "
    f"missing {','.join(missing_capabilities) if missing_capabilities else 'none'}; "
    f"extra {','.join(extra_capabilities) if extra_capabilities else 'none'}",
)

docs = (ROOT / "docs/releasing.md").read_text()
normalized_docs = re.sub(r"\s+", " ", docs)
for phrase in (
    "0.2.0~alpha.1-1~deb13u1",
    "0.2.0-0.1.alpha.1",
    "0.2.0alpha1-1",
    "Fedora 43",
    "2026-12-02",
    "Fedora 45 branched",
    "Fedora Rawhide (Fedora 46 development)",
    "optional experimental production COPR chroot",
    "best-effort pinned Track D smoke only",
    "not a release target",
    "Every Packit `copr_build` target must be an explicit member of the checked-in allowlist",
    "Mutable aliases such as `fedora-all`, `fedora-development`, and their architecture-suffixed forms are rejected",
    "cannot supply lifecycle, artifact, upgrade, rollback, served-version, or availability evidence",
    "separately reviewed amendment and full Fedora gates",
    "Arch Linux Archive snapshot 2026-08-18",
    "stable-tagged config",
):
    require(phrase in normalized_docs, f"release documentation omits matrix/version phrase: {phrase}")

contracts = (ROOT / "docs/contracts.md").read_text()
normalized_contracts = re.sub(r"\s+", " ", contracts)
for phrase in (
    "## Release Channels and APT Paths",
    "https://tysmith.me/facelock/apt/dists/trixie/Release",
    "https://tysmith.me/facelock/apt/dists/resolute/Release",
    "stable APT, stable AUR, or production COPR",
    "required supported production COPR chroots are exactly Fedora 43, Fedora 44, and Fedora 45",
    "Rawhide is the only optional allowed experimental production chroot",
    "Every Packit `copr_build` target must be an explicit member of the checked-in allowlist",
    "Mutable aliases such as `fedora-all`, `fedora-development`, and their architecture-suffixed forms are rejected",
    "Rawhide is not a Packit staging or production release target",
    "no alpha may publish to Rawhide",
    "Rawhide cannot supply lifecycle, artifact, upgrade, rollback, served-version, or availability evidence",
    "separately reviewed amendment and full Fedora gates",
    "issue #236",
):
    require(phrase in normalized_contracts, f"system contracts omit release-channel phrase: {phrase}")

release_skill = (ROOT / ".claude/skills/release/SKILL.md").read_text()
for phrase in (
    "Tags are parsed strictly as `vX.Y.Z` or `vX.Y.Z-{alpha,beta,rc}.N`.",
    "A bare invocation derives the version from `Cargo.toml` and classifies it with the same parser.",
):
    require(phrase in release_skill, f"release skill omits strict preflight classification: {phrase}")
require("a tag matching" not in release_skill, "release skill still describes substring prerelease classification")
require("Running it bare skips" not in release_skill, "release skill still claims bare preflight skips classification")

copr_build_test = (ROOT / "test/copr-build.sh").read_text()
packit_schema_command = "packit config validate --offline -c .packit.yaml"
require(
    packit_schema_command in copr_build_test,
    "COPR-equivalent gate does not run Packit's real offline schema validator",
)
packit_gate = (ROOT / "test/packit-config-validate.sh").read_text()
packit_gate_image = (ROOT / "test/Containerfile.packit").read_text()
require(
    packit_schema_command in packit_gate,
    "Packit config gate does not run Packit's real offline schema validator",
)
require(
    re.search(r"(?m)^FROM \S*/fedora:\S*@sha256:[0-9a-f]{64}\s*$", packit_gate_image) is not None,
    "Packit config gate image is not a digest-pinned Fedora",
)
require(
    "test/packit-config-validate.sh" in justfile,
    "release preflight does not run the containerized Packit schema gate",
)
require(
    packit_schema_command not in justfile,
    "release preflight still runs Packit from the host instead of the pinned container",
)
require(
    "SKIP: packit" not in justfile,
    "release preflight can still skip Packit schema validation",
)
copr_chroot_default = re.search(r'(?m)^CHROOT="\$\{COPR_CHROOT:-([^"}]+)\}"', copr_build_test)
require(copr_chroot_default is not None, "COPR-equivalent gate has no parseable chroot default")
require(
    copr_chroot_default.group(1) in expected_copr_targets,
    f"COPR-equivalent gate defaults to an undeclared chroot: {copr_chroot_default.group(1)}",
)
require(
    "staging_copr_targets" in copr_build_test,
    "COPR-equivalent gate does not check its chroot against the declared staging targets",
)

install_docs = {
    "README.md": (ROOT / "README.md").read_text(),
    "book/src/quickstart.md": (ROOT / "book/src/quickstart.md").read_text(),
    "website/index.html": (ROOT / "website/index.html").read_text(),
}
apt_platform_mappings = (
    ("Debian 13", "trixie", "TPM"),
    ("Ubuntu 26.04", "resolute", "TPM"),
)
retired_apt_source = re.compile(
    r"https://tysmith\.me/facelock/apt\s+(?:main|legacy)\s+facelock",
    re.IGNORECASE,
)
for relative_path, content in install_docs.items():
    require(retired_apt_source.search(content) is None, f"{relative_path} still configures a retired APT suite")
    require("https://tysmith.me/facelock/apt" in content, f"{relative_path} omits the public APT base")
    for platform, suite, capability in apt_platform_mappings:
        mapping = re.compile(
            rf"(?im)^.*{re.escape(platform)}.*{re.escape(suite)}.*{re.escape(capability)}.*$"
        )
        require(mapping.search(content) is not None, f"{relative_path} omits {platform}/{suite}/{capability} capability mapping")

readme = install_docs["README.md"]
require("two suite-specific `.deb` artifacts" in readme, "README release wording does not name the two Debian artifacts")
roadmap = (ROOT / "docs/testing-roadmap.md").read_text()
for phrase in (
    "two suite-specific `.deb` artifacts",
    "trixie and resolute",
):
    require(phrase in roadmap, f"testing roadmap omits release artifact inventory: {phrase}")
require(retired_apt_source.search(roadmap) is None, "testing roadmap still names retired APT suites")

debian_support_phrase = (
    "Debian-family release support is exactly Debian 13 (Trixie) and "
    "Ubuntu 26.04 LTS (Resolute)."
)
debian_source_phrases = (
    "Trixie package builds use the official Trixie Backports `cargo` and `rustc`",
    "Both codenamed suites ship one binary package named `facelock` with TPM support enabled.",
    "No `rustup` toolchain participates in Debian source builds.",
    "deterministic Cargo-vendor component",
    "network denied and empty Cargo/Rustup caches",
    "exactly two suite manifests",
    "Bookworm and Noble artifacts may remain in historical releases, but those suites are unsupported and receive no new packages.",
)
for relative_path in ("README.md", "docs/releasing.md", "docs/contracts.md", "docs/security.md"):
    content = re.sub(r"\s+", " ", (ROOT / relative_path).read_text())
    require(debian_support_phrase in content, f"{relative_path} omits the exact Debian-family support floor")
normalized_releasing = re.sub(r"\s+", " ", (ROOT / "docs/releasing.md").read_text())
for phrase in debian_source_phrases:
    require(phrase in normalized_releasing, f"release documentation omits Debian source policy: {phrase}")
    require(phrase in normalized_contracts, f"system contracts omit Debian source policy: {phrase}")

# The window is a promise to clients that are not reading this repository, so
# every install document carries it while it is open, and none may keep the
# present-tense promise once it has closed.
compat_window_claims = {
    "docs/contracts.md": (
        normalized_contracts,
        ("`main` and `legacy` are compatibility suites", f"removed at {compat_retire_at}"),
        "`main` and `legacy` are compatibility suites",
    ),
    "docs/releasing.md": (
        normalized_releasing,
        (f"compatibility suites present until {compat_retire_at}",),
        f"compatibility suites present until {compat_retire_at}",
    ),
    "README.md": (
        re.sub(r"\s+", " ", readme),
        (
            "`main` or `legacy`",
            f"keep working until {compat_retire_at}",
            "`apt update` fails until the entry is removed",
        ),
        f"keep working until {compat_retire_at}",
    ),
    "book/src/quickstart.md": (
        re.sub(r"\s+", " ", install_docs["book/src/quickstart.md"]),
        (
            "`main` or `legacy`",
            f"keep working until {compat_retire_at}",
            "`apt update` fails until the entry is removed",
        ),
        f"keep working until {compat_retire_at}",
    ),
    "website/index.html": (
        re.sub(r"\s+", " ", install_docs["website/index.html"]),
        (
            "<code>main</code> or <code>legacy</code>",
            f"keep working until {compat_retire_at}",
            "<code>apt update</code> fails until the entry is removed",
        ),
        f"keep working until {compat_retire_at}",
    ),
}
for relative_path, (content, claims, promise) in compat_window_claims.items():
    if compat_active:
        for claim in claims:
            require(claim in content, f"{relative_path} omits the APT compatibility window: {claim}")
    else:
        require(
            promise not in content,
            f"{relative_path} still promises the APT compatibility window past {compat_retire_at}: {promise}",
        )

compatibility_paths = ("docs/compatibility.md", "book/src/compatibility.md")
compatibility_support_floor = debian_support_phrase
tested_debian_family_rows = (
    "| Debian 13 (Trixie) | systemd | daemon + D-Bus activation | Booted package gate |",
    "| Ubuntu 26.04 LTS (Resolute) | systemd | daemon + D-Bus activation | Booted package gate |",
)
debian_family_row = re.compile(r"(?m)^\|[ \t]*(?:Debian|Ubuntu)\b.*\|[ \t]*$")
for relative_path in compatibility_paths:
    content = (ROOT / relative_path).read_text()
    tested_match = re.search(
        r"(?ms)^## Tested Distributions\s*$\n(.*?)(?=^#{1,6}\s|\Z)",
        content,
    )
    require(tested_match is not None, f"{relative_path} omits the tested distributions section")
    untested_match = re.search(
        r"(?ms)^### Expected to Work \(untested\)\s*$\n(.*?)(?=^#{1,6}\s|\Z)",
        content,
    )
    require(untested_match is not None, f"{relative_path} omits the untested distributions section")
    untested_debian_family_rows = tuple(debian_family_row.findall(untested_match.group(1)))
    require(not untested_debian_family_rows, f"{relative_path} classifies a supported Debian-family row as untested")
    normalized_content = re.sub(r"\s+", " ", content)
    require(
        compatibility_support_floor in normalized_content,
        f"{relative_path} omits the exact Debian-family support floor",
    )
    actual_tested_debian_family_rows = tuple(debian_family_row.findall(tested_match.group(1)))
    require(
        actual_tested_debian_family_rows == tested_debian_family_rows,
        f"{relative_path} tested Debian-family rows differ: {actual_tested_debian_family_rows!r}",
    )

compatibility = (ROOT / "docs/compatibility.md").read_text()
require("Ubuntu 22.04+" not in compatibility, "compatibility guide still claims Ubuntu 22.04 support")
require("Debian 12+" not in compatibility, "compatibility guide still claims Debian 12 support")

active_support_docs = (
    "README.md",
    "CONTRIBUTING.md",
    "docs/quickstart.md",
    "docs/compatibility.md",
    "book/src/quickstart.md",
    "book/src/contributing.md",
    "book/src/compatibility.md",
    ".github/ISSUE_TEMPLATE/bug_report.md",
    "website/index.html",
)
stale_support_claims = (
    "Rust 1.85+",
    "(1.85+)",
    "Ubuntu 22.04+",
    "Debian 12+",
    "Ubuntu 24.04",
)
for relative_path in active_support_docs:
    content = (ROOT / relative_path).read_text()
    for stale_claim in stale_support_claims:
        require(stale_claim not in content, f"{relative_path} retains stale support claim: {stale_claim}")
for relative_path in (
    "CONTRIBUTING.md",
    "docs/quickstart.md",
    "book/src/quickstart.md",
    "book/src/contributing.md",
    "book/src/compatibility.md",
    "website/index.html",
):
    require("1.88+" in (ROOT / relative_path).read_text(), f"{relative_path} omits the Rust 1.88+ floor")
for relative_path in compatibility_paths:
    require(
        "download-binaries" not in (ROOT / relative_path).read_text(),
        f"{relative_path} falsely claims that the disabled ort download-binaries feature supplies the runtime",
    )
packaging_skill = (ROOT / ".claude/skills/packaging-test/SKILL.md").read_text()
for recipe in ("just test-deb-trixie-pkg", "just test-deb-resolute-pkg"):
    require(recipe in packaging_skill, f"packaging skill omits {recipe}")
for retired_recipe in ("just test-deb-pkg", "just test-deb-tpm-pkg"):
    require(retired_recipe not in packaging_skill, f"packaging skill retains {retired_recipe}")

print("release matrix contract: OK")
