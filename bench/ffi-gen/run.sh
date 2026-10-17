#!/usr/bin/env bash
# Build the slice with each generator, generate its bindings, then run the benchmarks
# and the size table. See README.md.
#
#   bench/ffi-gen/run.sh [build|bench|size|all]...   (default: all)
#
# MOQ_BENCH_SECONDS sets each case's time budget (default 1). MOQ_BENCH_ROUNDS sets how
# many times each runner repeats, alternating which generator goes first (default 3).
set -euo pipefail

dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$dir/../.." && pwd)
out="$root/target/ffi-gen"
boltffi_version=0.31.1
typescript_version=7.0.2
types_node_version=26.6.4

host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
    *-apple-*) dylib=dylib os=darwin origin=@loader_path ;;
    *-linux-*) dylib=so os=linux origin="\$ORIGIN" ;;
    *) echo "ffi-gen: unsupported host $host" >&2 && exit 1 ;;
esac

# The release library each language package loads.
lib() {
    echo "$out/cargo/$1/release/libmoq_ffi_gen.$dylib"
}

boltffi() {
    (cd "$dir" && CARGO_TARGET_DIR="$out/cargo/boltffi" PATH="$out/node_modules/.bin:$PATH" \
        "$out/tools/bin/boltffi" "$@")
}

build_tools() {
    [[ -x "$out/tools/bin/boltffi" ]] ||
        cargo install --locked --quiet --root "$out/tools" "boltffi_cli@$boltffi_version"
    # BoltFFI's wasm packaging transpiles its TypeScript with `tsc`, which resolves these
    # from an ancestor of its output: the repo's versions, and the runtime its glue imports.
    [[ -d "$out/node_modules/@boltffi/runtime" ]] ||
        npm install --silent --no-save --prefix "$out" "typescript@$typescript_version" "@types/node@$types_node_version" \
            "@boltffi/runtime@$boltffi_version"
    [[ -x "$out/venv/bin/python" ]] || uv venv --quiet --seed --python "$(command -v python3)" "$out/venv"
    uv pip install --quiet --python "$out/venv/bin/python" setuptools
}

# moq's own UniFFI bindgen: the generator fork the language packages ship from.
bindgen() {
    cargo run --locked --quiet --release --package uniffi-bindgen --features cli --manifest-path "$root/Cargo.toml" -- "$@"
}

build_uniffi() {
    cargo build --locked --release --features uniffi --manifest-path "$dir/Cargo.toml" --target-dir "$out/cargo/uniffi"
    rm -rf "$out/uniffi"
    for lang in python kotlin swift; do
        bindgen generate --library "$(lib uniffi)" --language "$lang" --out-dir "$out/uniffi/$lang" --no-format
    done
    cp "$(lib uniffi)" "$out/uniffi/python/"
    # The browser half: ubrn, UniFFI's JS generator, does not support 0.32 yet (#5478),
    # so only the `.wasm` is measured, after wasm-bindgen strips it as it would ship.
    cargo build --locked --profile wasm-release --target wasm32-unknown-unknown --no-default-features \
        --features uniffi --manifest-path "$dir/Cargo.toml" --target-dir "$out/cargo/uniffi"
    wasm-bindgen --target web --out-dir "$out/uniffi/wasm" \
        "$out/cargo/uniffi/wasm32-unknown-unknown/wasm-release/moq_ffi_gen.wasm"
}

