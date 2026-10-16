#!/usr/bin/env python3
"""Scope regressions for the advisory PR mutation job."""

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("drill_pr", Path(__file__).with_name("pr.py"))
drill = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drill)


class Scope(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.mutations = Path(self.temp.name)
        (self.mutations / "first.patch").write_text("+++ b/rs/first.rs\n+++ b/rs/shared.rs\n")
        (self.mutations / "second.patch").write_text("+++ b/rs/second.rs\n+++ b/rs/shared.rs\n")

    def selected(self, *changed):
        return drill.select(set(changed), self.mutations)

    def test_recipe_reports_a_result_for_an_explicit_base(self):
        result = subprocess.run(
            ["just", "test", "drill-pr", "select", "HEAD"], cwd=drill.ROOT,
            text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"^needed=(true|false)\n$")

    def test_empty_and_unrelated_changes_select_nothing(self):
        self.assertEqual(self.selected(), [])
        self.assertEqual(self.selected("README.md", ".github/workflows/check.yml"), [])

    def test_target_paths_and_patch_changes_select_only_affected_mutations(self):
        self.assertEqual(self.selected("rs/first.rs"), ["first"])
        self.assertEqual(self.selected("test/drill/mutations/second.patch"), ["second"])
        self.assertEqual(self.selected("rs/shared.rs"), ["first", "second"])
        self.assertEqual(self.selected("rs/first.rs", "rs/shared.rs"), ["first", "second"])

    def test_drill_or_harness_changes_select_every_mutation(self):
        for path in ["rs/moq-relay/tests/drills.rs", "test/drill/sensitivity.sh", "test/drill/pr.py"]:
            with self.subTest(path=path):
                self.assertEqual(self.selected(path), ["first", "second"])

    def test_malformed_mutations_fail_instead_of_silently_selecting_nothing(self):
        (self.mutations / "broken.patch").write_text("not a patch\n")
        with self.assertRaisesRegex(ValueError, "no target files"):
            self.selected("README.md")

    def test_deleted_mutations_are_not_passed_to_the_harness(self):
        (self.mutations / "second.patch").unlink()
        self.assertEqual(self.selected("test/drill/mutations/second.patch"), [])


if __name__ == "__main__":
    unittest.main()
