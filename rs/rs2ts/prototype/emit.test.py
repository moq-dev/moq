"""Check generation drift and rejection paths without requiring Charon in CI."""

import copy
import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).parent
spec = importlib.util.spec_from_file_location("emit", ROOT / "emit.py")
emitter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(emitter)


class EmitTests(unittest.TestCase):
    def setUp(self):
        self.data = json.loads((ROOT / "integers.llbc").read_text())

    def test_generated_output_is_current(self):
        self.assertEqual(emitter.emit(self.data), (ROOT / "generated.ts").read_text())
        source = (ROOT.parents[2] / "rs/moq-net/src/coding/varint.rs").read_text()
        for decl in self.data["translated"]["fun_decls"]:
            self.assertIn(decl["item_meta"]["source_text"], source)

    def test_refuses_partial_extraction(self):
        self.data["has_errors"] = True
        with self.assertRaisesRegex(ValueError, "partial Charon output"):
            emitter.emit(self.data)

    def test_refuses_unknown_version(self):
        self.data["charon_version"] = "0.1.999"
        with self.assertRaisesRegex(ValueError, "expected Charon"):
            emitter.emit(self.data)

    def test_refuses_non_scalar_types(self):
        decl = self.data["translated"]["fun_decls"][0]
        decl["signature"]["output"] = {"Adt": {"id": 0}}
        with self.assertRaisesRegex(ValueError, "unsupported type: Adt"):
            emitter.emit(self.data)

    def test_unknown_statements_report_source_location(self):
        stmt = self.data["translated"]["fun_decls"][0]["body"]["Structured"]["body"]["statements"][0]
        stmt["kind"] = {"FutureOperation": None}
        with self.assertRaisesRegex(ValueError, r"moq_net::coding::varint::zigzag:\d+:\d+: unsupported statement"):
            emitter.emit(self.data)

    def test_refuses_unwind_cleanup_it_cannot_emit(self):
        body = self.data["translated"]["fun_decls"][0]["body"]["Structured"]["body"]
        statement = next(s for s in body["statements"] if "Assert" in s["kind"])
        statement["kind"]["Assert"]["on_unwind"]["statements"] = [{"kind": {"Drop": None}}]
        with self.assertRaisesRegex(ValueError, "nontrivial unwind cleanup"):
            emitter.emit(self.data)

    def test_refuses_colliding_names(self):
        self.data["translated"]["fun_decls"].append(copy.deepcopy(self.data["translated"]["fun_decls"][0]))
        with self.assertRaisesRegex(ValueError, "names collide"):
            emitter.emit(self.data)


if __name__ == "__main__":
    unittest.main()
