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

# The bindings are not committed: generate them into the package, where its
# tests and the publish copy below find them.
generated=$(mktemp -d)
trap 'rm -rf "$generated"' EXIT

MOQ_FFI_PROFILE=debug MOQ_DART_LIBRARY_OUT="$generated/library" "$DART_DIR/../sh/dart/generate.sh"
library_path=$(<"$generated/library")
mkdir -p "$generated/lib"
pinned_library="$generated/lib/$(basename "$library_path")"
cp "$library_path" "$pinned_library"

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
test_dir="$generated/test"
mkdir -p "$publish_dir" "$test_dir" "$generated/bin"
# Isolate Pub's file discovery from the checkout's ignores when TMPDIR is
# inside .tmp. Untracked copies also avoid the dirty-tree publish check.
git init --quiet "$publish_dir"

# Refuse any Cargo invocation from the Dart build hook during `dart test`:
# generate.sh already built the library with full cache env, and the staged
# pubspec pins it through hooks.user_defines.moq_ffi.library.
cat >"$generated/bin/cargo" <<'SHIM'
#!/usr/bin/env bash
echo "error: the moq_ffi build hook invoked cargo despite hooks.user_defines.moq_ffi.library" >&2
exit 1
SHIM
chmod +x "$generated/bin/cargo"

pinned_yaml="'${pinned_library//\'/\'\'}'"
for package in moq_ffi moq; do
    cp -R "$DART_DIR/$package" "$publish_dir/$package"
    rm -rf "$publish_dir/$package/.dart_tool" "$publish_dir/$package/build"

    cp -R "$DART_DIR/$package" "$test_dir/$package"
    rm -rf "$test_dir/$package/.dart_tool" "$test_dir/$package/build"
    cat >>"$test_dir/$package/pubspec.yaml" <<YAML

hooks:
  user_defines:
    moq_ffi:
      library: $pinned_yaml
YAML
done

for package in moq_ffi moq; do
    (
        cd "$DART_DIR/$package"
        dart analyze
    )
    (
        cd "$test_dir/$package"
        dart pub get --offline --enforce-lockfile
        PATH="$generated/bin:$PATH" dart test
    )
done

# Validate staged copies so this pre-commit check exercises package contents.
for package in moq_ffi moq; do
    (
        cd "$publish_dir/$package"
        dart pub get --offline --enforce-lockfile
        dart pub publish --dry-run 2>&1 | tee "$generated/$package.publish"
    )
done

# Publishing skips what .gitignore names, and the generated bindings are
# ignored in the checkout. A consumer installing moq_ffi has no generator.
for file in moq.dart uniffi_runtime.dart; do
    if ! grep -q -- "── $file (" "$generated/moq_ffi.publish"; then
        echo "error: the moq_ffi archive lacks lib/src/$file" >&2
        exit 1
    fi
done
