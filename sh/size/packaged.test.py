#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path
from packaged import sizes


class Packages(unittest.TestCase):
    def fixture(self, root, version):
        for name in [f"python/moq_ffi-{version}-py3-none-manylinux_2_28_aarch64.whl",
                     f"maven/dev/moq/moq-ffi-android/{version}/moq-ffi-android-{version}.aar",
                     f"maven/dev/moq/moq-ffi-jvm/{version}/moq-ffi-jvm-{version}.jar",
                     "swift/MoqFFI.xcframework.zip"]:
            path = Path(root) / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"package")

    def test_version_changes_keep_keys(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            self.fixture(a, "0.4.12")
            self.fixture(b, "0.4.13")
            self.assertEqual(sizes(a), sizes(b))
            self.assertEqual(len(sizes(a)), 4)

    def test_duplicate_and_missing_packages_refused(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(ValueError):
                sizes(root)
            self.fixture(root, "0.4.12")
            self.fixture(root, "0.4.13")
            with self.assertRaisesRegex(ValueError, "duplicate"):
                sizes(root)


unittest.main()
