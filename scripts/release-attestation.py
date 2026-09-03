#!/usr/bin/env python3
"""Render and validate the pre-tag release attestation (#236).

A green build job proves a package was built. It does not prove that a channel
serves it, that the bytes it serves are the ones that were validated, or that
the metadata a client would fetch is current. The attestation is the document
that binds those together for one candidate commit:

    candidate commit -> per-channel served EVRs
                     -> artifact and repository digests
                     -> signing key fingerprints
                     -> when each channel's metadata was last refreshed

`render` turns the facts a release run gathered into that document, in a
canonical form so two runs over the same facts produce identical bytes.
`validate` compares a document with the expectations recorded for the release
and with the checked-in channel authority in dist/release-matrix.json, and
fails closed on any disagreement.

Provisioning the staging channels is issue #236's infrastructure phase; this
script contacts nothing and only reads files it is given. Stdlib only: it runs
in the same bare containers as the rest of the release gates.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "dist" / "release-matrix.json"
SCHEMA = "facelock-release-attestation/1"

COMMIT = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
FINGERPRINT = re.compile(r"[0-9A-F]{40}")
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-(alpha|beta|rc)\.(0|[1-9][0-9]*))?")

CHANNEL_FIELDS = ("identity", "served_evrs", "repository_digest", "metadata_refreshed_at", "signing_key_fingerprint")


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def require(condition: object, message: str) -> None:
    if not condition:
        fail(message)


def load_json(path: Path, description: str) -> dict:
    try:
        content = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {description}: {error}")
    require(isinstance(content, dict), f"{description} is not a JSON object")
    return content


def timestamp(value: object, description: str) -> datetime:
    require(isinstance(value, str) and value.endswith("Z"), f"{description} must be a UTC RFC 3339 timestamp: {value!r}")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        fail(f"{description} is not a parseable timestamp: {error}")
    return parsed.astimezone(timezone.utc)


def string_map(value: object, description: str) -> dict[str, str]:
    require(
        isinstance(value, dict)
        and value
        and all(isinstance(key, str) and isinstance(item, str) and key and item for key, item in value.items()),
        f"{description} must be a non-empty map of strings to strings",
    )
    return dict(value)


def chroot_authority(value: object, channel: str) -> set[str]:
    """A channel's required_supported_chroots, validated before it becomes a
    set: a non-empty list of non-empty strings with no duplicates. Used for
    both COPR channels the release matrix declares."""
    require(
        isinstance(value, list) and value and all(isinstance(item, str) and item for item in value),
        f"release matrix {channel} COPR required_supported_chroots must be a non-empty list of non-empty strings: {value!r}",
    )
    require(
        len(value) == len(set(value)),
        f"release matrix {channel} COPR required_supported_chroots must not contain duplicate chroots: {value!r}",
    )
    return set(value)


def channel_authority() -> tuple[str, str, set[str]]:
    """The production identity no attestation may claim, plus the staging one
    and the chroots it is allowed to serve."""
    matrix = load_json(MATRIX_PATH, "release matrix")
    try:
        channels = matrix["copr_channels"]
        production = f"{channels['production']['owner']}/{channels['production']['project']}"
        staging = channels["staging"]
        staging_identity = f"{staging['owner']}/{staging['project']}"
        staging_required_chroots = staging["required_supported_chroots"]
    except (KeyError, TypeError) as error:
        fail(f"release matrix has no complete COPR channel authority: {error}")
    staging_chroots = chroot_authority(staging_required_chroots, "staging")
    return production, staging_identity, staging_chroots


def normalized_channel(name: str, channel: object) -> dict:
    where = f"channel {name}"
    require(isinstance(channel, dict), f"{where} is not an object")
    for field in CHANNEL_FIELDS:
        require(field in channel, f"{where} omits {field}")
    identity = channel["identity"]
    require(isinstance(identity, str) and identity, f"{where} identity must be a non-empty string")
    digest = channel["repository_digest"]
    require(isinstance(digest, str) and DIGEST.fullmatch(digest), f"{where} repository_digest is not a sha256 digest: {digest!r}")
    fingerprint = channel["signing_key_fingerprint"]
    require(
        isinstance(fingerprint, str) and FINGERPRINT.fullmatch(fingerprint),
        f"{where} signing_key_fingerprint is not a 40-character uppercase hex fingerprint: {fingerprint!r}",
    )
    timestamp(channel["metadata_refreshed_at"], f"{where} metadata_refreshed_at")
    unknown = sorted(set(channel) - set(CHANNEL_FIELDS))
    require(not unknown, f"{where} carries unknown fields: {unknown}")
    return {
        "identity": identity,
        "metadata_refreshed_at": channel["metadata_refreshed_at"],
        "repository_digest": digest,
        "served_evrs": dict(sorted(string_map(channel["served_evrs"], f"{where} served_evrs").items())),
        "signing_key_fingerprint": fingerprint,
    }


def normalized_artifact(name: str, artifact: object, channels: set[str]) -> dict:
    where = f"artifact {name}"
    require(isinstance(artifact, dict), f"{where} is not an object")
    require(set(artifact) == {"channel", "digest"}, f"{where} must carry exactly a channel and a digest")
    channel = artifact["channel"]
    require(channel in channels, f"{where} names an undeclared channel: {channel!r}")
    digest = artifact["digest"]
    require(isinstance(digest, str) and DIGEST.fullmatch(digest), f"{where} digest is not a sha256 digest: {digest!r}")
    return {"channel": channel, "digest": digest}


def render(inputs: dict) -> dict:
    commit = inputs.get("candidate_commit")
    require(
        isinstance(commit, str) and COMMIT.fullmatch(commit),
        f"candidate_commit must be a full lowercase git commit id: {commit!r}",
    )
    version = inputs.get("release_version")
    require(
        isinstance(version, str) and VERSION.fullmatch(version),
        f"release_version must be a release identity: {version!r}",
    )
    raw_channels = inputs.get("channels")
    require(isinstance(raw_channels, dict) and raw_channels, "channels must be a non-empty object")
    channels = {name: normalized_channel(name, channel) for name, channel in sorted(raw_channels.items())}
    raw_artifacts = inputs.get("artifacts")
    require(isinstance(raw_artifacts, dict) and raw_artifacts, "artifacts must be a non-empty object")
    artifacts = {
        name: normalized_artifact(name, artifact, set(channels))
        for name, artifact in sorted(raw_artifacts.items())
    }
    unknown = sorted(set(inputs) - {"candidate_commit", "release_version", "channels", "artifacts", "schema"})
    require(not unknown, f"attestation input carries unknown fields: {unknown}")
    return {
        "schema": SCHEMA,
        "candidate_commit": commit,
        "release_version": version,
        "channels": channels,
        "artifacts": artifacts,
    }


def validate(attestation: dict, expect: dict, now: datetime) -> None:
    require(attestation.get("schema") == SCHEMA, f"attestation schema drifted: {attestation.get('schema')!r}")
    # Re-render rather than trust the document's shape: a hand-edited
    # attestation is exactly what this gate is looking for.
    document = render({key: value for key, value in attestation.items() if key != "schema"})

    require(
        document["candidate_commit"] == expect.get("candidate_commit"),
        f"candidate commit drifted: attested {document['candidate_commit']}, expected {expect.get('candidate_commit')!r}",
    )
    require(
        document["release_version"] == expect.get("release_version"),
        f"release version drifted: attested {document['release_version']}, expected {expect.get('release_version')!r}",
    )
    max_age = expect.get("metadata_max_age_seconds")
    require(
        isinstance(max_age, int) and not isinstance(max_age, bool) and max_age > 0,
        f"expectation metadata_max_age_seconds must be a positive integer: {max_age!r}",
    )

    expected_channels = expect.get("channels")
    require(isinstance(expected_channels, dict) and expected_channels, "expectation channels must be a non-empty object")
    require(
        set(document["channels"]) == set(expected_channels),
        f"attested channels {sorted(document['channels'])} != expected {sorted(expected_channels)}",
    )

    production_identity, staging_identity, staging_chroots = channel_authority()
    for name, channel in document["channels"].items():
        expected = expected_channels[name]
        require(isinstance(expected, dict), f"expectation for channel {name} is not an object")
        require(
            channel["identity"] != production_identity,
            f"channel {name} must never attest the production COPR project {production_identity}",
        )
        for field in ("identity", "repository_digest", "signing_key_fingerprint"):
            label = {"identity": "identity", "repository_digest": "repository digest", "signing_key_fingerprint": "signing key fingerprint"}[field]
            require(
                channel[field] == expected.get(field),
                f"channel {name} {label} drifted: attested {channel[field]!r}, expected {expected.get(field)!r}",
            )
        expected_evrs = string_map(expected.get("served_evrs"), f"expectation for channel {name} served_evrs")
        require(
            set(channel["served_evrs"]) == set(expected_evrs),
            f"channel {name} served targets {sorted(channel['served_evrs'])} != expected {sorted(expected_evrs)}",
        )
        for target, evr in sorted(channel["served_evrs"].items()):
            require(
                evr == expected_evrs[target],
                f"channel {name} served EVR drifted for {target}: attested {evr!r}, expected {expected_evrs[target]!r}",
            )
        if channel["identity"] == staging_identity:
            require(
                set(channel["served_evrs"]) == staging_chroots,
                f"channel {name} disagrees with the staging COPR chroot authority: "
                f"attested {sorted(channel['served_evrs'])}, declared {sorted(staging_chroots)}",
            )
        refreshed = timestamp(channel["metadata_refreshed_at"], f"channel {name} metadata_refreshed_at")
        age = (now - refreshed).total_seconds()
        require(
            age >= 0,
            f"channel {name} metadata timestamp is in the future: refreshed {channel['metadata_refreshed_at']}, checked at {now.isoformat()}",
        )
        require(
            age <= max_age,
            f"channel {name} metadata is stale: refreshed {channel['metadata_refreshed_at']}, "
            f"{int(age)}s before the check, limit {max_age}s",
        )

    expected_artifacts = expect.get("artifacts")
    require(
        isinstance(expected_artifacts, dict) and expected_artifacts,
        "expectation artifacts must be a non-empty object",
    )
    require(
        set(document["artifacts"]) == set(expected_artifacts),
        f"attested artifacts {sorted(document['artifacts'])} != expected {sorted(expected_artifacts)}",
    )
    for name, artifact in sorted(document["artifacts"].items()):
        expected = expected_artifacts[name]
        require(isinstance(expected, dict), f"expectation for artifact {name} is not an object")
        require(
            artifact["channel"] == expected.get("channel"),
            f"artifact {name} channel drifted: attested {artifact['channel']!r}, expected {expected.get('channel')!r}",
        )
        require(
            artifact["digest"] == expected.get("digest"),
            f"artifact {name} digest drifted: attested {artifact['digest']}, expected {expected.get('digest')!r}",
        )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Render and validate the pre-tag release attestation.")
    commands = parser.add_subparsers(dest="command", required=True)

    renderer = commands.add_parser("render", help="render an attestation document from gathered release facts")
    renderer.add_argument("--input", type=Path, required=True, help="JSON document of gathered facts")
    renderer.add_argument("--output", type=Path, help="write the document here instead of stdout")

    validator = commands.add_parser("validate", help="compare an attestation with the release expectations")
    validator.add_argument("--attestation", type=Path, required=True, help="rendered attestation document")
    validator.add_argument("--expect", type=Path, required=True, help="expectations recorded for this release")
    validator.add_argument("--now", help="UTC RFC 3339 instant the check runs at (default: now)")

    args = parser.parse_args(argv)

    if args.command == "render":
        document = render(load_json(args.input, "attestation input"))
        rendered = json.dumps(document, indent=2, sort_keys=False) + "\n"
        if args.output:
            try:
                args.output.write_text(rendered)
            except OSError as error:
                fail(f"cannot write the attestation document: {error}")
            print(f"release attestation: rendered {args.output}")
        else:
            sys.stdout.write(rendered)
        return 0

    now = timestamp(args.now, "--now") if args.now else datetime.now(timezone.utc)
    validate(
        load_json(args.attestation, "attestation document"),
        load_json(args.expect, "attestation expectations"),
        now,
    )
    print("release attestation: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
