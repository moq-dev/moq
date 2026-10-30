#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path
from release import sizes


def fixture(root, version):
    """The layout `gh run download` leaves for a nightly run's release uploads."""
    for name in [
        "go-lib-x86_64-unknown-linux-gnu/libmoq_ffi.a",
        "go-lib-x86_64-unknown-linux-gnu/libmoq_ffi.so",
        "go-lib-x86_64-pc-windows-msvc/moq_ffi.lib",
        f"moq-c-aarch64-unknown-linux-gnu/moq-c-{version}-aarch64-unknown-linux-gnu.tar.gz",
        f"moq-cli-x86_64-unknown-linux-gnu/moq-cli-v{version}-x86_64-unknown-linux-gnu.tar.gz",
        f"moq-cli-x86_64-unknown-linux-gnu/moq-cli-v{version}-x86_64-unknown-linux-gnu",
        f"moq-cli-x86_64-unknown-linux-gnu/moq_{version}_amd64.deb",
        f"moq-relay-x86_64-pc-windows-msvc/moq-relay-v{version}-x86_64-pc-windows-msvc.zip",
        f"moq-relay-x86_64-unknown-linux-gnu/moq-relay-{version}-1.x86_64.rpm",
        f"python-aarch64-unknown-linux-gnu/moq_ffi-{version}-py3-none-manylinux_2_28_aarch64.whl",
        f"kotlin-package/maven-local/dev/moq/moq-ffi-android/{version}/moq-ffi-android-{version}.aar",
        f"kotlin-package/maven-local/dev/moq/moq-ffi-jvm/{version}/moq-ffi-jvm-{version}.jar",
        f"kotlin-package/maven-local/dev/moq/moq-ffi-jvm/{version}/moq-ffi-jvm-{version}-sources.jar",
        f"kotlin-package/maven-local/dev/moq/moq-ffi-jvm/{version}/moq-ffi-jvm-{version}.pom",
        "swift-ffi-package/MoqFFI.xcframework.zip",
    ]:
        path = Path(root) / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"package")


class Release(unittest.TestCase):
    def test_version_changes_keep_keys(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            fixture(a, "0.4.12")
            fixture(b, "0.5.0")
            self.assertEqual(sizes(a), sizes(b))
            self.assertEqual(
                sorted(sizes(a)),
                [
                    "release/go-lib-x86_64-pc-windows-msvc/moq_ffi.lib",
                    "release/go-lib-x86_64-unknown-linux-gnu/libmoq_ffi.a",
                    "release/kotlin-package/moq-ffi-android.aar",
                    "release/kotlin-package/moq-ffi-jvm.jar",
                    "release/moq-c-aarch64-unknown-linux-gnu/moq-c-aarch64-unknown-linux-gnu.tar.gz",
                    "release/moq-cli-x86_64-unknown-linux-gnu/moq-cli-x86_64-unknown-linux-gnu.tar.gz",
                    "release/moq-relay-x86_64-pc-windows-msvc/moq-relay-x86_64-pc-windows-msvc.zip",
                    "release/python-aarch64-unknown-linux-gnu/moq_ffi-py3-none-manylinux_2_28_aarch64.whl",
                    "release/swift-ffi-package/MoqFFI.xcframework.zip",
                ],
            )

    def test_shared_native_artifacts_keep_history_keys(self):
        with tempfile.TemporaryDirectory() as old, tempfile.TemporaryDirectory() as shared:
            fixture(old, "0.4.12")
            fixture(shared, "0.4.12")
            for path in Path(shared).glob("go-lib-*"):
                path.rename(path.with_name(path.name.replace("go-lib-", "default-lib-", 1)))
            extra = Path(shared) / "default-lib-aarch64-apple-ios/libmoq_ffi.a"
            extra.parent.mkdir()
            extra.write_bytes(b"already counted in the Swift package")
            self.assertEqual(sizes(old), sizes(shared))

    def test_duplicate_and_missing_artifacts_refused(self):
        with tempfile.TemporaryDirectory() as root:
            fixture(root, "0.4.12")
            for path in Path(root, "moq-relay-x86_64-pc-windows-msvc").iterdir():
                path.unlink()
            with self.assertRaisesRegex(ValueError, "moq-relay"):
                sizes(root)
        with tempfile.TemporaryDirectory() as root:
            fixture(root, "0.4.12")
            fixture(root, "0.4.13")
            with self.assertRaisesRegex(ValueError, "duplicate"):
                sizes(root)


unittest.main()
