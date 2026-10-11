#!/usr/bin/env bash
# Build the native shipping artifacts and consumer JS bundles, retaining diagnostics.
set -euo pipefail
out=$(realpath -m "${1:?usage: collect.sh OUTPUT [PACKAGED_ARTIFACTS]}")
packaged=${2:-}
mkdir -p "$out/native"
target=$(cargo metadata --locked --format-version 1 --no-deps | jq -r .target_directory)
case $(uname -s) in Linux) dylib=so ;; Darwin) dylib=dylib ;; *)
    echo 'native size capture requires Linux or macOS' >&2
    exit 1
    ;;
esac
for package in moq-ffi moq-c moq-relay moq-cli; do
    cargo build --locked --release -p "$package"
    case "$package" in
        moq-ffi) cp "$target/release/libmoq_ffi.$dylib" "$target/release/libmoq_ffi.a" "$out/native/" ;;
        moq-c) cp "$target/release/libmoq.a" "$out/native/" ;;
        moq-relay) cp "$target/release/moq-relay" "$out/native/" ;;
        moq-cli) cp "$target/release/moq" "$out/native/" ;;
    esac
done
cargo build --locked --release -p moq-ffi --no-default-features
cp "$target/release/libmoq_ffi.$dylib" "$out/native/libmoq_ffi-minimal.$dylib"
cp "$target/release/libmoq_ffi.a" "$out/native/libmoq_ffi-minimal.a"
bun install --frozen-lockfile
just js build
bun sh/size/bundles.ts "$out/js"
# Symbols are necessary only for attribution; they never replace the measured shipping sizes.
cargo bloat --locked --profile profiling -p moq-ffi --crates --lib >"$out/ffi-bloat.txt"
python3 - "$out" "$packaged" <<'PY'
import datetime, json, pathlib, subprocess, sys
sys.path.insert(0, "sh/size")
from packaged import sizes as packaged_sizes
out = pathlib.Path(sys.argv[1])
host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
sizes = {f'native/{host}/{p.name}': p.stat().st_size for p in (out / 'native').iterdir()}
sizes.update(json.loads((out / 'js/bytes.json').read_text()))
if sys.argv[2]:
    sizes.update(packaged_sizes(sys.argv[2]))
report = {'version': 1, 'date': datetime.date.today().isoformat(),
          'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
          'bytes': sizes}
(out / 'sizes.json').write_text(json.dumps(report, indent=2) + '\n')
PY
