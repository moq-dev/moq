#!/usr/bin/env python3
"""Exercise coupled updates without downloading nixpkgs or a browser."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class UpdateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        shutil.copy(Path(__file__).with_name("update.py"), self.root)
        (self.root / "clients/js").mkdir(parents=True)
        self.manifest = self.root / "clients/js/package.json"
        self.package = {"dependencies": {"@moq/watch": "latest"}, "devDependencies": {"playwright": "1.0.0"}}
        self.manifest.write_text(json.dumps(self.package))
        (self.root / "flake.lock").write_text("old lock")
        fake = self.root / "nix"
        fake.write_text("""#!/usr/bin/env python3
import os
from pathlib import Path
import sys
if sys.argv[1:] == ["flake", "update"]:
    if os.environ.get("FAIL_UPDATE"):
        sys.exit(1)
    Path("flake.lock").write_text("new lock")
elif sys.argv[1:] == ["eval", "--impure", "--raw", "--expr", "builtins.currentSystem"]:
    print("aarch64-linux")
elif sys.argv[1:] == ["eval", "--raw", ".#devShells.aarch64-linux.default.shellHook"]:
    print(os.environ["HOOK"])
else:
    sys.exit(2)
""")
        fake.chmod(0o755)
        self.env = {**os.environ, "PATH": f"{self.root}:{os.environ['PATH']}"}

    def run_update(self, hook, **env):
        return subprocess.run(
            ["python3", str(self.root / "update.py")],
            env={**self.env, "HOOK": hook, **env},
            text=True,
            capture_output=True,
        )

    def test_updates_both_files_and_preserves_latest_moq(self):
        result = self.run_update('export PLAYWRIGHT_VERSION="1.65.2"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "flake.lock").read_text(), "new lock")
        self.package["devDependencies"]["playwright"] = "1.65.2"
        self.assertEqual(json.loads(self.manifest.read_text()), self.package)

    def test_refuses_missing_ambiguous_or_unstable_version(self):
        for hook in [
            "",
            'export PLAYWRIGHT_VERSION="1.65.2-beta"',
            'export PLAYWRIGHT_VERSION="1.65.2"\nexport PLAYWRIGHT_VERSION="1.65.3"',
        ]:
            with self.subTest(hook=hook):
                result = self.run_update(hook)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(json.loads(self.manifest.read_text()), self.package)

    def test_failed_lock_update_does_not_move_pin(self):
        result = self.run_update('export PLAYWRIGHT_VERSION="1.65.2"', FAIL_UPDATE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.root / "flake.lock").read_text(), "old lock")
        self.assertEqual(json.loads(self.manifest.read_text()), self.package)


if __name__ == "__main__":
    unittest.main()
