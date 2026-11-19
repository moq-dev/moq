#!/usr/bin/env python3
"""A TLA+ config is re-checked only when its inputs change, unless it samples at random."""

import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# `just <lang> tla <spec> -config <cfg> ...` logs the config and exits as its status file says.
JUST = textwrap.dedent("""\
    #!/usr/bin/env bash
    all="$*"
    cfg=${all#*-config }
    cfg=${cfg%% *}
    echo "$cfg" >> "$TLA_CALLS"
    exit "$(cat "$TLA_STATUS/$(basename "$cfg")" 2>/dev/null || echo 0)"
    """)
TLC = textwrap.dedent("""\
    import os, sys
    assert sys.argv[1:] == ["--identity"], sys.argv
    print(os.environ["TLA_IDENTITY"])
    """)

EVERY = ["Exhaust.cfg", "Finding.cfg", "Random.cfg", "Seeded.cfg"]


class Cache(unittest.TestCase):
    def setUp(self):
        (ROOT / ".tmp").mkdir(exist_ok=True)
        self.repo = Path(tempfile.mkdtemp(dir=ROOT / ".tmp"))
        self.addCleanup(shutil.rmtree, self.repo)
        (self.repo / "sh").mkdir()
        shutil.copy(ROOT / "sh/tla.sh", self.repo / "sh/tla.sh")
        (self.repo / "sh/tlc.py").write_text(TLC)
        verify = self.repo / "rs/pkg/verify"
        verify.mkdir(parents=True)
        (verify / "M.tla").write_text("spec")
        self.write("Exhaust.cfg", "exhaust")
        self.write("Seeded.cfg", "\\* Simulate: -simulate num=10 -seed 1")
        self.write("Random.cfg", "\\* Simulate: -simulate num=10")
        self.write("Finding.cfg", "\\* Open finding: broken")
        for path in ("bin", "status"):
            (self.repo / path).mkdir()
        just = self.repo / "bin/just"
        just.write_text(JUST)
        just.chmod(0o755)
        subprocess.run(["git", "init", "-q"], cwd=self.repo, check=True)
        subprocess.run(["git", "add", "-A"], cwd=self.repo, check=True)
        self.identity = "tlc-1"
        self.cache = self.repo / "cache"

    def write(self, name, text):
        (self.repo / "rs/pkg/verify" / name).write_text(text)

    def exits(self, name, status):
        (self.repo / "status" / name).write_text(str(status))

    def run_tla(self, cache=True):
        calls = self.repo / "calls"
        calls.unlink(missing_ok=True)
        env = {
            **os.environ,
            "PATH": f"{self.repo / 'bin'}:{os.environ['PATH']}",
            "TLA_CALLS": str(calls),
            "TLA_STATUS": str(self.repo / "status"),
            "TLA_IDENTITY": self.identity,
        }
        env.pop("TLA_CACHE", None)
        if cache:
            env["TLA_CACHE"] = str(self.cache)
        result = subprocess.run(["bash", "sh/tla.sh"], cwd=self.repo, env=env, capture_output=True, text=True)
        ran = sorted(Path(line).name for line in calls.read_text().split()) if calls.exists() else []
        return result, ran

    def test_unchanged_inputs_skip_all_but_the_unseeded(self):
        self.exits("Finding.cfg", 11)
        result, ran = self.run_tla()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(ran, EVERY)
        result, ran = self.run_tla()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(ran, ["Random.cfg"])

    def test_no_cache_runs_everything(self):
        self.exits("Finding.cfg", 11)
        self.run_tla(cache=False)
        _, ran = self.run_tla(cache=False)
        self.assertEqual(ran, EVERY)

    def test_a_changed_spec_config_or_tlc_runs_again(self):
        self.exits("Finding.cfg", 11)
        self.run_tla()
        (self.repo / "rs/pkg/verify/M.tla").write_text("spec 2")
        _, ran = self.run_tla()
        self.assertEqual(ran, EVERY)
        self.write("Exhaust.cfg", "exhaust 2")
        _, ran = self.run_tla()
        self.assertEqual(ran, ["Exhaust.cfg", "Random.cfg"])
        self.identity = "tlc-2"
        _, ran = self.run_tla()
        self.assertEqual(ran, EVERY)

    def test_a_failure_is_never_cached(self):
        self.exits("Finding.cfg", 11)
        self.exits("Exhaust.cfg", 12)
        result, _ = self.run_tla()
        self.assertEqual(result.returncode, 1)
        result, ran = self.run_tla()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(ran, ["Exhaust.cfg", "Random.cfg"])
        self.exits("Exhaust.cfg", 0)
        result, ran = self.run_tla()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(ran, ["Exhaust.cfg", "Random.cfg"])

    def test_a_fixed_finding_is_noticed(self):
        # A finding's marker records that it still breaks; its fix changes the
        # spec or config, which is what keys the result.
        self.exits("Finding.cfg", 11)
        self.run_tla()
        self.exits("Finding.cfg", 0)
        (self.repo / "rs/pkg/verify/M.tla").write_text("fixed")
        result, ran = self.run_tla()
        self.assertEqual(result.returncode, 1)
        self.assertIn("Finding.cfg", ran)
        self.assertIn("its finding is fixed", result.stderr)

    def test_markers_of_deleted_configs_are_removed(self):
        self.exits("Finding.cfg", 11)
        self.run_tla()
        self.assertEqual(len(list(self.cache.iterdir())), 3)
        (self.repo / "rs/pkg/verify/Exhaust.cfg").unlink()
        self.run_tla()
        self.assertEqual(len(list(self.cache.iterdir())), 2)


if __name__ == "__main__":
    unittest.main()
