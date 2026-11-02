"""Map cargo tree output to workspace crate directories whatever the checkout path looks like."""

import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("ts_paths", Path(__file__).with_name("ts-paths.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def metadata(root: str, sep: str) -> dict:
    return {
        "workspace_root": root,
        "packages": [
            {"name": name, "version": version, "manifest_path": sep.join([root, "rs", name, "Cargo.toml"])}
            for name, version in (
                ("moq-cli", "0.1.0"),
                ("moq-lite", "0.2.0"),
                ("moq-macro", "0.1.0"),
                ("moq-unused", "0.1.0"),
            )
        ],
    }


class BuiltDirsTest(unittest.TestCase):
    def check(self, root: str, sep: str):
        tree = "".join(
            f"{line}\n"
            for line in (
                f"moq-cli v0.1.0 ({sep.join([root, 'rs', 'moq-cli'])})",
                f"moq-lite v0.2.0 ({sep.join([root, 'rs', 'moq-lite'])}) (*)",
                f"moq-macro v0.1.0 ({sep.join([root, 'rs', 'moq-macro'])}) (proc-macro)",
                "moq-lite v0.1.0",
                "serde v1.0.0",
                "serde_derive v1.0.0 (proc-macro)",
                "forked v0.1.0 (https://github.com/example/forked#abc123)",
            )
        )
        self.assertEqual(module.built_dirs(tree, metadata(root, sep)), {"rs/moq-cli", "rs/moq-lite", "rs/moq-macro"})

    def test_posix(self):
        self.check("/repo", "/")

    def test_windows(self):
        self.check("C:\\repo", "\\")

    def test_parentheses(self):
        self.check("/repo (copy)", "/")


class TsFilterTest(unittest.TestCase):
    def test_only_the_ts_entry(self):
        workflow = "".join(
            f"{line}\n"
            for line in (
                "          filters: |",
                "            obs:",
                '              - "rs/moq-c/**"',
                "            ts:",
                '              - "test/ts/**"',
                "              # A comment.",
                '              - "rs/moq-net/**"',
                "            uring:",
                '              - "rs/**"',
            )
        )
        self.assertEqual(
            module.ts_filter(workflow),
            '              - "test/ts/**"\n              # A comment.\n              - "rs/moq-net/**"\n',
        )

    def test_missing(self):
        with self.assertRaises(SystemExit):
            module.ts_filter('            obs:\n              - "rs/moq-c/**"\n')


if __name__ == "__main__":
    unittest.main()
