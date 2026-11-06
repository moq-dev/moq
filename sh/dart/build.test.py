"""Dart checks build dev test libraries while benchmarks pin optimized libraries."""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class BuildTest(unittest.TestCase):
    def test_generator_profiles(self):
        # Only the dev build carries the test hooks, which would skew a benchmark.
        for profile, release, configured in (
            ("debug", False, False),
            ("release", True, False),
            ("debug", False, True),
            ("release", True, True),
        ):
            with self.subTest(profile=profile), tempfile.TemporaryDirectory() as directory:
                scratch = Path(directory)
                calls = scratch / "calls"
                compiler_env = scratch / "compiler-env"
                target = scratch / "external output"
                target = target if release else ROOT / "target/dart"
                metadata = json.dumps({"target_directory": str(target)})
                library = target / ("x86_64-unknown-linux-gnu" if configured else "") / profile / "libmoq_ffi.so"
                artifact = json.dumps(
                    {"reason": "compiler-artifact", "target": {"name": "moq_ffi"}, "filenames": [str(library)]}
                )
                for name, source in {
                    "cargo": f"#!/bin/sh\nif [ \"$1\" = metadata ]; then echo '{metadata}'; exit; fi\n"
                    f'printf "%s\\n" "$*" >> "{calls}"\n'
                    f'printf "%s" "${{CC:-unset}}" > "{compiler_env}"\n'
                    f"echo '{artifact}'\n",
                    "rustc": '#!/bin/sh\necho "host: x86_64-unknown-linux-gnu"\n',
                    "dart": "#!/bin/sh\nexit 0\n",
                    "uniffi_bindgen_dart": f'#!/bin/sh\ntest "$2" = "{library}" || exit 2\n'
                    'while [ "$1" != --out-dir ]; do shift; done\n'
                    'mkdir -p "$2"\nfor name in moq.dart uniffi_runtime.dart; do '
                    'echo "// ignore_for_file: unused_import" > "$2/$name"; done\n',
                }.items():
                    tool = scratch / name
                    tool.write_text(source)
                    tool.chmod(0o755)
                env = dict(
                    os.environ,
                    PATH=f"{scratch}:{os.environ['PATH']}",
                    MOQ_FFI_PROFILE=profile,
                    BASH_ENV=os.devnull,
                    CC="fixture-compiler",
                )
                result = subprocess.run(
                    ["bash", "sh/dart/generate.sh", str(scratch / "generated")],
                    cwd=ROOT,
                    env=env,
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(compiler_env.read_text(), "fixture-compiler" if release else "unset")
                args = calls.read_text().split()
                self.assertEqual(args[0], "build-ffi")
                self.assertEqual("--release" in args, release)
                self.assertNotIn("--target", args)
                self.assertEqual(
                    args[1:],
                    (["--release"] if release else ["--features", "testing", "--target-dir", str(ROOT / "target/dart")])
                    + ["--message-format=json-render-diagnostics"],
                )

    def test_failed_benchmark_restores_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "sh/dart").mkdir(parents=True)
            (root / "sh/rs").mkdir()
            shutil.copy(ROOT / "sh/rs/ffi-artifact.jq", root / "sh/rs/ffi-artifact.jq")
            (root / "dart/moq").mkdir(parents=True)
            shutil.copy(ROOT / "sh/dart/bench.sh", root / "sh/dart/bench.sh")
            generate = root / "sh/dart/generate.sh"
            generate.write_text('#!/bin/sh\ntest "$MOQ_FFI_PROFILE" = release\n')
            generate.chmod(0o755)
            manifest = root / "dart/moq/pubspec.yaml"
            manifest.write_text("name: fixture\n")
            tools = root / "tools"
            tools.mkdir()
            target = root / "external output"
            metadata = json.dumps({"target_directory": str(target)})
            artifact = json.dumps(
                {
                    "reason": "compiler-artifact",
                    "target": {"name": "moq_ffi"},
                    "filenames": [str(target / "release/libmoq_ffi.so")],
                }
            )
            for name, source in {
                "cargo": f"#!/bin/sh\nif [ \"$1\" = metadata ]; then echo '{metadata}'; else echo '{artifact}'; fi\n",
                "git": f'#!/bin/sh\necho "{root}"\n',
                "rustc": '#!/bin/sh\necho "host: x86_64-unknown-linux-gnu"\n',
                "dart": '#!/bin/sh\nif [ "$1" = run ]; then '
                f"grep -Fq '{target}/release/libmoq_ffi.so' pubspec.yaml || exit 2; exit 7; fi\n",
            }.items():
                tool = tools / name
                tool.write_text(source)
                tool.chmod(0o755)
            result = subprocess.run(
                ["bash", "sh/dart/bench.sh"],
                cwd=root,
                env=dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}", BASH_ENV=os.devnull),
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 7, result.stderr)
            self.assertEqual(manifest.read_text(), "name: fixture\n")
            self.assertEqual(list((root / ".tmp").iterdir()), [])


if __name__ == "__main__":
    unittest.main()
