#!/usr/bin/env bash
set -euo pipefail

workspace=$(git rev-parse --show-toplevel)
cd "$workspace"
mkdir -p .tmp
scratch=$(mktemp -d .tmp/dart-bench.XXXXXX)
cp dart/moq/pubspec.yaml "$scratch/pubspec.yaml"
restore() {
    cd "$workspace"
    cp "$scratch/pubspec.yaml" dart/moq/pubspec.yaml
    rm -rf "$scratch"
}
trap restore EXIT
trap 'exit 1' INT TERM

# Reuse the public hook's library override, keeping checkout builds quick
# without timing an unoptimized native library in this benchmark.
MOQ_FFI_PROFILE=release sh/dart/generate.sh "$scratch/generated"
host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
    *-apple-*) library=libmoq_ffi.dylib ;;
    *-windows-*) library=moq_ffi.dll ;;
    *) library=libmoq_ffi.so ;;
esac
cat >>dart/moq/pubspec.yaml <<YAML

hooks:
  user_defines:
    moq_ffi:
      library: ../../target/$host/release/$library
YAML

export MOQ_BENCH_DIR="${MOQ_BENCH_DIR:-$PWD/target/bench/ffi}"
cd dart/moq
dart pub get --enforce-lockfile
dart run bench/ffi.dart
