#!/usr/bin/env python3
"""Prove scoped upstream selection and failure sensitivity without a network."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from prepare import prepare

UPSTREAM = Path(sys.argv.pop(1)).read_text()


class Selection(unittest.TestCase):
    def run_suite(self, exit_code=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / "conformance.sh"
            script.write_text(prepare(UPSTREAM))
            client = root / "moxygen/moqtest/moqtest_client"
            client.parent.mkdir(parents=True)
            client.write_text("""#!/usr/bin/env python3
import json, os, sys
with open(os.environ["CONFORMANCE_CALLS"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
print("MoQTest verification result: SUCCESS")
sys.exit(int(os.environ["CONFORMANCE_EXIT"]))
""")
            client.chmod(0o755)
            calls = root / "calls.jsonl"
            env = dict(
                os.environ,
                SECTIONS="1 2 3 4 5 6 7",
                SKIP_FETCH="1",
                MOXYGEN_DIR=str(root),
                CONFORMANCE_CALLS=str(calls),
                CONFORMANCE_EXIT=str(exit_code),
            )
            result = subprocess.run(
                ["bash", str(script), "http://127.0.0.1:1", "16", "Q"],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
            )
            return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def test_every_selected_case_uses_subscribe_and_supported_forwarding(self):
        result, calls = self.run_suite()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(calls), 29)
        preferences = set()
        for call in calls:
            self.assertIn("--request=subscribe", call)
            preference = next(arg for arg in call if arg.startswith("--forwarding_preference="))
            self.assertIn(preference, ["--forwarding_preference=0", "--forwarding_preference=3"])
            preferences.add(preference)
        self.assertEqual(len(preferences), 2)
        self.assertIn("EXCLUDED (nonzero subgroup)", result.stdout)
        print(f"selected {len(calls)} upstream cases")

    def test_a_client_crash_after_success_still_fails_the_suite(self):
        result, calls = self.run_suite(5)
        self.assertTrue(calls)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Client exited with status 5", result.stdout)

    def test_an_upstream_selection_change_is_refused(self):
        with self.assertRaises(ValueError):
            prepare(UPSTREAM.replace("local args=", "local renamed="))


if __name__ == "__main__":
    unittest.main()
