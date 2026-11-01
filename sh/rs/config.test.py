"""Exercise Cargo's config inheritance without building the workspace."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ConfigTest(unittest.TestCase):
    def test_nested_checkout_aliases(self):
        scratch = ROOT / ".tmp"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            outer = Path(directory)
            inner = outer / ".claude/worktrees/nested checkout"
            for checkout in (outer, inner):
                (checkout / ".cargo").mkdir(parents=True)
                shutil.copyfile(ROOT / ".cargo/config.toml", checkout / ".cargo/config.toml")
            (inner / "src").mkdir()
            (inner / "Cargo.toml").write_text(
                '[package]\nname = "moq-ffi"\nversion = "0.0.0"\nedition = "2024"\n'
                '[workspace]\n[[bin]]\nname = "moq"\npath = "src/main.rs"\n'
                '[features]\nopenh264 = []\n'
            )
            (inner / "src/main.rs").write_text(
                'fn main() { for arg in std::env::args().skip(1) { println!("{arg}"); } }\n'
            )
            env = dict(os.environ, CARGO_NET_OFFLINE="true", CARGO_TERM_COLOR="never",
                       CARGO_TARGET_DIR=str(outer / "target"))
            # --help parses the expanded build command without compiling moq-ffi.
            result = subprocess.run(["cargo", "build-ffi", "--help"], cwd=inner, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("--package", result.stdout)
            # moq's separator hides duplicated alias tokens from Cargo's parser.
            for args in (["--help"], ["publish", "path with spaces", "--", "--help"]):
                with self.subTest(args=args):
                    result = subprocess.run(["cargo", "moq", *args], cwd=inner, env=env,
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertEqual(result.stdout.splitlines(), args, result.stderr)


if __name__ == "__main__":
    unittest.main()
