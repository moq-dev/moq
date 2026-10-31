#!/usr/bin/env bash
# Measure a workflow run's release uploads and the builds no release job makes, retaining diagnostics.
set -euo pipefail
out=$(realpath -m "${1:?usage: collect.sh OUTPUT [RUN]}")
run=${2:-}
mkdir -p "$out/native"
target=$(cargo metadata --locked --format-version 1 --no-deps | jq -r .target_directory)
case $(uname -s) in Linux) dylib=so ;; Darwin) dylib=dylib ;; *)
    echo 'native size capture requires Linux or macOS' >&2
    exit 1
    ;;
esac
# Release jobs ship default features only; this shows what the feature gates save.
# Its own target directory, since the cdylib would replace the default build's.
cargo build --locked --release -p moq-ffi --no-default-features --target-dir "$target/minimal"
cp "$target/minimal/release/libmoq_ffi.$dylib" "$out/native/libmoq_ffi-minimal.$dylib"
cp "$target/minimal/release/libmoq_ffi.a" "$out/native/libmoq_ffi-minimal.a"
bun install --frozen-lockfile
just js build
bun sh/size/bundles.ts "$out/js"
# Symbols are necessary only for attribution; they never replace the measured shipping sizes.
cargo bloat --locked --profile profiling -p moq-ffi --crates --lib >"$out/ffi-bloat.txt"
python3 - "$out" "$run" <<'PY'
import datetime, json, pathlib, subprocess, sys
sys.path.insert(0, "sh/size")
import release
out, run = pathlib.Path(sys.argv[1]), sys.argv[2]
host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
sizes = {f'native/{host}/{p.name}': p.stat().st_size for p in (out / 'native').iterdir()}
sizes.update(json.loads((out / 'js/bytes.json').read_text()))
report = {'version': 1, 'date': datetime.date.today().isoformat(),
          'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
          'bytes': sizes}
if run:
    release.download(run, out / 'release')
    sizes.update(release.sizes(out / 'release'))
    report['seconds'] = release.seconds(run)
(out / 'sizes.json').write_text(json.dumps(report, indent=2) + '\n')
PY