build_boltffi() {
    cargo build --locked --release --features boltffi --manifest-path "$dir/Cargo.toml" --target-dir "$out/cargo/boltffi"
    rm -rf "$out/boltffi"
    # --deny-skipped: fail rather than silently drop a declaration a backend cannot bind.
    for target in python kotlin swift; do
        boltffi generate "$target" --deny-skipped
    done
    # Python is a CPython extension module, compiled against the release library.
    boltffi pack python --release --deny-skipped --python "$out/venv/bin/python"
    uv pip install --quiet --python "$out/venv/bin/python" --reinstall --no-deps \
        --target "$out/boltffi/python-site" "$out"/boltffi/python/wheelhouse/*.whl
    # Kotlin on a desktop JVM loads JNI glue, linked against the release library.
    local java_home="${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")}"
    cc -shared -fPIC -O2 -o "$(dirname "$(lib boltffi)")/libmoq_ffi_gen_jni.$dylib" \
        -I"$out/boltffi/kotlin/jni" -I"$java_home/include" -I"$java_home/include/$os" \
        "$out/boltffi/kotlin/jni/jni_glue.c" -L"$(dirname "$(lib boltffi)")" -lmoq_ffi_gen -Wl,-rpath,"$origin"
    # The browser package: `.wasm`, loader, and TypeScript glue. BoltFFI's macros emit the
    # FFI only under its CLI's build, which accepts no custom profile, so `release` is
    # overridden to match `wasm-release`.
    CARGO_PROFILE_RELEASE_OPT_LEVEL=z boltffi pack wasm --release --deny-skipped
}

# moq-ffi and moq-wasm themselves, for the size table's reference rows.
build_reference() {
    cargo build --locked --release --package moq-ffi --manifest-path "$root/Cargo.toml" --target-dir "$out/cargo/moq-ffi"
    cp "$out/cargo/moq-ffi/release/libmoq_ffi.$dylib" "$out/cargo/moq-ffi/libmoq_ffi-default.$dylib"
    rm -rf "$out/moq-ffi"
    for lang in python kotlin swift; do
        bindgen generate --library "$out/cargo/moq-ffi/libmoq_ffi-default.$dylib" --language "$lang" \
            --out-dir "$out/moq-ffi/$lang" --no-format
    done
    cargo build --locked --release --package moq-ffi --no-default-features --manifest-path "$root/Cargo.toml" \
        --target-dir "$out/cargo/moq-ffi"
    cp "$out/cargo/moq-ffi/release/libmoq_ffi.$dylib" "$out/cargo/moq-ffi/libmoq_ffi-no-default.$dylib"
    cargo build --locked --profile wasm-release --target wasm32-unknown-unknown --package moq-ffi \
        --manifest-path "$root/Cargo.toml" --target-dir "$out/cargo/moq-ffi"
    wasm-bindgen --target web --out-dir "$out/moq-ffi/wasm" \
        "$out/cargo/moq-ffi/wasm32-unknown-unknown/wasm-release/moq_ffi.wasm"
    cargo build --locked --profile wasm-release --target wasm32-unknown-unknown --package moq-wasm \
        --manifest-path "$root/Cargo.toml" --target-dir "$out/cargo/moq-wasm"
    rm -rf "$out/moq-wasm"
    wasm-bindgen --target web --out-name moq --out-dir "$out/moq-wasm" \
        "$out/cargo/moq-wasm/wasm32-unknown-unknown/wasm-release/moq_wasm.wasm"
}

# The same rounds in Rust with no boundary: the floor both generators add to.
bench_rust() {
    cargo run --locked --quiet --release --example bench --features uniffi --manifest-path "$dir/Cargo.toml" \
        --target-dir "$out/cargo/rust" >"$1"
}

bench_python() {
    local path
    case "$1" in
        uniffi) path="$out/uniffi/python" ;;
        boltffi) path="$out/boltffi/python-site" ;;
    esac
    PYTHONPATH="$path" "$out/venv/bin/python" "$dir/py/bench.py" "$1" >"$2"
}

bench_kotlin() {
    MOQ_GEN_LIB="$(dirname "$(lib "$1")")" MOQ_GEN_BINDINGS="$out/$1/kotlin" \
        gradle --quiet --project-cache-dir "$out/gradle" -p "$dir/kt" ":$1:run" --args="$2"
}

bench() {
    rm -rf "$out/results"
    mkdir -p "$out/results"
    local rounds=${MOQ_BENCH_ROUNDS:-3} order
    for ((round = 0; round < rounds; round++)); do
        # Alternate which generator goes first, so drift on a shared host hits both.
        order=(uniffi boltffi)
        ((round % 2)) && order=(boltffi uniffi)
        echo "ffi-gen: round $round rust" >&2
        bench_rust "$out/results/rust-$round.json"
        for gen in "${order[@]}"; do
            echo "ffi-gen: round $round python $gen" >&2
            bench_python "$gen" "$out/results/python-$gen-$round.json"
        done
        for gen in "${order[@]}"; do
            echo "ffi-gen: round $round kotlin $gen" >&2
            bench_kotlin "$gen" "$out/results/kotlin-$gen-$round.json"
        done
    done
    python3 "$dir/report.py" "$out/results"
}

step() {
    case "$1" in
        build)
            build_tools
            build_uniffi
            build_boltffi
            ;;
        bench) bench ;;
        # Only the size table needs moq-ffi and moq-wasm, which take longer to build than
        # the slice, so the nightly's `build bench` skips them.
        size)
            build_reference
            python3 "$dir/size.py" "$out"
            ;;
        all)
            step build
            step bench
            step size
            ;;
        *)
            echo "usage: $0 [build|bench|size|all]..." >&2
            exit 1
            ;;
    esac
}

for arg in "${@:-all}"; do
    step "$arg"
done
