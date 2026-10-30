#!/usr/bin/env python3
"""Exercise pinned native inputs without network access or Rust compilation."""

import hashlib
import importlib.util
import io
import json
import os
import pathlib
import platform
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

REPO = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ffi", pathlib.Path(__file__).with_name("ffi.py"))
ffi = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ffi)


class Inputs(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.patch = patch.object(ffi, "ROOT", self.root)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        (self.root / "cpp/moq").mkdir(parents=True)
        (self.root / "cpp/moq/FFI_VERSION").write_text("1.2.3\n")
        (self.root / "cpp/moq/FFI_SHA256.json").write_text("{}")

    def pin(self, path):
        (self.root / "cpp/moq/FFI_SHA256.json").write_text(
            json.dumps(
                {
                    ffi.name("1.2.3", target) + ".tar.gz": hashlib.sha256(path.read_bytes()).hexdigest()
                    for target in ffi.TARGETS
                }
            )
        )

    def test_fetch_requires_a_real_pin_before_network(self):
        with patch.object(ffi.urllib.request, "urlretrieve") as download:
            with self.assertRaisesRegex(ValueError, "SHA-256 pin"):
                ffi.fetch("x86_64-unknown-linux-gnu", self.root / "out")
            download.assert_not_called()

    def test_fetch_rejects_absent_or_malformed_selection_before_network(self):
        for pin in ("", "# unset\n", "1.2.3\n4.5.6\n", "latest\n"):
            (self.root / "cpp/moq/FFI_VERSION").write_text(pin)
            with patch.object(ffi.urllib.request, "urlretrieve") as download:
                with self.assertRaises(ValueError):
                    ffi.fetch("x86_64-unknown-linux-gnu", self.root / "out")
                download.assert_not_called()

    def test_checksum_mismatch_never_extracts(self):
        path = self.root / "bad"
        path.write_bytes(b"changed")
        self.pin(path)
        path.write_bytes(b"replacement")

        def download(url, dest):
            self.assertIn("moq-ffi-v1.2.3/moq-ffi-default-1.2.3-", url)
            shutil.copyfile(path, dest)

        with patch.object(ffi.urllib.request, "urlretrieve", side_effect=download):
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                ffi.fetch("x86_64-unknown-linux-gnu", self.root / "out")
        self.assertFalse((self.root / "out").exists())

    def test_checked_archive_refuses_paths_and_symlinks(self):
        for member in ("../escape", "/escape", "link"):
            path = self.root / "bad.tar.gz"
            with tarfile.open(path, "w:gz") as tar:
                info = tarfile.TarInfo(member)
                if member == "link":
                    info.type = tarfile.SYMTYPE
                    info.linkname = "../escape"
                tar.addfile(info, io.BytesIO(b""))
            self.pin(path)
            with patch.object(
                ffi.urllib.request, "urlretrieve", side_effect=lambda url, dest: shutil.copyfile(path, dest)
            ):
                with self.assertRaisesRegex(ValueError, "archive member"):
                    ffi.fetch("x86_64-unknown-linux-gnu", self.root / "out")
            self.assertFalse((self.root / "out").exists())

    def test_reproducible_archive_roundtrip(self):
        directory = self.root / "input"
        directory.mkdir()
        (directory / "manifest.json").write_text(
            json.dumps(
                {"version": "1.2.3", "target": "x86_64-unknown-linux-gnu", "features": "default", "profile": "release"}
            )
        )
        (directory / "library").write_bytes(b"static native library")
        ffi.archive(directory, self.root / "archives")
        path = next((self.root / "archives").glob("*.tar.gz"))
        before = path.read_bytes()
        os.utime(directory / "library", (42, 42))
        ffi.archive(directory, self.root / "archives")
        self.assertEqual(before, path.read_bytes())
        self.pin(path)
        with patch.object(ffi.urllib.request, "urlretrieve", side_effect=lambda url, dest: shutil.copyfile(path, dest)):
            ffi.fetch("x86_64-unknown-linux-gnu", self.root / "out")
        self.assertEqual((self.root / "out/library").read_bytes(), b"static native library")

    def test_same_run_extract_checks_sidecar_and_manifest(self):
        directory = self.root / "input"
        directory.mkdir()
        manifest = {
            "version": "1.2.3",
            "target": "x86_64-unknown-linux-gnu",
            "features": "default",
            "profile": "release",
        }
        (directory / "manifest.json").write_text(json.dumps(manifest))
        ffi.archive(directory, self.root / "archives")
        ffi.extract("1.2.3", manifest["target"], self.root / "archives", self.root / "good")
        self.assertEqual(json.loads((self.root / "good/manifest.json").read_text()), manifest)
        path = next((self.root / "archives").glob("*.tar.gz"))
        path.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            ffi.extract("1.2.3", manifest["target"], self.root / "archives", self.root / "bad")
        self.assertFalse((self.root / "bad").exists())

    def test_checksummed_archive_must_match_selected_manifest(self):
        directory = self.root / "input"
        directory.mkdir()
        manifest = {
            "version": "9.9.9",
            "target": "x86_64-unknown-linux-gnu",
            "features": "default",
            "profile": "release",
        }
        for field, value in (
            ("version", "9.9.9"),
            ("target", "aarch64-unknown-linux-gnu"),
            ("features", "minimal"),
            ("profile", "debug"),
        ):
            current = dict(manifest, version="1.2.3", **{field: value}) if field != "version" else manifest
            (directory / "manifest.json").write_text(json.dumps(current))
            ffi.archive(directory, self.root / "archives")
            path = self.root / "archives" / (ffi.name(current["version"], current["target"]) + ".tar.gz")
            self.pin(path)
            output = self.root / ("out-" + field)
            with patch.object(
                ffi.urllib.request, "urlretrieve", side_effect=lambda url, dest: shutil.copyfile(path, dest)
            ):
                with self.assertRaisesRegex(ValueError, "manifest disagrees"):
                    ffi.fetch("x86_64-unknown-linux-gnu", output)
            self.assertFalse(output.exists())

    def test_publish_rejects_bad_sidecar_before_network(self):
        directory = self.root / "assets"
        directory.mkdir()
        for target in ffi.TARGETS:
            for suffix in (".tar.gz", ".tar.gz.sha256"):
                (directory / (ffi.name("1.2.3", target) + suffix)).write_bytes(b"bad")
        with patch.object(ffi.subprocess, "run") as command:
            with self.assertRaisesRegex(ValueError, "checksum disagrees"):
                ffi.publish("1.2.3", directory, "")
            command.assert_not_called()

    def test_publish_refuses_changed_existing_asset(self):
        directory = self.root / "assets"
        directory.mkdir()
        for target in ffi.TARGETS:
            filename = ffi.name("1.2.3", target) + ".tar.gz"
            (directory / filename).write_bytes(b"new")
            (directory / (filename + ".sha256")).write_text(f"{hashlib.sha256(b'new').hexdigest()}  {filename}\n")
        existing = next(directory.iterdir()).name

        def run(args, **kwargs):
            if args[2] == "download":
                (pathlib.Path(args[-1]) / existing).write_bytes(b"old")
            return subprocess.CompletedProcess(args, 0)

        with (
            patch.object(ffi.subprocess, "run", side_effect=run) as calls,
            patch.object(ffi.subprocess, "check_output", return_value=json.dumps({"assets": [{"name": existing}]})),
        ):
            with self.assertRaisesRegex(ValueError, "refusing to replace"):
                ffi.publish("1.2.3", directory, "")
            self.assertFalse(any(call.args[0][2] == "upload" for call in calls.call_args_list))

    def test_publish_creates_release_like_other_ffi_publishers(self):
        directory = self.root / "assets"
        directory.mkdir()
        for target in ffi.TARGETS:
            filename = ffi.name("1.2.3", target) + ".tar.gz"
            (directory / filename).write_bytes(b"new")
            (directory / (filename + ".sha256")).write_text(f"{hashlib.sha256(b'new').hexdigest()}  {filename}\n")

        def run(args, **kwargs):
            if args[2] == "view":
                return subprocess.CompletedProcess(args, 1, b"", b"release not found")
            return subprocess.CompletedProcess(args, 0)

        with (
            patch.object(ffi.subprocess, "run", side_effect=run) as calls,
            patch.object(ffi.subprocess, "check_output", return_value=json.dumps({"assets": []})),
        ):
            ffi.publish("1.2.3", directory, "moq-ffi-v1.2.2")
        create = next(call.args[0] for call in calls.call_args_list if call.args[0][2] == "create")
        self.assertEqual(create[-3:], ["--generate-notes", "--notes-start-tag", "moq-ffi-v1.2.2"])
        self.assertNotIn("--notes", create)
        self.assertEqual(sum(call.args[0][2] == "upload" for call in calls.call_args_list), 8)


@unittest.skipUnless(platform.system() == "Linux", "Linux CMake input fixtures")
class Configure(unittest.TestCase):
    def test_prebuilt_generation_never_builds_rust_and_refuses_bad_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            source = root / "source"
            shutil.copytree(REPO / "cpp/moq", source, ignore=shutil.ignore_patterns("build"))
            native = root / "native"
            (native / "lib").mkdir(parents=True)
            for name in ("libmoq_ffi.a", "libmoq_ffi.so"):
                (native / "lib" / name).touch()
            (native / "native-libs.txt").write_text("# no linker inputs needed by this fixture\n")
            manifest = {
                "version": "1.2.3",
                "target": "aarch64-unknown-linux-gnu"
                if platform.machine() == "aarch64"
                else "x86_64-unknown-linux-gnu",
                "features": "default",
                "profile": "release",
                "source": "a" * 40,
            }
            tools = root / "tools"
            tools.mkdir()
            generator = tools / "uniffi-bindgen-cpp"
            generator.write_text(
                "#!/usr/bin/env python3\nimport pathlib, sys\nout=pathlib.Path(sys.argv[sys.argv.index('--out-dir')+1]); out.mkdir(parents=True)\nfor name in ('moq.hpp','moq.cpp','moq_scaffolding.hpp','uniffi_expected.hpp'): (out/name).touch()\n"
            )
            generator.chmod(0o755)
            cargo = tools / "cargo"
            cargo.write_text("#!/usr/bin/env bash\necho unexpected Rust build >&2\nexit 99\n")
            cargo.chmod(0o755)
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"])

            def configure():
                return subprocess.run(
                    ["cmake", "-S", str(source), "-B", str(root / "good"), f"-DMOQ_FFI_DIR={native}"],
                    env=env,
                    capture_output=True,
                    text=True,
                )

            (native / "manifest.json").write_text(json.dumps(manifest))
            configured = configure()
            self.assertEqual(configured.returncode, 0, configured.stdout + configured.stderr)
            built = subprocess.run(
                ["cmake", "--build", str(root / "good"), "--target", "moq_ffi"], env=env, capture_output=True, text=True
            )
            self.assertEqual(built.returncode, 0, built.stdout + built.stderr)
            for field, value in (
                ("target", "unsupported"),
                ("features", "minimal"),
                ("profile", "debug"),
                ("version", "unsupported"),
            ):
                (native / "manifest.json").write_text(json.dumps(dict(manifest, **{field: value})))
                self.assertNotEqual(configure().returncode, 0)


if __name__ == "__main__":
    unittest.main()
