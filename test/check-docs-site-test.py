#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("site_check", Path(__file__).with_name("check-docs-site.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class SiteTest(unittest.TestCase):
    def test_missing_file_fragment_and_asset(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "index.html").write_text('<a href="chapter.html#missing">x</a><img src="missing.svg"><a href="absent.html">bad</a>')
            (root / "chapter.html").write_text('<h1 id="present">Chapter</h1>')
            errors = MODULE.check_site(root)
            self.assertEqual(len(errors), 3)
            self.assertIn("missing", " ".join(errors))

    def test_valid_relative_and_encoded_fragment(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "docs").mkdir()
            (root / "index.html").write_text('<a href="docs/page.html#hello%20world">x</a><a href="https://example.org/">external</a>')
            (root / "docs/page.html").write_text('<h1 id="hello world">x</h1><a href="../index.html">home</a>')
            self.assertEqual(MODULE.check_site(root), [])

    def test_link_cannot_escape_site(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "index.html").write_text('<a href="../../outside">outside</a>')
            self.assertIn("escape", " ".join(MODULE.check_site(root)))

    def test_empty_site_is_not_success(self):
        with tempfile.TemporaryDirectory() as folder:
            self.assertTrue(MODULE.check_site(Path(folder)))


if __name__ == "__main__":
    unittest.main()
