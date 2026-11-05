#!/usr/bin/env python3
"""Exercise history recovery without network access or native builds."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("nightly.sh").resolve()
REPORT = Path(__file__).with_name("report.py").resolve()


class History(unittest.TestCase):
    def test_skips_retained_artifact_without_history(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            (root / "sh/size").mkdir(parents=True)
            (root / "sh/size/report.py").symlink_to(REPORT)
            gh = root / "bin/gh"
            gh.write_text("""#!/usr/bin/env python3
import json, pathlib, sys
args = sys.argv[1:]
if args[:2] == ['run', 'list']:
    print('2\\n1')
elif args[0] == 'api':
    print('artifact')
elif args[:2] == ['run', 'download']:
    out = pathlib.Path(args[args.index('--dir') + 1])
    out.mkdir(parents=True)
    if args[2] == '1':
        (out / 'history.json').write_text(json.dumps([{'version': 1, 'date': '2026-10-09', 'bytes': {'a': 100}}]))
    else:
        (out / 'sizes.json').write_text('{}')
else:
    raise SystemExit(args)
""")
            gh.chmod(0o755)
            collect = root / "sh/size/collect.sh"
            collect.write_text("""#!/usr/bin/env bash
set -eu
printf '%s' '{"version":1,"date":"2026-10-10","bytes":{"a":100}}' >"$1/sizes.json"
printf 'attribution' >"$1/ffi-bloat.txt"
""")
            collect.chmod(0o755)
            env = dict(
                os.environ, PATH=f"{root / 'bin'}:{os.environ['PATH']}", GITHUB_STEP_SUMMARY=str(root / "summary")
            )
            result = subprocess.run(
                ["bash", str(SCRIPT), "out", "3"], cwd=root, env=env, capture_output=True, text=True
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            history = json.loads((root / "out/history.json").read_text())
            self.assertEqual([row["date"] for row in history], ["2026-10-09", "2026-10-10"])


unittest.main()
