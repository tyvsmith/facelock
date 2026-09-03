"""The builders' digest attestations, and the rules that make them evidence.

The publish job downloads every `release-digests-*` artifact into one tree and
reads provenance out of it. Anything running in a builder can call the Actions
artifact API and upload an artifact of its own, so the tree is attacker-shaped:
an extra artifact whose `digests.json` claims another job's identity would put
forged image and component digests into `MANIFEST.json` while every asset digest
still checked out.

So the attesting set is pinned rather than merely deduplicated. Exactly the
expected artifacts must be present, each named for the slot it fills, each
holding one document, and each declaring the job that slot belongs to. An extra
artifact, a missing one, a renamed one, or one claiming another job's name stops
the release.

`release-assets.sh` owns the expected slot list and both of its readers load
attestations through here, so the rule has one implementation.
"""

from __future__ import annotations

import json
from pathlib import Path

ARTIFACT_PREFIX = "release-digests-"
DOCUMENT_NAME = "digests.json"


class AttestationError(Exception):
    """A statement the release must not be built on."""


def expected_slots(spec: str) -> dict[str, str]:
    """Parse `<slot><TAB><job>` lines into the expected attesting set."""
    slots: dict[str, str] = {}
    for line in spec.splitlines():
        if not line.strip():
            continue
        slot, _, job = line.partition("\t")
        if not slot or not job:
            raise AttestationError(f"malformed expected attestation: {line!r}")
        if slot in slots:
            raise AttestationError(f"expected attestation slot declared twice: {slot}")
        slots[slot] = job
    if not slots:
        raise AttestationError("the expected attesting set is empty")
    return slots


def load(root: str, spec: str) -> list[dict]:
    """Every attestation under `root`, or an error naming what is wrong.

    Returns the documents in slot order, each with a `slot` key added."""
    base = Path(root)
    expected = expected_slots(spec)

    smuggled = [
        path
        for path in sorted(base.rglob(DOCUMENT_NAME))
        if not path.relative_to(base).parts[0].startswith(ARTIFACT_PREFIX)
    ]
    if smuggled:
        raise AttestationError(
            f"{smuggled[0]} is a digest attestation inside a payload artifact"
        )

    present = sorted(
        path.name[len(ARTIFACT_PREFIX):]
        for path in base.iterdir()
        if path.is_dir() and path.name.startswith(ARTIFACT_PREFIX)
    )
    unexpected = sorted(set(present) - set(expected))
    if unexpected:
        raise AttestationError(
            "no builder attests as "
            + ", ".join(unexpected)
            + f"; the release is attested by exactly {', '.join(sorted(expected))}"
        )
    missing = sorted(set(expected) - set(present))
    if missing:
        raise AttestationError(
            "no attestation from " + ", ".join(missing) + "; every builder must attest"
        )

    documents: list[dict] = []
    for slot in sorted(expected):
        artifact = base / f"{ARTIFACT_PREFIX}{slot}"
        found = sorted(artifact.rglob(DOCUMENT_NAME))
        if len(found) != 1:
            raise AttestationError(
                f"attestation {slot} holds {len(found)} {DOCUMENT_NAME} documents, expected one"
            )
        document = json.loads(found[0].read_text(encoding="utf-8"))
        if document.get("job") != expected[slot]:
            raise AttestationError(
                f"attestation {slot} declares job {document.get('job')!r}, "
                f"but that slot belongs to {expected[slot]!r}"
            )
        document["slot"] = slot
        documents.append(document)
    return documents


def provenance(documents: list[dict]) -> tuple[dict[str, str], dict[str, object]]:
    """The build images and components the attestations agree on.

    Two attestations claiming one key is a contradiction, not a merge."""
    build_images: dict[str, str] = {}
    components: dict[str, object] = {}
    for document in documents:
        job = document["job"]
        key = f"{job}:{document['suite']}" if document.get("suite") else job
        if document.get("image"):
            if key in build_images:
                raise AttestationError(f"two attestations claim the build image {key}")
            build_images[key] = document["image"]
        for name, value in document.get("components", {}).items():
            if name in components:
                raise AttestationError(f"two attestations claim the component {name}")
            components[name] = value
    return dict(sorted(build_images.items())), dict(sorted(components.items()))
