#!/usr/bin/env python3
"""Compare public release-channel state with the checked-in authority."""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "dist" / "release-matrix.json"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def load_json(path: Path, description: str) -> object:
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {description}: {error}")


def chroot_set(value: object, description: str) -> set[str]:
    if not isinstance(value, list) or not all(isinstance(chroot, str) for chroot in value):
        fail(f"release matrix {description} are invalid")
    chroots = set(value)
    if len(chroots) != len(value):
        fail(f"release matrix {description} contain duplicates")
    return chroots


parser = argparse.ArgumentParser()
parser.add_argument(
    "--channel",
    choices=("production", "staging"),
    default="production",
    help="which COPR channel to compare (default: production)",
)
parser.add_argument(
    "--response-file",
    type=Path,
    help="read a COPR project response fixture instead of the public API",
)
args = parser.parse_args()

matrix = load_json(MATRIX_PATH, "release matrix")
try:
    channel = matrix["copr_channels"][args.channel]
    owner = channel["owner"]
    project = channel["project"]
    api_url = channel["api_url"]
    required_chroot_list = channel["required_supported_chroots"]
    required_forge_project = channel["required_forge_project"]
except (KeyError, TypeError) as error:
    fail(f"release matrix has no complete {args.channel} COPR authority: {error}")

if not isinstance(required_forge_project, str) or not required_forge_project:
    fail(f"release matrix {args.channel} COPR required forge project must be a non-empty string")

required_chroots = chroot_set(required_chroot_list, f"{args.channel} COPR required supported chroots")
# Production tolerates one optional experimental chroot (Rawhide); staging has
# no such allowance, so its allowed set is exactly the supported set. Neither
# side defaults: a production authority that lost its optional list, or a
# staging authority that grew one, is drift in the file this checker trusts.
declared_optional = channel.get("optional_experimental_chroots")
if args.channel == "production":
    if declared_optional is None:
        fail("release matrix production COPR authority omits its optional experimental chroots")
elif declared_optional is not None:
    fail("release matrix staging COPR authority must declare no optional experimental chroots")
optional_chroots = chroot_set(
    declared_optional if declared_optional is not None else [],
    f"{args.channel} COPR optional experimental chroots",
)
if not required_chroots.isdisjoint(optional_chroots):
    fail(f"release matrix {args.channel} COPR required and optional chroots overlap")
allowed_chroots = required_chroots | optional_chroots

# The provisioning switch decides whether a channel may be skipped, so it is
# read strictly. Only an explicit false skips: a missing or malformed switch
# queries, because a channel that cannot say it is unprovisioned has not said
# it. Production declares no switch at all and therefore always queries.
declared_provisioned = channel.get("provisioned")
if args.channel == "production":
    if declared_provisioned is not None:
        fail("release matrix production COPR authority must not declare a provisioning switch")
elif not isinstance(declared_provisioned, bool):
    fail(
        "release matrix staging COPR authority must declare a boolean provisioning switch: "
        f"{declared_provisioned!r}"
    )

# A response fixture is the project answering, so it is always compared. Without
# one, an unprovisioned channel reports that and queries nothing: issue #236
# owns creating the staging project, and a checker that invented a verdict for a
# project that does not exist would be the failure this gate exists to prevent.
if args.response_file:
    response = load_json(args.response_file, f"{args.channel} COPR response fixture")
elif declared_provisioned is False:
    provisioning_issue = channel.get("provisioning_issue")
    owed_to = f"; issue #{provisioning_issue} owns it" if provisioning_issue else ""
    print(
        f"live release channel contract: SKIPPED ({args.channel} COPR "
        f"{owner}/{project} is not provisioned{owed_to})"
    )
    raise SystemExit(0)
else:
    request = urllib.request.Request(api_url, headers={"User-Agent": "facelock-release-matrix/1"})
    try:
        with urllib.request.urlopen(request, timeout=20) as remote:
            response = json.load(remote)
    except (OSError, urllib.error.URLError, json.JSONDecodeError) as error:
        fail(f"cannot read the public {args.channel} COPR API: {error}")

if not isinstance(response, dict):
    fail(f"{args.channel} COPR API response is not an object")
expected_full_name = f"{owner}/{project}"
if (
    response.get("ownername") != owner
    or response.get("name") != project
    or response.get("full_name") != expected_full_name
):
    fail(
        f"{args.channel} COPR identity drifted: "
        f"expected {expected_full_name}, "
        f"got owner={response.get('ownername')!r}, name={response.get('name')!r}, "
        f"full_name={response.get('full_name')!r}"
    )
# Two project settings docs/releasing.md promises, checked here because the same
# response already carries them and neither is inferable from the chroot set.
# Both are public: COPR returns them to an anonymous caller for a project it does
# not own, so this holds from a fork. Builder permission is not here; COPR serves
# project permissions only to an authenticated owner, so the release guide keeps
# it as a setup step rather than a claim this gate can make.
if response.get("enable_net") is not True:
    fail(
        f"{args.channel} COPR {owner}/{project} builds have no internet access "
        f"(enable_net={response.get('enable_net')!r}); the RPM builds from source and cargo "
        "fetches crates during %build, so a build without it fails resolving crates. "
        'Enable Settings -> "Enable internet access during builds" in the COPR web UI'
    )
forge_projects = response.get("packit_forge_projects_allowed")
if not isinstance(forge_projects, list) or not all(isinstance(entry, str) for entry in forge_projects):
    fail(
        f"{args.channel} COPR API response has no valid packit_forge_projects_allowed list: "
        f"{forge_projects!r}"
    )
if required_forge_project not in forge_projects:
    fail(
        f"{args.channel} COPR {owner}/{project} does not accept Packit builds from "
        f"{required_forge_project}; allowed={sorted(forge_projects)}. "
        'Add it under Settings -> "allowed forge projects" in the COPR web UI'
    )

chroot_repos = response.get("chroot_repos")
if not isinstance(chroot_repos, dict) or not all(isinstance(chroot, str) for chroot in chroot_repos):
    fail(f"{args.channel} COPR API response has no valid chroot_repos object")

live_chroots = set(chroot_repos)
missing = sorted(required_chroots - live_chroots)
extra = sorted(live_chroots - allowed_chroots)
if missing or extra:
    fail(
        f"{args.channel} COPR {owner}/{project} chroots drifted; "
        f"required={sorted(required_chroots)}, optional={sorted(optional_chroots)}, "
        f"live={sorted(live_chroots)}, "
        f"missing={missing}, extra={extra}"
    )

print(
    f"live release channel contract: OK ({args.channel} COPR {owner}/{project}; "
    f"live={sorted(live_chroots)}; enable_net=True; "
    f"packit forge={required_forge_project})"
)
