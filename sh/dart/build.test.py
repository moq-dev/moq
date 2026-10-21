"""Dart checks build dev libraries while benchmarks pin optimized libraries."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BuildTest(unittest.TestCase):
    def test_generator_profiles(self):
        for profile, release in (("debug", False), ("release", True)):
            with self.subTest(profile=profile), tempfile.TemporaryDirectory() as directory:
                scratch = Path(directory)
                calls = scratch / "calls"
                for name, source in {
                    "cargo": f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{calls}"\n',
                    "rustc": '#!/bin/sh\necho "host: x86_64-unknown-linux-gnu"\n',
                    "dart": '#!/bin/sh\nexit 0\n',
                    "uniffi_bindgen_dart": '#!/bin/sh\nwhile [ "$1" != --out-dir ]; do shift; done\n'
                        'mkdir -p "$2"\nfor name in moq.dart uniffi_runtime.dart; do '
                        'echo "// ignore_for_file: unused_import" > "$2/$name"; done\n',
                }.items():
                    tool = scratch / name
                    tool.write_text(source)
                    tool.chmod(0o755)
                env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}",
                           MOQ_FFI_PROFILE=profile, BASH_ENV=os.devnull)
                result = subprocess.run(["bash", "sh/dart/generate.sh", str(scratch / "generated")],
                                        cwd=ROOT, env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                args = calls.read_text().split()
                self.assertEqual("--release" in args, release)
                self.assertIn("--features", args)
                self.assertIn("openh264", args)

    def test_failed_benchmark_restores_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "sh/dart").mkdir(parents=True)
            (root / "dart/moq").mkdir(parents=True)
            shutil.copy(ROOT / "sh/dart/bench.sh", root / "sh/dart/bench.sh")
            generate = root / "sh/dart/generate.sh"
            generate.write_text('#!/bin/sh\ntest "$MOQ_FFI_PROFILE" = release\n')
            generate.chmod(0o755)
            manifest = root / "dart/moq/pubspec.yaml"
            manifest.write_text("name: fixture\n")
            tools = root / "tools"
            tools.mkdir()
            for name, source in {
                "git": f'#!/bin/sh\necho "{root}"\n',
                "rustc": '#!/bin/sh\necho "host: x86_64-unknown-linux-gnu"\n',
                "dart": '#!/bin/sh\nif [ "$1" = run ]; then '
                    'grep -q "release/libmoq_ffi.so" pubspec.yaml || exit 2; exit 7; fi\n',
            }.items():
                tool = tools / name
                tool.write_text(source)
                tool.chmod(0o755)
            result = subprocess.run(["bash", "sh/dart/bench.sh"], cwd=root,
                                    env=dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}",
                                             BASH_ENV=os.devnull), capture_output=True, text=True)
            self.assertEqual(result.returncode, 7, result.stderr)
            self.assertEqual(manifest.read_text(), "name: fixture\n")
            self.assertEqual(list((root / ".scratch").iterdir()), [])


if __name__ == "__main__":
    unittest.main()
