#!/usr/bin/env python3
"""Compare public release-channel state with the checked-in authority.

Two halves. The project half compares a COPR project's enabled chroots with the
targets the release matrix declares. The served half, requested with
`--expect-evr` or `--expect-predecessor`, compares the EVR the project's latest
succeeded build carries with the EVR that release should have produced.

Exit status is 0 when both halves hold, 1 on a verdict that will not change on
its own, and 2 when the expected build has simply not arrived yet -- a build
still running, or none submitted. Only a poller distinguishes the last two; for
everyone else 2 is a failure like any other.
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "dist" / "release-matrix.json"
# COPR build states a build never leaves. `skipped` is one of them: COPR uses it
# when the same NVR was already built, and that earlier build is what
# `latest_succeeded` reports. Every other state -- running, pending, importing,
# and the ones COPR may add later -- is read as "not yet", so a state this file
# has not heard of costs a wait rather than a wrong verdict.
TERMINAL_STATES = frozenset({"failed", "canceled", "cancelled", "skipped"})


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def pending(message: str) -> None:
    """Exit 2: not yet, and it might still arrive. Any non-poller reads this as failure."""
    print(f"PENDING: {message}", file=sys.stderr)
    raise SystemExit(2)


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
served = parser.add_mutually_exclusive_group()
served.add_argument(
    "--expect-evr",
    help="also require the channel to serve this RPM EVR as its latest build",
)
served.add_argument(
    "--expect-predecessor",
    action="store_true",
    help="also require the channel to serve the released predecessor the matrix pins",
)
parser.add_argument(
    "--package-response-file",
    type=Path,
    help="read a COPR package response fixture instead of the public API",
)
args = parser.parse_args()
# An expectation given as an empty string is a caller whose EVR came out empty,
# not a caller asking for nothing. Skipping the served half there would report a
# green verification of nothing at all.
if args.expect_evr is not None and not args.expect_evr:
    fail("--expect-evr was given an empty EVR")
if args.package_response_file and not (args.expect_evr or args.expect_predecessor):
    fail("--package-response-file needs an expected EVR to compare it against")

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
    # Nothing to compare against, including any EVR the caller asked for: an
    # unprovisioned channel has no project, so this says so rather than
    # reporting a served version it never looked up.
    asked = " and its served EVR" if (args.expect_evr or args.expect_predecessor) else ""
    print(
        f"live release channel contract: SKIPPED ({args.channel} COPR "
        f"{owner}/{project} is not provisioned{owed_to}; its chroots{asked} went unchecked)"
    )
    raise SystemExit(0)
else:
    request = urllib.request.Request(api_url, headers={"User-Agent": "facelock-release-matrix/1"})
    try:
        with urllib.request.urlopen(request, timeout=20) as remote:
            response = json.load(remote)
    except (OSError, urllib.error.URLError, json.JSONDecodeError) as error:
        # A read that did not happen is not a verdict. Preflight and CI fail on
        # this either way; the release poller retries instead of reddening a
        # published release over one 5xx from COPR.
        pending(f"cannot read the public {args.channel} COPR API: {error}")

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
        'Enable Settings -> "Enable internet access during builds" in the COPR web UI. '
        "That toggle is only the default for a build that carries no value of its own; "
        "a Packit-submitted build carries one, so .packit.yaml must declare "
        "enable_net: true too"
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

# Enabled chroots prove the project is shaped right; they prove nothing about
# what it serves. v0.1.4 failed Packit's build submission on every target and
# the project stayed correct, so this half of the checker existed and passed
# for the three months COPR served 0.1.3 (#333). Comparing the served EVR is
# what turns that into a release-time failure.
if not (args.expect_evr or args.expect_predecessor):
    raise SystemExit(0)

if args.expect_predecessor:
    predecessors = matrix.get("predecessors")
    if not isinstance(predecessors, dict):
        fail("release matrix declares no predecessors block")
    tags = sorted(tag for tag in predecessors if tag.startswith("v"))
    if len(tags) != 1:
        fail(f"release matrix must pin exactly one released predecessor, found {tags}")
    expected_evr = predecessors[tags[0]].get("rpm_evr")
    if not isinstance(expected_evr, str) or not expected_evr:
        fail(f"release matrix predecessor {tags[0]} declares no RPM EVR")
    expected_for = f" (pinned predecessor {tags[0]})"
else:
    expected_evr = args.expect_evr
    expected_for = ""

package = channel.get("package")
package_api_url = channel.get("package_api_url")
if not isinstance(package, str) or not package:
    fail(f"release matrix {args.channel} COPR authority names no package")
if not isinstance(package_api_url, str) or not package_api_url:
    fail(f"release matrix {args.channel} COPR authority declares no package API URL")

if args.package_response_file:
    package_response = load_json(args.package_response_file, f"{args.channel} COPR package fixture")
else:
    package_request = urllib.request.Request(
        package_api_url, headers={"User-Agent": "facelock-release-matrix/1"}
    )
    try:
        with urllib.request.urlopen(package_request, timeout=20) as remote:
            package_response = json.load(remote)
    except (OSError, urllib.error.URLError, json.JSONDecodeError) as error:
        pending(f"cannot read the public {args.channel} COPR package API: {error}")

if not isinstance(package_response, dict):
    fail(f"{args.channel} COPR package API response is not an object")
if (
    package_response.get("ownername") != owner
    or package_response.get("projectname") != project
    or package_response.get("name") != package
):
    fail(
        f"{args.channel} COPR package identity drifted: "
        f"expected {owner}/{project}/{package}, "
        f"got owner={package_response.get('ownername')!r}, "
        f"project={package_response.get('projectname')!r}, "
        f"name={package_response.get('name')!r}"
    )

builds = package_response.get("builds") or {}
if not isinstance(builds, dict):
    fail(f"{args.channel} COPR package API response has no valid builds object")


def build_evr(build: object) -> str | None:
    if not isinstance(build, dict):
        return None
    version = (build.get("source_package") or {}).get("version")
    return version if isinstance(version, str) else None


def build_chroots(build: object) -> set[str]:
    chroots = build.get("chroots") if isinstance(build, dict) else None
    if not isinstance(chroots, list):
        return set()
    return {chroot for chroot in chroots if isinstance(chroot, str)}


# What the channel serves is the latest *succeeded* build. `latest` is only the
# most recently submitted one -- a rebuild, a retry, a backfill -- and reading
# the served version off it would report a channel as behind while it is still
# serving the right EVR.
succeeded = builds.get("latest_succeeded")
latest = builds.get("latest")
served_evr = build_evr(succeeded)
# Reported, never required. A build's chroot list is what that build covered,
# which is not what the repository serves: a single-chroot rebuild -- the
# documented recovery from a failed submission -- becomes the newest succeeded
# build while the earlier complete one still serves the other two. Requiring
# every chroot here would call that channel broken. The enabled-chroot contract
# is the project half's, above.
served_chroots = build_chroots(succeeded)


# How strictly this channel's EVR is read, decided by whether its Packit job
# pins the release. Production sets `update_release: false`, so it publishes the
# EVR the conversion table promises and is compared exactly. Staging keeps
# Packit's `1.{timestamp}.{ref}` suffix, which its per-pull-request NVRs need,
# so its comparison ends at the boundary dot instead. `check-release-matrix.py`
# holds this value and the Packit flag together; neither moves alone.
exact = channel.get("served_evr_exact")
if not isinstance(exact, bool):
    fail(
        f"release matrix {args.channel} COPR authority must declare a boolean "
        f"served EVR comparison: {exact!r}"
    )


def carries(evr: str | None, wanted: str) -> bool:
    """Is `evr` the build `wanted` names? Exactly, or under a release suffix."""
    if evr is None:
        return False
    if exact:
        return evr == wanted
    # The boundary dot keeps `0.2.0-1` from swallowing `0.2.0-11`.
    return evr == wanted or evr.startswith(f"{wanted}.")


def matches(evr: str | None) -> bool:
    return carries(evr, expected_evr)


if matches(served_evr):
    print(
        f"served release channel contract: OK ({args.channel} COPR {owner}/{project} "
        f"serves {package}-{served_evr}; newest build covered {sorted(served_chroots)})"
    )
    raise SystemExit(0)

# A gap the maintainer has already accepted, pinned to both EVRs so it cannot
# outlive itself: the moment the channel serves anything else, the recorded
# gap stops matching and this stops excusing anything. The served side is read
# under the channel's own rule, so it tightens and loosens with the expected
# side rather than drifting away from it.
#
# Only `--expect-predecessor` consults it. A gap is a statement about a release
# that already shipped without reaching COPR, and preflight is the only caller
# asking about one. `verify-copr` asks about the release it is publishing right
# now with `--expect-evr`, and a record that could answer that question would
# silence the job on the very failure it exists to make loud -- reachable,
# because a predecessor may be pinned at the workspace version.
gap = channel.get("served_evr_gap") if args.expect_predecessor else None
gap_served = gap.get("served_evr") if isinstance(gap, dict) else None
gap_matches_served = isinstance(gap_served, str) and carries(served_evr, gap_served)
if isinstance(gap, dict) and gap.get("expected_evr") == expected_evr and gap_matches_served:
    print(
        f"served release channel contract: KNOWN GAP ({args.channel} COPR "
        f"{owner}/{project} serves {package}-{served_evr}, not the expected "
        f"{expected_evr}; issue #{gap.get('issue')} owns it)"
    )
    raise SystemExit(0)

latest_evr = build_evr(latest)
latest_state = latest.get("state") if isinstance(latest, dict) else None
detail = (
    f"{args.channel} COPR {owner}/{project} does not serve "
    f"{package}-{expected_evr}{expected_for}: "
    f"latest succeeded build is {served_evr!r} on {sorted(served_chroots)}; "
    f"latest build is {latest_evr!r} state={latest_state!r}"
)
# A build of the expected EVR that is already dead is a verdict; polling it to a
# deadline would delay the same failure by an hour. Everything else is a build
# that has not happened yet, which only a deadline can turn into a failure. The
# release job polls, so it needs the two told apart -- and a caller that does
# not poll treats both as failure.
if matches(latest_evr) and latest_state in TERMINAL_STATES:
    fail(detail)
pending(detail)
