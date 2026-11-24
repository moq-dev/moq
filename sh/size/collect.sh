#!/usr/bin/env bash
# Measure a workflow run's release uploads and the builds no release job makes, retaining diagnostics.
set -euo pipefail
out=$(realpath -m "${1:?usage: collect.sh OUTPUT [RUN]}")
run=${2:-}
mkdir -p "$out"
uv run --no-project python sh/js/install.py frozen
just js build
bun sh/size/bundles.ts "$out/js"
# Symbols are necessary only for attribution; they never replace the measured shipping sizes.
cargo bloat --locked --profile profiling -p moq-ffi --crates --lib >"$out/ffi-bloat.txt"
python3 - "$out" "$run" <<'PY'
import datetime, json, pathlib, subprocess, sys
sys.path.insert(0, "sh/size")
import release
out, run = pathlib.Path(sys.argv[1]), sys.argv[2]
sizes = json.loads((out / 'js/bytes.json').read_text())
report = {'version': 1, 'date': datetime.date.today().isoformat(),
          'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
          'bytes': sizes}
if run:
    release.download(run, out / 'release')
    sizes.update(release.sizes(out / 'release'))
    report['seconds'] = release.seconds(run)
(out / 'sizes.json').write_text(json.dumps(report, indent=2) + '\n')
PY
