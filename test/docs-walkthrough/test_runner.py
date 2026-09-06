"""Exercise safety refusals without package installation or host changes."""
import importlib.util
import json
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
    def test_adapter_write_roots_reject_parent_and_child_binds(self):
        for target in ('/usr', '/usr/lib', '/home', '/root', '/bin', '/lib', '/sbin', '/tmp', '/opt'):
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, 'mount'):
                runner().check_mounts(f'31 20 8:1 /host{target} {target} rw - ext4 /dev/sda rw')

    def test_symlinked_pam_and_service_parents_are_not_pristine(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'etc').mkdir()
            (root / 'external').mkdir()
            (root / 'etc/pam.d').symlink_to(root / 'external', target_is_directory=True)
            self.assertFalse(runner().pristine_files(root)['pam_absent'])
            (root / 'etc/systemd').symlink_to(root / 'external', target_is_directory=True)
            self.assertFalse(runner().pristine_files(root)['service_assets_absent'])

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

    def test_route_reference_does_not_claim_literal_occurrence_coverage(self):
        row = {"source": {"path": "docs/releasing.md", "anchor": "packages", "ordinal": 1, "sha256": "a" * 64}, "raw": "just test-deb", "classification": "executable"}
        route = {"id": "deb-direct", "source_role": "route-reference", "sources": [row["source"]]}
        self.assertEqual(runner().coverage([route], [row])["unmapped"], 1)
        self.assertEqual(len(runner().manual_sections([route], [row])), 1)

    def test_guard_rejects_protected_parent_bind_mounts(self):
        for target in ("/etc", "/run", "/var", "/var/lib"):
            with self.subTest(target=target):
                line = f"35 22 8:1 /host{target} {target} rw - ext4 /dev/sda rw"
                with self.assertRaisesRegex(ValueError, "protected"):
                    runner().check_mounts(line)

    def test_pristine_files_detect_stale_pam_and_policy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "etc/pam.d").mkdir(parents=True)
            (root / "etc/pam.d/sudo").write_text("auth sufficient pam_facelock.so\n")
            policy = root / "usr/share/dbus-1/system.d/org.facelock.Daemon.conf"
            policy.parent.mkdir(parents=True)
            policy.write_text("stale")
            observations = runner().pristine_files(root)
            self.assertFalse(observations["pam_absent"])
            self.assertFalse(observations["service_assets_absent"])

    def test_scenario_checks_guest_image_and_observed_init(self):
        case = {"target": "debian-13", "image": "debian@sha256:" + "a" * 64}
        guest = {"os": "debian-13", "image": "wrong", "init": "systemd", "level": "booted-vm"}
        with self.assertRaisesRegex(ValueError, "image"):
            runner().check_guest_case(guest, case, "systemd")
        guest["image"] = case["image"]
        with self.assertRaisesRegex(ValueError, "init"):
            runner().check_guest_case(guest, case, "bash")

    def test_copied_guest_harness_cannot_claim_a_stale_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            module = runner()
            module.ROOT = Path(directory)
            module.HERE = module.ROOT / "harness"
            module.HERE.mkdir()
            (module.HERE / "run.py").write_text("changed")
            (module.ROOT / "walkthrough-provenance.json").write_text(json.dumps({"harness_sha256": "a" * 64, "harness_tree_dirty": False}))
            with self.assertRaisesRegex(ValueError, "harness"):
                module.harness_identity()


if __name__ == "__main__":
    unittest.main()
