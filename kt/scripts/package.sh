#!/usr/bin/env bash
set -euo pipefail

# Assemble the moq-ffi Kotlin package and stage it for publication.
#
# Designed to run after the workflow has placed per-target moq-ffi
# native libs into $LIB_DIR (one subdir per cargo target) and the
# uniffi-bindgen kotlin output at $BINDINGS_DIR/uniffi/moq/moq.kt.
#
# Usage:
#   kt/scripts/package.sh --version 0.0.0-dev --lib-dir libs --bindings-dir bindings --output dist
#
# Expected $LIB_DIR layout (per target, populated by the build matrix):
#   $LIB_DIR/aarch64-linux-android/libmoq_ffi.so
#   $LIB_DIR/armv7-linux-androideabi/libmoq_ffi.so
#   $LIB_DIR/x86_64-linux-android/libmoq_ffi.so
#   $LIB_DIR/x86_64-unknown-linux-gnu/libmoq_ffi.so
#   ... etc.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

VERSION=""
LIB_DIR=""
OUTPUT_DIR=""
BINDINGS_DIR=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --version)
            VERSION="$2"
            shift 2
            ;;
        --lib-dir)
            LIB_DIR="$2"
            shift 2
            ;;
        --output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --bindings-dir)
            BINDINGS_DIR="$2"
            shift 2
            ;;
        -h | --help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

[[ -z "$VERSION" ]] && {
    echo "Error: --version is required" >&2
    exit 1
}
[[ -z "$LIB_DIR" ]] && {
    echo "Error: --lib-dir is required" >&2
    exit 1
}
[[ -z "$BINDINGS_DIR" ]] && {
    echo "Error: --bindings-dir is required" >&2
    exit 1
}
[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="dist"

mkdir -p "$OUTPUT_DIR"

# Clean staging dirs.
rm -rf "$KT_DIR/moq/src/androidMain/jniLibs"
rm -rf "$KT_DIR/moq/src/jvmMain/resources"
rm -rf "$KT_DIR/moq/src/jvmAndAndroidMain/kotlin/uniffi"
mkdir -p "$KT_DIR/moq/src/androidMain/jniLibs"
mkdir -p "$KT_DIR/moq/src/jvmMain/resources"

# Entries are "<cargo-target>:<...>" so the script stays portable to Bash 3.2
# (default on macOS), which has no associative arrays.

# --- Android JNI libs --- ("<cargo-target>:<android-abi>")
ANDROID_ABIS=(
    "aarch64-linux-android:arm64-v8a"
    "armv7-linux-androideabi:armeabi-v7a"
    "x86_64-linux-android:x86_64"
)
HAVE_ANDROID_LIBS=false
for entry in "${ANDROID_ABIS[@]}"; do
    target="${entry%%:*}"
    abi="${entry##*:}"
    src="$LIB_DIR/$target/libmoq_ffi.so"
    if [[ -f "$src" ]]; then
        dest="$KT_DIR/moq/src/androidMain/jniLibs/$abi"
        mkdir -p "$dest"
        cp "$src" "$dest/"
        echo "  android $abi <- $target"
        HAVE_ANDROID_LIBS=true
    else
        echo "  android $abi: skipped, $src missing"
    fi
done

# --- JVM desktop resources (JNA classpath layout) ---
# Entries are "<cargo-target>:<jna-dir>:<libname>".
JVM_LIBS=(
    "x86_64-unknown-linux-gnu:linux-x86-64:libmoq_ffi.so"
    "aarch64-unknown-linux-gnu:linux-aarch64:libmoq_ffi.so"
    "universal-apple-darwin:darwin:libmoq_ffi.dylib"
    "aarch64-apple-darwin:darwin-aarch64:libmoq_ffi.dylib"
    "x86_64-apple-darwin:darwin-x86-64:libmoq_ffi.dylib"
    "x86_64-pc-windows-msvc:win32-x86-64:moq_ffi.dll"
)
for entry in "${JVM_LIBS[@]}"; do
    target="${entry%%:*}"
    rest="${entry#*:}"
    dir="${rest%%:*}"
    libname="${rest##*:}"
    src="$LIB_DIR/$target/$libname"
    if [[ -f "$src" ]]; then
        dest="$KT_DIR/moq/src/jvmMain/resources/$dir"
        mkdir -p "$dest"
        cp "$src" "$dest/"
        echo "  jvm $dir <- $target"
    else
        echo "  jvm $dir: skipped, $src missing"
    fi
done

# --- Uniffi-generated Kotlin source ---
GENERATED_KT="$BINDINGS_DIR/uniffi/moq/moq.kt"
[[ -f "$GENERATED_KT" ]] || {
    echo "Error: uniffi-bindgen output not found at $GENERATED_KT" >&2
    exit 1
}
mkdir -p "$KT_DIR/moq/src/jvmAndAndroidMain/kotlin/uniffi/moq"
cp "$GENERATED_KT" "$KT_DIR/moq/src/jvmAndAndroidMain/kotlin/uniffi/moq/moq.kt"

# --- Maven-local publish ---
MAVEN_LOCAL="$OUTPUT_DIR/maven-local"
mkdir -p "$MAVEN_LOCAL"

GRADLE_ARGS=("-Pmoqffi.version=$VERSION" "-Dmaven.repo.local=$(cd "$MAVEN_LOCAL" && pwd)")
if [[ "$HAVE_ANDROID_LIBS" == true ]]; then
    GRADLE_ARGS+=("-Pandroid.enabled=true")
fi

GRADLE_CMD="${GRADLE_CMD:-$(command -v gradle || true)}"
[[ -n "$GRADLE_CMD" ]] || {
    echo "Error: gradle not on PATH" >&2
    exit 1
}

"$GRADLE_CMD" -p "$KT_DIR" "${GRADLE_ARGS[@]}" :moq:assemble :moq:publishToMavenLocal
