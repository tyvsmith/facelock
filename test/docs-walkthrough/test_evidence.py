"""Evidence must preserve the difference between a check and a walkthrough."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


HERE = Path(__file__).resolve().parent


def module(name):
    spec = importlib.util.spec_from_file_location(name, HERE / f"{name}.py")
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.evidence = module("evidence")
        self.case = {
            "id": "deb-trixie-direct", "target": "debian-13", "channel": "github-alpha",
            "minimum_level": "booted-vm", "requirements": [],
            "sources": [{"path": "README.md", "anchor": "install", "ordinal": 1, "sha256": "a" * 64}],
            "steps": [{"id": "install", "expect_exit": 0, "expect_output": "", "expect_state": ["installed"], "expected_invocations": [{"program": "apt", "arguments": ["install", "./facelock.deb"]}]}],
        }
        self.record = {
            "schema_version": 1, "scenario": self.case["id"], "target": self.case["target"],
            "status": "pass", "level": "booted-vm", "reason": "",
            "docs_commit": "a" * 40, "harness_commit": "b" * 40,
            "harness_sha256": "1" * 64, "harness_tree_dirty": False,
            "sources": copy.deepcopy(self.case["sources"]),
            "started_at": "2026-09-05T00:00:00+00:00", "finished_at": "2026-09-05T00:01:00+00:00",
            "identity": {
                "release": "v0.2.0-alpha.1", "version": "0.2.0-alpha.1", "native_version": "0.2.0~alpha.1-1",
                "artifact_commit": "c" * 40, "channel": "github-alpha", "runtime_policy": "bundled-ort",
                "artifact": {"asset_id": 123, "name": "facelock.deb", "url": "https://github.com/tyvsmith/facelock/releases/download/v0.2.0-alpha.1/facelock.deb", "sha256": "d" * 64, "size": 1234},
                "repository": {},
            },
            "environment": {
                "guest_id": "test-guest", "os": "debian-13", "image": "debian@sha256:" + "e" * 64,
                "init": "systemd", "snapshot": "clean-001", "pristine": True,
                "pristine_observations": {"binary_absent": True, "config_absent": True, "state_absent": True, "package_absent": True},
                "isolation_verified": True, "hardware": [],
            },
            "steps": [{"id": "install", "status": "pass", "executed": True, "command": ["apt", "install", "./facelock.deb"], "exit_code": 0, "output": "", "states": {"installed": True}, "log": {"path": "install.log", "sha256": "f" * 64, "sanitized": True}}],
            "installed": {"version": "0.2.0-alpha.1", "native_version": "0.2.0~alpha.1-1", "artifact_sha256": "d" * 64},
        }

    def check(self, **kwargs):
        return self.evidence.validate(self.record, self.case, **kwargs)

    def test_accepts_complete_published_vm_record(self):
        self.check(require_pass=True)

    def test_rejects_stale_source(self):
        self.record["sources"][0]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "source"):
            self.check()

    def test_rejects_omitted_step(self):
        self.record["steps"] = []
        with self.assertRaisesRegex(ValueError, "steps"):
            self.check()

    def test_rejects_skip_claimed_as_pass(self):
        self.record["steps"][0]["executed"] = False
        with self.assertRaisesRegex(ValueError, "executed"):
            self.check()

    def test_rejects_wrong_artifact_and_version(self):
        for field in ("version", "native_version", "artifact_sha256"):
            with self.subTest(field=field):
                old = self.record["installed"][field]
                self.record["installed"][field] = "wrong"
                with self.assertRaisesRegex(ValueError, "installed"):
                    self.check()
                self.record["installed"][field] = old

    def test_rejects_non_pristine_guest(self):
        self.record["environment"]["pristine_observations"]["state_absent"] = False
        with self.assertRaisesRegex(ValueError, "pristine"):
            self.check()

    def test_container_pass_is_not_vm_completion(self):
        self.record["level"] = "container"
        self.check()
        with self.assertRaisesRegex(ValueError, "level"):
            self.check(require_pass=True)

    def test_blocked_record_is_valid_but_not_complete(self):
        self.record.update(status="blocked", level="syntax-only", reason="alpha release is not published", steps=[])
        self.record["identity"]["artifact"] = {}
        self.check()
        with self.assertRaisesRegex(ValueError, "status"):
            self.check(require_pass=True)

    def test_blocked_record_requires_reason(self):
        self.record.update(status="blocked", steps=[])
        with self.assertRaisesRegex(ValueError, "reason"):
            self.check()

    def test_rejects_alpha_claim_on_stable_channel(self):
        self.record["identity"]["channel"] = self.case["channel"] = "apt"
        with self.assertRaisesRegex(ValueError, "prerelease"):
            self.check()

    def test_rejects_unobserved_postcondition(self):
        self.record["steps"][0]["states"] = {}
        with self.assertRaisesRegex(ValueError, "state"):
            self.check()

    def test_rejects_wrong_release_url(self):
        self.record["identity"]["artifact"]["url"] = "https://example.invalid/facelock.deb"
        with self.assertRaisesRegex(ValueError, "artifact URL"):
            self.check()

    def test_requires_physical_hardware_record_for_hardware_requirement(self):
        self.case["requirements"] = ["ir-camera"]
        with self.assertRaisesRegex(ValueError, "hardware"):
            self.check(require_pass=True)

    def test_rejects_unrelated_command_claiming_install(self):
        self.record["steps"][0]["command"] = ["true"]
        with self.assertRaisesRegex(ValueError, "invocation"):
            self.check()

    def test_rejects_unidentified_dirty_harness(self):
        del self.record["harness_sha256"]
        with self.assertRaisesRegex(ValueError, "harness"):
            self.check()

    def test_manual_pass_needs_operator_and_reviewed_expectations(self):
        self.case["adapter"] = "manual"
        with self.assertRaisesRegex(ValueError, "manual"):
            self.check(require_pass=True)

    def test_candidate_manual_cannot_be_completed_by_attestation(self):
        self.case.update(adapter="manual", review_status="candidate", generated_section=True)
        self.record["manual_review"] = {"operator": "tester", "notes": "observed", "expectations_reviewed": True, "fixture_bindings": {}}
        with self.assertRaisesRegex(ValueError, "candidate"):
            self.check(require_pass=True)

    def test_repository_cache_hash_does_not_complete_a_walkthrough(self):
        self.case.update(channel="copr-production", chroot="fedora-44-x86_64")
        identity = self.record["identity"]
        identity.update(channel="copr-production", release="v0.1.4", version="0.1.4", package_sha256="d" * 64, repository={"url": "https://copr.fedorainfracloud.org/coprs/tyvsmith/facelock/", "chroot": "fedora-44-x86_64"})
        self.record["installed"]["version"] = "0.1.4"
        self.record["installed"]["payload_binding"] = "matching retained cache only"
        self.check()
        with self.assertRaisesRegex(ValueError, "transaction"):
            self.check(require_pass=True)

    def test_file_validation_rejects_tampered_missing_and_escaping_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "evidence.json"
            for name in ("install.log", "missing.log", "../outside.log"):
                with self.subTest(name=name):
                    self.record["steps"][0]["log"]["path"] = name
                    path.write_text(json.dumps(self.record))
                    (root / "install.log").write_text("tampered")
                    with self.assertRaises((ValueError, OSError)):
                        self.evidence.validate_record_file(path, self.case)


if __name__ == "__main__":
    unittest.main()
