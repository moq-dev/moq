"""Concurrent root recipes and media entry points share one installation."""

import os
import shutil
import signal
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class Install(unittest.TestCase):
    def test_overlap(self):
        (ROOT / ".tmp").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=ROOT / ".tmp") as directory:
            root = Path(directory)
            (root / "sh/js").mkdir(parents=True)
            shutil.copy(ROOT / "sh/js/install.py", root / "sh/js/install.py")
            (root / "test").mkdir()
            # Extract the setup commands from actual root and test recipes,
            # leaving lint and playback to their own suites.
            recipe_setup = (ROOT / "justfile").read_text().split("biome-fix:\n")[1].splitlines()[0]
            test_setup = (ROOT / "test/justfile").read_text().splitlines()
            test_setup = next(line for line in test_setup if "install.py" in line)
            (root / "justfile").write_text("biome:\n" + recipe_setup + "\n")
            (root / "test/justfile").write_text("media:\n" + test_setup + "\n")
            (root / "package.json").write_text('{"workspaces": []}')
            (root / "bun.lock").write_text("before")
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(["git", "-C", str(root), "add", "."], check=True)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            bun = bin_dir / "bun"
            bun.write_text("""#!/usr/bin/env python3
from pathlib import Path
import sys
r = Path.cwd()
if sys.argv[1] == '--version':
    print('fixture')
else:
    with (r / 'calls').open('a') as f:
        f.write('install\\n')
    # Signal the parent while holding the install lock, then block on a pipe.
    print('ready', flush=True)
    with open(r / 'continue') as f:
        f.read()
    (r / 'node_modules').mkdir(exist_ok=True)
    (r / 'js/libav/dist').mkdir(parents=True, exist_ok=True)
    (r / 'js/libav/dist/libav-opus-aac-af.mjs').touch()
    (r / 'bun.lock').write_text('after')
""")
            bun.chmod(0o755)
            os.mkfifo(root / "continue")
            env = {**os.environ, "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"]}
            recipe = subprocess.Popen(
                ["just", "biome"], cwd=root, env=env, stdout=subprocess.PIPE, text=True, start_new_session=True
            )
            self.addCleanup(recipe.stdout.close)
            self.addCleanup(lambda: os.killpg(recipe.pid, signal.SIGKILL) if recipe.poll() is None else None)
            self.assertEqual(recipe.stdout.readline(), "ready\n")
            test = subprocess.Popen(
                ["just", "--justfile", "test/justfile", "media"],
                cwd=root,
                env=env,
                stderr=subprocess.STDOUT,
                stdout=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            self.addCleanup(test.stdout.close)
            self.addCleanup(lambda: os.killpg(test.pid, signal.SIGKILL) if test.poll() is None else None)
            self.assertIn("install.py frozen", test.stdout.readline())
            self.assertEqual(test.stdout.readline(), "waiting for another workspace dependency install\n")
            with (root / "continue").open("w"):
                pass
            self.assertEqual(recipe.wait(), 0)
            self.assertEqual(test.wait(), 0)
            self.assertEqual((root / "calls").read_text(), "install\n")
            # Frozen readers reuse the post-update fingerprint as well.
            subprocess.run(["just", "--justfile", "test/justfile", "media"], cwd=root, env=env, check=True)
            self.assertEqual((root / "calls").read_text(), "install\n")


if __name__ == "__main__":
    unittest.main()
