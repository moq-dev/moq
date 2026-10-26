"""A test build must reject Clippy findings without a separate lint compile."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class TestLints(unittest.TestCase):
    def test_clippy_warning_fails_test_build(self):
        temporary = ROOT / ".tmp"
        temporary.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=temporary) as directory:
            root = Path(directory)
            (root / "rs").mkdir()
            (root / "sh" / "rs").mkdir(parents=True)
            (root / "src").mkdir()
            shutil.copy(ROOT / "sh" / "local.sh", root / "sh" / "local.sh")
            (root / "justfile").write_text("mod rs\n")
            shutil.copy(ROOT / "rs" / "justfile", root / "rs" / "justfile")
            shutil.copy(ROOT / "sh" / "rs" / "nextest.sh", root / "sh" / "rs" / "nextest.sh")
            (root / "Cargo.toml").write_text(
                '[package]\nname = "test-lint-gate"\nversion = "0.0.0"\nedition = "2024"\n'
                '\n[workspace]\n'
            )
            (root / "src" / "lib.rs").write_text(
                'pub fn empty(values: &[u8]) -> bool { values.len() == 0 }\n'
                '#[test]\nfn runs() { assert!(empty(&[])); }\n'
            )
            toolchain = Path(shutil.which("clippy-driver")).parent
            env = dict(os.environ, LOCAL_SUPERVISED="1", CARGO_TARGET_DIR=str(root / "target"),
                       PATH=f"{toolchain}:{os.environ['PATH']}",
                       NEXTEST_PROFILE="default")
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root,
                           env=env, check=True, capture_output=True)
            result = subprocess.run(["just", "rs", "test"], cwd=root, env=env,
                                    text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("clippy::len_zero", result.stderr)
            source = root / "src" / "lib.rs"
            source.write_text(source.read_text().replace("values.len() == 0", "values.is_empty()"))
            result = subprocess.run(["just", "rs", "test"], cwd=root, env=env,
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
