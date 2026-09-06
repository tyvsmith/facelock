#!/usr/bin/env python3
"""Regression tests for collecting instructions without running them."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("docs_examples", Path(__file__).with_name("docs-examples.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ExamplesTest(unittest.TestCase):
    def extract(self, text, path="docs/example.md"):
        return MODULE.extract_text(path, text)

    def test_quotes_comments_and_continuation(self):
        text = '\n'.join(['## Enroll', '```sh', 'sudo facelock enroll \\', '  --label "office # 1" # keep quote', '```'])
        rows = self.extract(text)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["segments"][0]["argv"], ["facelock", "enroll", "--label", "office # 1"])
        self.assertEqual(rows[0]["source"]["line"], 3)

    def test_pipeline_and_commands_after_operator(self):
        rows = self.extract('```bash\nRUST_LOG=debug sudo -u alice facelock devices --json | jq . && facelock capabilities\n```')
        self.assertEqual([s["argv"][0] for s in rows[0]["segments"]], ["facelock", "jq", "facelock"])
        self.assertEqual(rows[0]["segments"][0]["wrapper"], ["RUST_LOG=debug", "sudo", "-u", "alice"])

    def test_quoted_operator_is_an_argument(self):
        row = self.extract('```sh\nfacelock enroll --label "|"\n```')[0]
        self.assertEqual(row["segments"][0]["argv"], ["facelock", "enroll", "--label", "|"])

    def test_redirect_descriptor_requires_adjacency(self):
        self.assertEqual(MODULE.segments('facelock remove 1 > /tmp/out')[0][0]["argv"], ["facelock", "remove", "1"])
        self.assertEqual(MODULE.segments('facelock remove 1 2>/tmp/err')[0][0]["argv"], ["facelock", "remove", "1"])

    def test_hash_inside_word_is_not_a_comment(self):
        self.assertEqual(MODULE.segments('facelock enroll --label face#1 # comment')[0][0]["argv"], ["facelock", "enroll", "--label", "face#1"])

    def test_absolute_binary_inline_reference_is_collected(self):
        self.assertEqual(self.extract('Run `/usr/bin/facelock status`.')[0]["segments"][0]["argv"], ["facelock", "status"])

    def test_html_void_elements_preserve_ordered_commands(self):
        rows = self.extract('<pre>facelock status<br>facelock devices</pre>', 'website/index.html')
        self.assertEqual([r["segments"][0]["argv"] for r in rows], [["facelock", "status"], ["facelock", "devices"]])

    def test_sudo_stdin_flag_does_not_consume_command(self):
        row = self.extract('```sh\nsudo -S facelock devices\n```')[0]
        self.assertEqual(row["segments"][0]["argv"], ["facelock", "devices"])

    def test_html_entities_and_continuation(self):
        text = '<section id="apt"><span class="command">curl https://example.test/key \\</span>\n<span class="command">| sudo tee /tmp/key &gt;/dev/null &amp;&amp; facelock capabilities</span></section>'
        row = self.extract(text, "website/index.html")[0]
        self.assertEqual(row["source"]["anchor"], "apt")
        self.assertEqual([s["argv"] for s in row["segments"]], [["curl", "https://example.test/key"], ["tee", "/tmp/key"], ["facelock", "capabilities"]])

    def test_unsupported_substitution_is_visible(self):
        row = self.extract('```bash\nfacelock remove $(id -u)\n```')[0]
        self.assertEqual(row["classification"], "manual")
        self.assertIn("substitution", row["reason"])

    def test_inline_synopsis_is_not_an_executable_example(self):
        rows = self.extract('Use `facelock auth --user <name>` and `just test`.')
        self.assertEqual(len(rows), 2)
        self.assertTrue(all(row["classification"] == "schematic" for row in rows))

    def test_roff_examples(self):
        rows = self.extract('.SH EXAMPLES\n.nf\nsudo facelock devices \\-\\-json\n.fi\n', "man/facelock.1")
        self.assertEqual(rows[0]["segments"][0]["argv"], ["facelock", "devices", "--json"])

    def test_cargo_run_target(self):
        row = self.extract('```bash\ncargo run --bin facelock -- devices --json\n```')[0]
        self.assertEqual(row["segments"][0]["argv"], ["facelock", "devices", "--json"])

    def test_prefixed_cargo_run_keeps_target_validation_input(self):
        for prefix in ('env FOO=bar ', 'sudo -u alice ', 'FOO=bar '):
            segment = MODULE.segments(prefix + 'cargo run --bin nonexistent -- --help')[0][0]
            self.assertIn(['cargo', 'run', '--bin', 'nonexistent', '--'], MODULE.entrypoint_argv(segment))

    def test_content_hash_changes_not_with_line_number(self):
        a = self.extract('## Test\n```bash\nfacelock devices\n```')[0]
        b = self.extract('\n\n## Test\n```bash\nfacelock devices\n```')[0]
        c = self.extract('## Test\n```bash\nfacelock devices --json\n```')[0]
        self.assertEqual(a["source"]["sha256"], b["source"]["sha256"])
        self.assertNotEqual(a["source"]["sha256"], c["source"]["sha256"])

    def test_include_cycle_and_escape_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "a.md").write_text('{{#include b.md}}\n')
            (root / "b.md").write_text('{{#include a.md}}\n')
            with self.assertRaisesRegex(ValueError, "cycle"):
                MODULE.resolve_includes(root, "a.md")
            (root / "b.md").write_text('{{#include ../outside.md}}\n')
            with self.assertRaisesRegex(ValueError, "escape"):
                MODULE.resolve_includes(root, "a.md")

    def test_non_shell_blocks_not_interpreted(self):
        self.assertEqual(self.extract('```toml\ncommand = "facelock fake"\n```'), [])

    def test_shell_instructions_in_config_comments_are_collected(self):
        rows = self.extract('# Configuration\n# env FACELOCK_CONFIG=/tmp/test.toml facelock config show\n# max_height = 480', 'config/facelock.toml')
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["segments"][0]["argv"], ['facelock', 'config', 'show'])

    def test_syntax_check_reports_incomplete_shell_block(self):
        errors = MODULE.shell_syntax_errors("example.md", '```bash\nif true; then\necho hi\n```')
        self.assertTrue(errors)
        self.assertEqual(MODULE.shell_syntax_errors("example.md", '```bash\nif true; then\necho hi\nfi\n```'), [])

    def test_metavariable_does_not_disappear_as_redirect(self):
        row = self.extract('```bash\njust release <X.Y.Z>\n```')[0]
        self.assertEqual(row["classification"], "schematic")
        self.assertIn("metavariable", row["reason"])

    def test_negative_annotation_binds_to_block(self):
        text = '<!-- docs-example: negative MissingRequiredArgument -->\n```bash\nfacelock auth\n```'
        row = self.extract(text)[0]
        self.assertEqual(row["classification"], "negative")
        self.assertEqual(row["expected_error"], "MissingRequiredArgument")

    def test_inline_annotation_is_scoped_to_next_content_line(self):
        rows = self.extract('<!-- docs-example: negative InvalidSubcommand -->\nThe rejected name was `facelock purge`.\nUse `facelock devices` instead.')
        self.assertEqual(rows[0]["classification"], "negative")
        self.assertEqual(rows[0]["expected_error"], "InvalidSubcommand")
        self.assertEqual(rows[1]["classification"], "schematic")


if __name__ == "__main__":
    unittest.main()
