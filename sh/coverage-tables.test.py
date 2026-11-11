#!/usr/bin/env python3
"""Check sh/coverage-tables.py against a scratch tree."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "coverage-tables.py"
SOURCE = """\
pub enum Restart {
    Redial,
    Wait,
}
pub struct Reader {
    pub(crate) bound: u64,
}
impl Reader {
    pub fn sync(&mut self) {}
}
"""
SKILL = """\
# Verify

## Coverage

| Code | Model |
| --- | --- |
| `src/lib.rs` `Reader::sync` (`Unmodelled::thing`), `bound` | `Sync` |
| `src/lib.rs` `Restart::Wait`; `src/other.rs` `run` | `Wait` |

## Left out
"""


class CoverageTablesTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.write("src/lib.rs", SOURCE)
        self.write("src/other.rs", "async fn run() {}\n")
        self.write(".claude/skills/verify-x/SKILL.md", SKILL)

    def write(self, path, text):
        (self.root / path).parent.mkdir(parents=True, exist_ok=True)
        (self.root / path).write_text(text)

    def check(self):
        return subprocess.run([sys.executable, SCRIPT, "--root", self.root], capture_output=True, text=True)

    def test_items_fields_and_variants_pass(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_renamed_function_fails(self):
        self.write("src/lib.rs", SOURCE.replace("fn sync", "fn resync"))
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("SKILL.md:7: `Reader::sync`: no `sync` defined", result.stderr)

    def test_a_renamed_type_fails(self):
        self.write("src/lib.rs", SOURCE.replace("enum Restart", "enum Policy"))
        self.assertIn("no `Restart` defined", self.check().stderr)

    def test_a_moved_file_fails(self):
        (self.root / "src/other.rs").unlink()
        self.assertIn("`src/other.rs` is not a Rust file", self.check().stderr)

    def test_a_cell_without_a_path_fails(self):
        self.write(".claude/skills/verify-x/SKILL.md", SKILL.replace("`src/lib.rs` `Reader", "`Reader"))
        self.assertIn("must start with a repo-relative path", self.check().stderr)


if __name__ == "__main__":
    unittest.main()
