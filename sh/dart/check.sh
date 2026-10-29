#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

DART_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../dart" && pwd)
ACTION=check

if [[ "${1:-}" == --fix ]]; then
    ACTION=fix
    shift
fi

for tool in cargo dart uniffi_bindgen_dart; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "dart check: skipping, $tool is not installed" >&2
        exit 0
    fi
done

# Dependabot refreshes moq_ffi's lock but never moq's, so moq's drifts until
# someone reruns the solver. Refreshing here makes `just dart fix` the remedy
# for the `--enforce-lockfile` failure that drift causes below; enforcing it in
# fix mode would instead fail before reaching the fix.
for package in moq_ffi moq; do
    (
        cd "$DART_DIR/$package"
        if [[ "$ACTION" == fix ]]; then
            dart pub get
        else
            dart pub get --enforce-lockfile
        fi
    )
done

if [[ "$ACTION" == fix ]]; then
    dart format "$DART_DIR/moq_ffi" "$DART_DIR/moq"
    exit 0
fi

# package.sh injects the real version from the moq-ffi-v* tag. A hardcoded one
# would silently go stale on the next release-plz bump, and the build hook uses
# it to name the native assets it downloads, so it would fetch a library whose
# ABI no longer matches these bindings.
pubspec_version=$(sed -n 's/^version: //p' "$DART_DIR/moq_ffi/pubspec.yaml" | head -1)
hook_version=$(sed -n "s/^const ffiVersion = '\(.*\)';/\1/p" "$DART_DIR/moq_ffi/hook/build.dart" | head -1)
for pinned in "$pubspec_version:moq_ffi/pubspec.yaml" "$hook_version:moq_ffi/hook/build.dart"; do
    if [[ "${pinned%%:*}" != "0.0.0-dev" ]]; then
        echo "error: ${pinned#*:} pins ${pinned%%:*}; commit the 0.0.0-dev sentinel instead" >&2
        exit 1
    fi
done

generated=$(mktemp -d)
trap 'rm -rf "$generated"' EXIT

# generate.sh and the build hook both reach Cargo through PATH, which Dart's
# filter keeps. A recorder in front of Cargo saves each build's environment and
# stderr, so a rebuild below can name its cause. It leaves out the variables its
# own shell sets, and leaves PATH before Cargo starts, or mbx's Cargo shim would
# find the recorder again as the real Cargo. Only builds are recorded:
# uniffi_bindgen_dart runs `cargo metadata` in the full environment.
real_cargo=$(command -v cargo)
mkdir "$generated/bin"
record_cargo() {
    local phase=$1 flags=$2
    cat >"$generated/bin/cargo" <<EOF
#!/usr/bin/env bash
if [[ "\${1:-}" == build-ffi ]]; then
    env -u PWD -u OLDPWD -u SHLVL -u _ -0 | sort -z >"$generated/$phase.env"
    set -- "\$@" $flags
fi
export PATH=\${PATH#"$generated/bin:"}
exec "$real_cargo" "\$@" 2> >(tee -a "$generated/$phase.log" >&2)
EOF
    chmod +x "$generated/bin/cargo"
}
export PATH="$generated/bin:$PATH"

record_cargo generate ""
MOQ_FFI_PROFILE=debug "$DART_DIR/../sh/dart/generate.sh" "$generated"
touch "$generated/built"
# Verbose, so Cargo says why it rebuilt anything.
record_cargo hook --verbose

for file in moq.dart uniffi_runtime.dart; do
    if ! diff -u -B "$DART_DIR/moq_ffi/lib/src/$file" "$generated/$file"; then
        echo "error: Dart bindings are stale; run 'just dart generate'" >&2
        exit 1
    fi
done

# Analyze samples alongside the wrapper tests so package imports resolve to
# this checkout. These functions only type-check; tests never execute them.
{
    echo "import 'prelude.dart';"
    bash "$DART_DIR/../doc/lib/samples.sh" dart \
        "$DART_DIR/../doc/lib/dart/"*.md "$DART_DIR/moq/README.md"
} >"$DART_DIR/moq/test/docs/samples.dart"
dart format "$DART_DIR/moq/test/docs/samples.dart"

dart format --output=none --set-exit-if-changed \
    "$DART_DIR/moq_ffi" "$DART_DIR/moq"

publish_dir="$generated/publish"
mkdir -p "$publish_dir"
for package in moq_ffi moq; do
    cp -R "$DART_DIR/$package" "$publish_dir/$package"
    rm -rf "$publish_dir/$package/.dart_tool" "$publish_dir/$package/build"

    (
        cd "$DART_DIR/$package"
        dart analyze
        dart test
    )
done

# The build hook reuses generate.sh's build only while both run Cargo with the
# same environment and that build's inputs stay in place. A Dart SDK bump that
# changes the hook's filter would otherwise silently double this check's Rust
# build. Cargo rewrites the .d dep-info file even on a no-op, so only the
# libraries count.
host=$(rustc -vV | sed -n 's/^host: //p')
if [[ -n "$(find "$DART_DIR/../target/$host/debug" -maxdepth 1 -name '*moq_ffi.*' ! -name '*.d' -newer "$generated/built")" ]]; then
    drift=$(
        { diff <(tr '\0' '\n' <"$generated/generate.env") <(tr '\0' '\n' <"$generated/hook.env") || true; } |
            sed -n 's/^[<>] \([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' | sort -u | paste -sd ' ' -
    )
    if [[ -n "$drift" ]]; then
        echo "error: the moq_ffi build hook ran Cargo with $drift set differently than sh/dart/generate.sh; update hook_vars there to match the Dart SDK" >&2
    else
        echo "error: the moq_ffi build hook rebuilt moq-ffi in generate.sh's environment, so an input of that build changed or went missing, such as a generated source tree mbx gc evicted" >&2
    fi
    # Units rebuilt only because a dependency was are left out.
    grep -hs 'Dirty' "$generated/hook.log" | grep -v 'info of dependency' >&2 || true
    exit 1
fi

# Pub rejects dry runs from a dirty Git tree. Validate copies outside the
# checkout so this pre-commit check exercises package contents, not Git state.
for package in moq_ffi moq; do
    (
        cd "$publish_dir/$package"
        dart pub get --enforce-lockfile
        dart pub publish --dry-run
    )
done
