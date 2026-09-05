#!/usr/bin/env python3
"""Check package-name extraction and Debian lookup offline; no network requests."""
import importlib.util
import contextlib
import http.client
import io
import lzma
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

SPEC = importlib.util.spec_from_file_location(
    "live_names", Path(__file__).with_name("check-package-names-live.py")
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class SourcePrerequisitesTest(unittest.TestCase):
    def names(self, text):
        previous = MODULE.ROOT
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in (
                "README.md", "AGENTS.md", "config/facelock.toml",
                "website/index.html", "book/src/gpu.md", "docs/quickstart.md",
            ):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text if name == "docs/quickstart.md" else "")
            try:
                MODULE.ROOT = root
                return MODULE.documented_names()
            finally:
                MODULE.ROOT = previous

    def test_arch_upgrade_install_includes_packages_not_searches(self):
        names = self.names(
            "sudo pacman -Syu --needed base-devel onnxruntime-cpu\n"
            "pacman -Ss not-a-package\n"
        )
        self.assertEqual(names["Arch"], {"base-devel", "onnxruntime-cpu"})

    def test_apt_target_release_is_not_a_package(self):
        names = self.names("sudo apt install -t trixie-backports rustc cargo\n")
        self.assertEqual(names["Debian"], {"rustc", "cargo"})


class DebianBinaryIndexTest(unittest.TestCase):
    INDEX_URL = "https://deb.debian.org/debian/dists/trixie/main/binary-amd64/Packages.xz"
    INDEX = (
        b"Package: libpam0g-dev\nSource: pam\nVersion: 1.7.0-5\n"
        b"Architecture: amd64\nDescription: PAM development files\n"
        b" A continuation line.\n\n"
        b"Package: example-data\nVersion: 1.0-1\nArchitecture: all\n\n"
    )

    def setUp(self):
        # Successful indexes are cached across names, never across fixtures.
        if hasattr(MODULE, "debian_binary_names"):
            MODULE.debian_binary_names.cache_clear()

    def response(self, payload):
        def open_url(request, **kwargs):
            # Reproduce the old resolver's source/binary mismatch on RED.
            if request.full_url.startswith("https://sources.debian.org/api/src/"):
                return io.BytesIO(b'{"error": 404}')
            self.assertEqual(request.full_url, self.INDEX_URL)
            return io.BytesIO(payload)
        return patch.object(MODULE.urllib.request, "urlopen", side_effect=open_url)

    def test_binary_name_resolves_without_source_name_alias(self):
        with self.response(lzma.compress(self.INDEX)) as requests:
            self.assertTrue(MODULE.in_debian("libpam0g-dev"))
            self.assertFalse(MODULE.in_debian("pam"))
            self.assertTrue(MODULE.in_debian("example-data"))
            self.assertEqual(requests.call_count, 1)

    def test_absent_binary_is_missing_after_valid_index(self):
        with self.response(lzma.compress(self.INDEX)):
            self.assertFalse(MODULE.in_debian("invented-package"))

    def test_malformed_or_empty_index_is_unavailable_not_missing(self):
        for data in (
            b"", b"<html>upstream error</html>", b"Package: libpam0g-dev\n",
            b"Package: invalid name\nVersion: 1\nArchitecture: amd64\n",
            self.INDEX + b"broken record\n",
            self.INDEX + b"Package: x\nPackage: y\nVersion: 1\nArchitecture: amd64\n",
        ):
            with self.subTest(data=data), self.response(lzma.compress(data)):
                with self.assertRaises(MODULE.RepositoryUnavailable):
                    MODULE.in_debian("libpam0g-dev")

    def test_corrupt_compression_and_invalid_utf8_are_unavailable(self):
        for data in (b"not xz", lzma.compress(self.INDEX)[:-8], lzma.compress(b"\xff")):
            with self.subTest(data=data), self.response(data):
                with self.assertRaises(MODULE.RepositoryUnavailable):
                    MODULE.in_debian("libpam0g-dev")

    def test_index_http_404_and_network_failure_are_unavailable(self):
        for error in (
            urllib.error.HTTPError(self.INDEX_URL, 404, "not found", {}, None),
            urllib.error.URLError("offline"), TimeoutError("timed out"),
            http.client.IncompleteRead(b"partial index"),
        ):
            with self.subTest(error=error), patch.object(
                MODULE.urllib.request, "urlopen", side_effect=error
            ):
                with self.assertRaises(MODULE.RepositoryUnavailable):
                    MODULE.in_debian("libpam0g-dev")

    def test_exit_status_distinguishes_missing_and_unavailable(self):
        for payload, expected in ((lzma.compress(self.INDEX), 1), (b"broken index", 2)):
            self.setUp()
            with self.subTest(expected=expected), self.response(payload), patch.object(
                MODULE, "documented_names", return_value={"Debian": {"invented-package"}}
            ), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(MODULE.main(), expected)


if __name__ == "__main__":
    unittest.main()
