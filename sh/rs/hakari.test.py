"""Keep UI test projects and non-media test builds free of native media dependencies."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class EdgesTest(unittest.TestCase):
    def test_trybuild_cannot_inherit_the_hack(self):
        scratch = ROOT / ".tmp"
        scratch.mkdir(exist_ok=True)
        for excluded in (False, True):
            for target in (False, True):
                with (
                    self.subTest(excluded=excluded, target=target),
                    tempfile.TemporaryDirectory(dir=scratch) as directory,
                ):
                    root = Path(directory)
                    (root / "sh/rs").mkdir(parents=True)
                    shutil.copyfile(ROOT / "sh/rs/hakari.py", root / "sh/rs/hakari.py")
                    (root / ".config").mkdir()
                    (root / ".config/hakari.toml").write_text(
                        'hakari-package = "workspace-hack"\n[final-excludes]\nworkspace-members = '
                        + ('["ui", "trybuild"]\n' if excluded else '["trybuild"]\n')
                    )
                    (root / "Cargo.toml").write_text(
                        '[workspace]\nmembers = ["ui", "trybuild", "workspace-hack"]\nresolver = "2"\n'
                    )
                    for name in ("ui", "trybuild", "workspace-hack"):
                        crate = root / name
                        (crate / "src").mkdir(parents=True)
                        (crate / "src/lib.rs").touch()
                        (crate / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.0.0"\n')
                    manifest = root / "ui/Cargo.toml"
                    with manifest.open("a") as output:
                        output.write(
                            ("[target.'cfg(unix)'.dev-dependencies]\n" if target else "[dev-dependencies]\n")
                            + 'ui-runner = { package = "trybuild", path = "../trybuild" }\n'
                        )
                    safe = manifest.read_text()
                    with manifest.open("a") as output:
                        output.write('workspace-hack = { path = "../workspace-hack" }\n')
                    result = subprocess.run(["python3", "sh/rs/hakari.py"], cwd=root, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("trybuild", result.stderr)
                    self.assertIn("workspace-hack", result.stderr)
                    # A new UI crate is safe without having to copy kio-macros' exclusion.
                    manifest.write_text(safe)
                    result = subprocess.run(["python3", "sh/rs/hakari.py"], cwd=root, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class NativeDependenciesTest(unittest.TestCase):
    def test_non_media_linux_test_graph(self):
        for target in ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"):
            with self.subTest(target=target):
                result = subprocess.run(
                    [
                        "cargo",
                        "tree",
                        "--locked",
                        "-p",
                        "moq-json",
                        "--target",
                        target,
                        "--edges",
                        "normal,build,dev",
                        "--prefix",
                        "none",
                        "--format",
                        "{p}",
                    ],
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                packages = {line.split()[0] for line in result.stdout.splitlines()}
                self.assertIn("workspace-hack", packages)
                self.assertTrue(packages.isdisjoint({"pipewire-sys", "libspa-sys", "clang-sys"}), packages)


if __name__ == "__main__":
    unittest.main()
