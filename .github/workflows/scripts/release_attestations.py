"""The builders' digest attestations, and the rules that make them evidence.

The publish job downloads every `release-digests-*` artifact into one tree and
reads provenance out of it. Two things make that tree attacker-shaped. Anything
running in a builder can call the Actions artifact API and upload an artifact
of its own. And the artifact store is shared by every job in the run and
writable with any job's runtime token: a builder that runs later can delete
and re-upload an earlier builder's payload and its attestation as a matching
pair, and every digest check still passes.

So an artifact is untrusted until it is bound to a job output. Each attesting
job records the SHA-256 of its own `digests.json` as a job output; the Actions
service stores that under the job that produced it, where no other job can
rewrite it. The publish job reads those outputs through `toJSON(needs)`, and an
attestation is used only after its bytes hash to the value its job recorded. A
missing output is a refusal, named by slot.

The attesting set is likewise pinned rather than merely deduplicated. Exactly
the expected artifacts must be present, each named for the slot it fills, each
holding one document, and each declaring the job that slot belongs to. An extra
artifact, a missing one, a renamed one, or one claiming another job's name
stops the release.

`release-assets.sh` owns the expected slot list and both of its readers load
attestations through here, so the rule has one implementation.
"""

from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path

ARTIFACT_PREFIX = "release-digests-"
DOCUMENT_NAME = "digests.json"
DIGEST = re.compile(r"[0-9a-f]{64}")
# Everything a document may carry. A field outside this set is a claim the
# release has no rule for, and is refused rather than ignored.
DOCUMENT_KEYS = {"job", "assets", "suite", "image", "components"}


class AttestationError(Exception):
    """A statement the release must not be built on."""


def expected_slots(spec: str) -> dict[str, dict]:
    """Parse the expected attesting set: one tab-separated line per slot,
    `<slot> <job> <output> <suite> <image> <components>`, `-` for none.

    An attestation is a self-report, so the suite, image and component names
    each slot may declare are pinned here and held exactly."""
    slots: dict[str, dict] = {}
    for line in spec.splitlines():
        if not line.strip():
            continue
        fields = line.split("\t")
        if len(fields) != 6 or not all(fields):
            raise AttestationError(f"malformed expected attestation: {line!r}")
        slot, job, output, suite, image, components = fields
        if slot in slots:
            raise AttestationError(f"expected attestation slot declared twice: {slot}")
        slots[slot] = {
            "job": job,
            "output": output,
            "suite": None if suite == "-" else suite,
            "image": None if image == "-" else image,
            "components": [] if components == "-" else sorted(components.split(",")),
        }
    if not slots:
        raise AttestationError("the expected attesting set is empty")
    return slots


def _no_symlinks(paths: list[Path]) -> list[Path]:
    """`paths`, or an attestation error naming the first symlink found.

    The artifact tree these are globbed from is attacker-controlled, and a
    symlink lets a builder point a trusted-looking name at bytes it does not
    own; the digest and JSON checks below never run against a followed link."""
    for path in paths:
        if path.is_symlink():
            raise AttestationError(f"{path} is a symlink; refusing to trust it as an attestation")
    return paths


def read_object(path: Path, what: str) -> dict:
    """A JSON object from `path`, or an attestation error naming `what`."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AttestationError(f"{what} is not valid JSON: {error}") from error
    if not isinstance(document, dict):
        raise AttestationError(f"{what} is not a JSON object")
    return document


def load(root: str, spec: str, job_outputs: str) -> list[dict]:
    """Every attestation under `root`, or an error naming what is wrong.

    `job_outputs` is the file the publish job wrote from `toJSON(needs)`.
    Returns the documents in slot order, each with a `slot` key added."""
    base = Path(root)
    expected = expected_slots(spec)
    needs = read_object(Path(job_outputs), "the job outputs document")

    smuggled = [
        path
        for path in _no_symlinks(sorted(base.rglob(DOCUMENT_NAME)))
        if not path.relative_to(base).parts[0].startswith(ARTIFACT_PREFIX)
    ]
    if smuggled:
        raise AttestationError(
            f"{smuggled[0]} is a digest attestation inside a payload artifact"
        )

    prefixed = _no_symlinks(
        sorted(path for path in base.iterdir() if path.name.startswith(ARTIFACT_PREFIX))
    )
    present = sorted(path.name[len(ARTIFACT_PREFIX):] for path in prefixed if path.is_dir())
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
        job, output = expected[slot]["job"], expected[slot]["output"]
        artifact = _no_symlinks([base / f"{ARTIFACT_PREFIX}{slot}"])[0]
        found = _no_symlinks(sorted(artifact.rglob(DOCUMENT_NAME)))
        if len(found) != 1:
            raise AttestationError(
                f"attestation {slot} holds {len(found)} {DOCUMENT_NAME} documents, expected one"
            )
        # The binding comes first: nothing in the document is read until its
        # bytes are the ones its job recorded.
        entry = needs.get(job)
        outputs = entry.get("outputs") if isinstance(entry, dict) else None
        recorded = outputs.get(output) if isinstance(outputs, dict) else None
        if not isinstance(recorded, str) or not DIGEST.fullmatch(recorded):
            raise AttestationError(
                f"attestation {slot} is not bound to a job output: {job} recorded no "
                f"{output} digest, so its artifact cannot be trusted"
            )
        actual = hashlib.sha256(found[0].read_bytes()).hexdigest()
        if actual != recorded:
            raise AttestationError(
                f"attestation {slot} is not the document {job} recorded as {output} "
                f"({recorded} recorded, {actual} downloaded); the artifact changed "
                "after its job finished"
            )
        document = read_object(found[0], f"attestation {slot}")
        if document.get("job") != job:
            raise AttestationError(
                f"attestation {slot} declares job {document.get('job')!r}, "
                f"but that slot belongs to {job!r}"
            )
        # The provenance a slot may declare is pinned; the document is held to
        # it exactly, so a swapped image, an invented suite or an added
        # component never reaches the manifest.
        suite, image = expected[slot]["suite"], expected[slot]["image"]
        components = expected[slot]["components"]
        extra = sorted(set(document) - DOCUMENT_KEYS)
        if extra:
            raise AttestationError(
                f"attestation {slot} declares keys the release does not record: {', '.join(extra)}"
            )
        if document.get("suite") != suite:
            raise AttestationError(
                f"attestation {slot} declares suite {document.get('suite')!r}, "
                f"but that slot attests {suite!r}"
            )
        if document.get("image") != image:
            raise AttestationError(
                f"attestation {slot} declares the build image {document.get('image')!r}, "
                f"but the release pins {image!r} for that slot"
            )
        if not isinstance(document.get("components", {}), dict):
            raise AttestationError(f"attestation {slot} components is not a JSON object")
        if sorted(document.get("components", {})) != components:
            raise AttestationError(
                f"attestation {slot} declares components {sorted(document.get('components', {}))}, "
                f"but that slot may declare exactly {components}"
            )
        assets = document.get("assets")
        if not isinstance(assets, dict) or not all(
            isinstance(name, str) and isinstance(digest, str) and DIGEST.fullmatch(digest)
            for name, digest in assets.items()
        ):
            raise AttestationError(
                f"attestation {slot} does not map asset names to SHA-256 digests"
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
