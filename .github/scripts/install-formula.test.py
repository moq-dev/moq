"""Exercise successive releases into an existing tap without losing migrations."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("install_formula", Path(__file__).with_name("install-formula.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class InstallFormulaTest(unittest.TestCase):
    def test_successive_releases(self):
        with tempfile.TemporaryDirectory() as directory:
            tap = Path(directory)
            formulas = tap / "Formula"
            formulas.mkdir()
            for crate in ("moq-cli", "moq-token-cli", "moq-relay"):
                (formulas / f"{crate}.rb").write_text("old")
            (tap / "formula_renames.json").write_text('{"prior": "unrelated"}')
            (tap / "README.md").write_text("brew install moq-dev/tap/moq-cli moq-dev/tap/moq-token-cli\n")
            rendered = tap / "rendered.rb"
            rendered.write_text("new")
            module.install("moq-cli", rendered, tap)
            self.assertFalse((formulas / "moq-cli.rb").exists())
            self.assertEqual((formulas / "moq.rb").read_text(), "new")
            self.assertTrue((formulas / "moq-token-cli.rb").exists())
            self.assertNotIn("moq-token-cli", json.loads((tap / "formula_renames.json").read_text()))
            module.install("moq-token-cli", rendered, tap)
            module.install("moq-relay", rendered, tap)
            module.install("moq-cli", rendered, tap)
            self.assertEqual(json.loads((tap / "formula_renames.json").read_text()), {
                "prior": "unrelated", "moq-cli": "moq", "moq-token-cli": "moq-token",
            })
            self.assertEqual((tap / "README.md").read_text(), "brew install moq-dev/tap/moq moq-dev/tap/moq-token\n")
            self.assertEqual((formulas / "moq-relay.rb").read_text(), "new")


if __name__ == "__main__":
    unittest.main()
