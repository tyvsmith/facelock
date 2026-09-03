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
except (KeyError, TypeError) as error:
    fail(f"release matrix has no complete {args.channel} COPR authority: {error}")

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

# A response fixture is the project answering, so it is always compared. Without
# one, an unprovisioned channel reports that and queries nothing: issue #236
# owns creating the staging project, and a checker that invented a verdict for a
# project that does not exist would be the failure this gate exists to prevent.
if args.response_file:
    response = load_json(args.response_file, f"{args.channel} COPR response fixture")
elif channel.get("provisioned") is not True:
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
    f"live={sorted(live_chroots)})"
)
