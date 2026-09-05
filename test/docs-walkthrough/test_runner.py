"""Exercise safety refusals without package installation or host changes."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


def runner():
    spec = importlib.util.spec_from_file_location("walkthrough_run", HERE / "run.py")
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


class RunnerTests(unittest.TestCase):
    def test_derives_suite_and_fedora_cases_from_matrix(self):
        cases = runner().load_cases()
        names = {case["id"] for case in cases}
        self.assertTrue({"apt-trixie", "apt-resolute", "deb-trixie-direct", "copr-production-43", "copr-staging-45", "aur-facelock", "source", "nixos", "openrc", "runit", "s6"} <= names)

    def test_refuses_host_without_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "marker"):
                runner().verify_guest(Path(directory) / "absent")

    def test_missing_explicit_run_arguments_never_executes(self):
        result = subprocess.run(["python3", str(HERE / "run.py"), "run"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--scenario", result.stderr)

    def test_rootless_launch_rejects_unpinned_or_wrong_image(self):
        value = runner()
        case = next(c for c in value.load_cases() if c["id"] == "deb-trixie-direct")
        for image in ("debian:latest", "fedora@sha256:" + "a" * 64):
            with self.assertRaisesRegex(ValueError, "matrix image"):
                value.container_image(case, image)

    def test_adapter_selection_is_closed(self):
        with self.assertRaisesRegex(ValueError, "adapter"):
            runner().adapter_command({"adapter": "../../etc/passwd"}, {})

    def test_nix_version_follows_workspace(self):
        source = (HERE.parents[1] / "dist/nix/default.nix").read_text()
        self.assertIn("(builtins.fromTOML (builtins.readFile ../../Cargo.toml)).workspace.package.version", source)
        self.assertNotIn('version = "0.1.0"', source)

    def test_coverage_keeps_unmapped_commands_and_manual_obligations(self):
        occurrence = {"source": {"path": "docs/new.md", "anchor": "test", "ordinal": 1, "sha256": "a" * 64}, "raw": "facelock test", "classification": "manual"}
        report = runner().coverage([], [occurrence])
        self.assertEqual(report["unmapped"], 1)
        self.assertEqual(report["obligations"][0]["status"], "pending")

    def test_coverage_does_not_deduplicate_same_command_in_different_guides(self):
        rows = [{"source": {"path": path, "anchor": "test", "ordinal": 1, "sha256": "a" * 64}, "raw": "facelock test", "classification": "executable"} for path in ("README.md", "docs/new.md")]
        self.assertEqual(len(runner().coverage([], rows)["obligations"]), 2)

    def test_timeout_terminates_the_command_group(self):
        code, output = runner().execute(["sh", "-c", "sleep 30 & wait"], {}, 0.05)
        self.assertEqual(code, 124)
        self.assertIn("timed out", output)

    def test_manual_sections_keep_order_and_camera_gate(self):
        rows = [{"source": {"path": "docs/cli.md", "anchor": "enroll", "ordinal": number, "sha256": str(number) * 64}, "raw": raw, "classification": "executable"} for number, raw in ((1, "facelock enroll"), (2, "facelock test"))]
        case = runner().manual_sections([], rows)[0]
        self.assertEqual([step["raw"] for step in case["steps"]], [row["raw"] for row in rows])
        self.assertIn("ir-camera", case["requirements"])
        self.assertEqual(case["minimum_level"], "physical-hardware")

    def test_release_commands_require_maintainer_authority(self):
        row = {"source": {"path": "docs/releasing.md", "anchor": "publish", "ordinal": 1, "sha256": "a" * 64}, "raw": "gh release create v0.2.0", "classification": "manual"}
        self.assertIn("explicit-publication-authority", runner().manual_sections([], [row])[0]["requirements"])


if __name__ == "__main__":
    unittest.main()
