"""Tests for stage-docs.py. Run: python3 -I -m unittest scripts/test_stage_docs.py"""

import importlib.util
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
spec = importlib.util.spec_from_file_location("stage_docs", HERE / "stage-docs.py")
stage_docs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stage_docs)

BLOB = "https://github.com/ocpu/impromptu-wifi/blob/main/"
TREE = "https://github.com/ocpu/impromptu-wifi/tree/main/"


class StageDocsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.out = pathlib.Path(cls.tmp.name)
        stage_docs.stage(REPO, cls.out)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def read(self, rel):
        return (self.out / rel).read_text()

    def test_docs_are_staged_at_the_root(self):
        self.assertTrue((self.out / "README.md").is_file())
        self.assertTrue((self.out / "reference" / "cli.md").is_file())
        self.assertTrue((self.out / "examples" / "esp32s3" / "README.md").is_file())

    def test_non_doc_files_link_to_github(self):
        text = self.read("reference/support-matrix.md")
        self.assertIn(f"]({BLOB}examples/raspberry-pi/setup.sh)", text)

    def test_staged_pages_stay_relative(self):
        text = self.read("reference/support-matrix.md")
        self.assertIn("](../examples/esp32s3/README.md#limitations)", text)

    def test_directory_with_readme_links_to_it(self):
        text = self.read("reference/support-matrix.md")
        self.assertIn("](../examples/raspberry-pi/README.md)", text)

    def test_unstaged_markdown_links_to_github(self):
        self.assertIn(f"]({BLOB}README.md)", self.read("README.md"))

    def test_directory_without_readme_links_to_github_tree(self):
        self.assertEqual(
            stage_docs.rewrite_link("decisions/", REPO / "docs" / "README.md", REPO),
            "decisions/README.md" if (REPO / "docs/decisions/README.md").exists()
            else f"{TREE}docs/decisions",
        )

    def test_anchors_and_external_links_are_kept(self):
        page = REPO / "docs" / "README.md"
        self.assertEqual(stage_docs.rewrite_link("#x", page, REPO), "#x")
        self.assertEqual(
            stage_docs.rewrite_link("https://example.org/a.md", page, REPO),
            "https://example.org/a.md",
        )

    def test_code_blocks_are_not_rewritten(self):
        md = "```\n[a](../x.sh)\n```\n[b](../x.sh)\n"
        out = stage_docs.rewrite_markdown(md, REPO / "docs" / "README.md", REPO)
        self.assertIn("```\n[a](../x.sh)\n```", out)
        self.assertIn(f"[b]({BLOB}x.sh)", out)

    def test_summary_lists_every_page(self):
        summary = self.read("SUMMARY.md")
        self.assertIn("(decisions/0007-wpa-supplicant-control-socket-backend.md)", summary)
        self.assertIn("(examples/raspberry-pi/README.md)", summary)
        self.assertIn("(reference/support-matrix.md)", summary)
        self.assertTrue(summary.splitlines()[2].startswith("[Introduction](README.md)"))

    def test_decisions_are_titled(self):
        self.assertIn(
            "- [0001: Talk to BlueZ over D-Bus with zbus instead of libdbus-based crates]"
            "(decisions/0001-use-zbus-for-bluez.md)",
            self.read("SUMMARY.md"),
        )

    def test_check_passes_on_the_repository(self):
        result = subprocess.run(
            [sys.executable, "-I", str(HERE / "stage-docs.py"), "--check", str(self.out)],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
