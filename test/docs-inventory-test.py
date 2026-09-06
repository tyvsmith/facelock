#!/usr/bin/env python3
"""Inventory checks must detect newly undocumented public surfaces."""
import importlib.util
import re
from pathlib import Path
import unittest
import tempfile

SPEC = importlib.util.spec_from_file_location("docs_inventory", Path(__file__).with_name("docs-inventory.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class InventoryTest(unittest.TestCase):
    def test_container_workspace_tests_trust_only_the_checkout(self):
        workflow = (MODULE.ROOT / '.github/workflows/ci.yml').read_text()
        trust = '\n        run: git config --global --add safe.directory "$GITHUB_WORKSPACE"'
        for job in ('build-and-test', 'tpm-tests'):
            with self.subTest(job=job):
                body = re.search(r'(?ms)^  ' + job + r':\n(.*?)(?=^  \S|\Z)', workflow)[1]
                self.assertIn(trust, body)
                self.assertLess(body.index('uses: actions/checkout@'), body.index(trust))
                self.assertLess(body.index(trust), body.index('run: cargo test --workspace'))

    def test_source_archive_discovery_finds_new_docs_and_excludes_build_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'docs').mkdir()
            (root / 'docs/new.md').write_text('# New documentation')
            (root / 'target').mkdir()
            (root / 'target/generated.html').write_text('build output')
            self.assertEqual(MODULE.instructional_files(root), ['docs/new.md'])

    def test_new_instruction_requires_classification(self):
        errors = MODULE.check_corpus(["docs/new.md"], {"files": {}})
        self.assertIn("docs/new.md", " ".join(errors))

    def test_historical_requires_reason(self):
        errors = MODULE.check_corpus(["CHANGELOG.md"], {"files": {"CHANGELOG.md": {"role": "historical"}}})
        self.assertTrue(errors)

    def test_recipe_required_and_optional_arguments(self):
        recipes = {"test-rpm": {"parameters": [{"name": "release", "default": "44", "kind": "singular"}]}, "release": {"parameters": [{"name": "version", "default": None, "kind": "singular"}]}}
        self.assertIsNone(MODULE.check_just_argv(["just", "test-rpm"], recipes))
        self.assertIsNone(MODULE.check_just_argv(["just", "test-rpm", "44"], recipes))
        self.assertIsNotNone(MODULE.check_just_argv(["just", "test-rpm", "44", "extra"], recipes))
        self.assertIsNotNone(MODULE.check_just_argv(["just", "release"], recipes))
        self.assertIsNotNone(MODULE.check_just_argv(["just", "invented"], recipes))

    def test_generated_index_detects_new_recipe(self):
        first = MODULE.render_recipes({"test": {"name": "test", "parameters": [], "doc": "Run tests", "private": False}}, [])
        second = MODULE.render_recipes({"test": {"name": "test", "parameters": [], "doc": "Run tests", "private": False}, "new": {"name": "new", "parameters": [], "doc": "New", "private": False}}, [])
        self.assertNotEqual(first, second)
        self.assertIn("just new", second)

    def test_all_cargo_target_spellings_are_validated(self):
        for argv in (["cargo", "build", "--bin", "invented"], ["cargo", "run", "--bin=invented"]):
            self.assertIsNotNone(MODULE.check_entrypoint(argv, {"facelock"}, {}, schematic=False))
        self.assertIsNone(MODULE.check_entrypoint(["cargo", "build", "--bin", "facelock"], {"facelock"}, {}))

    def test_schematic_recipe_requires_real_name_but_may_omit_arguments(self):
        recipes = {"release": {"parameters": [{"name": "version", "default": None}]}}
        self.assertIsNotNone(MODULE.check_entrypoint(["just", "invented"], set(), recipes, schematic=True))
        self.assertIsNone(MODULE.check_entrypoint(["just", "release"], set(), recipes, schematic=True))

    def test_just_aliases_retain_target_parameters_and_dependencies(self):
        target = {"parameters": [{"name": "version", "default": None}], "dependencies": [{"recipe": "build", "arguments": []}], "private": False}
        recipes = MODULE.recipe_metadata({"recipes": {"release": target}, "aliases": {"r": {"target": "release", "attributes": []}}})
        self.assertEqual(recipes["r"]["alias_for"], "release")
        self.assertEqual(recipes["r"]["dependencies"], target["dependencies"])
        self.assertIsNotNone(MODULE.check_just_argv(["just", "r"], recipes))
        self.assertIn("Alias for", MODULE.render_recipes(recipes, []))


if __name__ == "__main__":
    unittest.main()
