"""Verify renamed package artifacts and apt transition dependencies."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class PackageRenameTest(unittest.TestCase):
    def test_packages(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            binary = scratch / "binary"
            binary.write_text("#!/bin/sh\necho packaged\n")
            binary.chmod(0o755)
            for name in ("moq", "moq-token"):
                with self.subTest(name=name):
                    old = f"{name}-cli"
                    env = dict(os.environ, VERSION="99.0.0", ARCH="amd64", BINARY_PATH=str(binary))
                    outputs = {}
                    for config, packager in (("nfpm", "deb"), ("transition", "deb"), ("nfpm", "rpm")):
                        output = scratch / f"{name}-{config}.{packager}"
                        subprocess.run([
                            "bash", "rs/scripts/package-nfpm.sh",
                            f"packaging/{old}/{config}.yaml", packager, str(output),
                        ], env=env, check=True)
                        outputs[config, packager] = output

                    def field(config, key):
                        return subprocess.check_output([
                            "dpkg-deb", "--field", str(outputs[config, "deb"]), key,
                        ], text=True).strip()

                    self.assertEqual(field("nfpm", "Package"), name)
                    self.assertEqual(field("nfpm", "Breaks"), f"{old} (<< 99.0.0)")
                    self.assertEqual(field("nfpm", "Replaces"), f"{old} (<< 99.0.0)")
                    self.assertEqual(field("transition", "Package"), old)
                    self.assertEqual(field("transition", "Depends"), f"{name} (>= 99.0.0)")
                    for config in ("nfpm", "transition"):
                        destination = scratch / f"{name}-{config}"
                        subprocess.run([
                            "dpkg-deb", "--extract", str(outputs[config, "deb"]), str(destination),
                        ], check=True)
                        files = [p for p in destination.rglob("*") if p.is_file()]
                        if config == "transition":
                            self.assertEqual(files, [])
                        else:
                            self.assertEqual(files, [destination / "usr/bin" / name])
                            self.assertEqual(files[0].read_bytes(), binary.read_bytes())


if __name__ == "__main__":
    unittest.main()
