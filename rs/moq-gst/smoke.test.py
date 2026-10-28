"""Plugin discovery consumes inspection output and checks both factories."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SMOKE = Path(__file__).with_name("smoke.sh")


class SmokeTest(unittest.TestCase):
    def inspect(self, mode):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            inspect = scratch / "gst-inspect-1.0"
            inspect.write_text('''#!/usr/bin/env python3
import os
import signal
import sys

signal.signal(signal.SIGPIPE, signal.SIG_DFL)
if sys.argv[1] == "--version":
    print("gst-inspect-1.0 test", flush=True)
else:
    assert sys.argv[1] == "moq"
    assert os.environ["GST_PLUGIN_PATH_1_0"] == os.environ["PLUGIN_DIR"]
    assert os.environ["GST_PLUGIN_SYSTEM_PATH_1_0"] == ""
    assert os.environ["GST_REGISTRY_1_0"] != os.environ["INHERITED_REGISTRY"]
    print("  moqsink: MoQ Sink", flush=True)
    if os.environ["INSPECT_MODE"] != "missing":
        print("  moqsrc: MoQ Src", flush=True)
# A reader must allow the writer to finish after finding its first match.
for _ in range(16):
    sys.stdout.write("x" * 4096 + "\\n")
    sys.stdout.flush()
if sys.argv[1] == "moq" and os.environ["INSPECT_MODE"] == "error":
    sys.exit(1)
''')
            inspect.chmod(0o755)
            env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}", BASH_ENV=os.devnull,
                       PLUGIN_DIR=str(scratch), INSPECT_MODE=mode,
                       GST_PLUGIN_PATH_1_0="inherited", GST_PLUGIN_SYSTEM_PATH_1_0="inherited",
                       GST_REGISTRY_1_0=str(scratch / "inherited-registry"),
                       INHERITED_REGISTRY=str(scratch / "inherited-registry"))
            return subprocess.run(["bash", str(SMOKE), str(scratch)], env=env,
                                  text=True, capture_output=True)

    def test_large_inspection_completes(self):
        result = self.inspect("complete")
        self.assertEqual(result.returncode, 0, result.stderr[:1000])
        self.assertIn("Smoke test passed", result.stdout)

    def test_missing_factory_is_refused(self):
        result = self.inspect("missing")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("didn't find moqsink/moqsrc", result.stderr)

    def test_inspection_failure_is_refused_even_with_both_factories(self):
        self.assertNotEqual(self.inspect("error").returncode, 0)


if __name__ == "__main__":
    unittest.main()
