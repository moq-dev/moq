#!/usr/bin/env python3
"""Check all manifests without inspecting installed or smoke-owned dependencies."""

import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from playwright import check


class PlaywrightTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        subprocess.run(["git", "init", "--quiet", str(self.root)], check=True)
        (self.root / ".gitignore").write_text("node_modules/\n")

    def package(self, name, **fields):
        path = self.root / name / "package.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(fields))

    def test_exact_pin_matches(self):
        self.package("test/video", devDependencies={"playwright": "1.63.0"})
        self.assertEqual(check(self.root, "1.63.0"), [])

    def test_all_dependency_fields_and_new_packages_are_checked(self):
        for field in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]:
            with self.subTest(field=field):
                self.package("new/package", **{field: {"playwright": "^1.63.0"}})
                problems = check(self.root, "1.63.0")
                self.assertEqual(len(problems), 1)
                self.assertIn(f"{field}.playwright", problems[0])

    def test_nix_bump_refuses_an_old_exact_pin(self):
        self.package("test/video", devDependencies={"playwright": "1.63.0"})
        self.assertEqual(len(check(self.root, "1.64.0")), 1)

    def test_deleted_manifest_does_not_leave_a_pin(self):
        self.package("test/removed", devDependencies={"playwright": "1.1.0"})
        subprocess.run(["git", "add", "test/removed/package.json"], cwd=self.root, check=True)
        (self.root / "test/removed/package.json").unlink()
        self.assertEqual(check(self.root, "1.63.0"), [])

    def test_smoke_and_installed_dependencies_are_independent(self):
        self.package("test/smoke/clients/js", devDependencies={"playwright": "1.1.0"})
        self.package("node_modules/fixture", dependencies={"playwright": "1.1.0"})
        self.package("test/video", devDependencies={"playwright": "1.63.0"})
        self.assertEqual(check(self.root, "1.63.0"), [])


if __name__ == "__main__":
    unittest.main()
