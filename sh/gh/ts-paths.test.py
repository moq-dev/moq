"""Parse cargo tree output into workspace crate directories on POSIX and Windows hosts."""

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("ts_paths", Path(__file__).with_name("ts-paths.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class BuiltDirsTest(unittest.TestCase):
    def test_posix(self):
        tree = (
            "moq-cli v0.1.0 (/repo/rs/moq-cli)\n"
            "moq-lite v0.2.0 (/repo/rs/moq-lite) (*)\n"
            "moq-macro v0.1.0 (/repo/rs/moq-macro) (proc-macro)\n"
            "serde v1.0.0\n"
            "serde_derive v1.0.0 (proc-macro)\n"
            "forked v0.1.0 (https://github.com/example/forked#abc123)\n"
        )
        self.assertEqual(module.built_dirs(tree, "/repo"), {"rs/moq-cli", "rs/moq-lite", "rs/moq-macro"})

    def test_windows(self):
        tree = "moq-cli v0.1.0 (C:\\repo\\rs\\moq-cli)\nmoq-lite v0.2.0 (C:\\repo\\rs\\moq-lite) (*)\nserde v1.0.0\n"
        self.assertEqual(module.built_dirs(tree, "C:\\repo"), {"rs/moq-cli", "rs/moq-lite"})


if __name__ == "__main__":
    unittest.main()
