#!/usr/bin/env bash
set -euo pipefail

# Assemble the dev.moq:moq-ffi Kotlin bindings package and stage it for
# publication. (The dev.moq:moq wrapper is pure Kotlin and published
# separately by release-kt-lib.yml.)
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
rm -rf "$KT_DIR/moq-ffi/src/androidMain/jniLibs"
rm -rf "$KT_DIR/moq-ffi/src/jvmMain/resources"
rm -rf "$KT_DIR/moq-ffi/src/jvmAndAndroidMain/kotlin/uniffi"
mkdir -p "$KT_DIR/moq-ffi/src/androidMain/jniLibs"
mkdir -p "$KT_DIR/moq-ffi/src/jvmMain/resources"

# Entries are "<cargo-target>:<...>" so the script stays portable to Bash 3.2
# (default on macOS), which has no associative arrays.

# --- Android JNI libs --- ("<cargo-target>:<android-abi>:<ndk-sysroot-triple>")
#
# libmoq_ffi.so links the NDK C++ runtime, because moq-video's openh264 fallback
# is C++ and cc-rs links Android C++ against libc++_shared by default. The AAR
# has to carry that runtime: an app with no C++ of its own never packages it, and
# would fail to load the binding with `libc++_shared.so not found`. The sysroot
# triple differs from the cargo target for 32-bit ARM.
ANDROID_ABIS=(
    "aarch64-linux-android:arm64-v8a:aarch64-linux-android"
    "armv7-linux-androideabi:armeabi-v7a:arm-linux-androideabi"
    "x86_64-linux-android:x86_64:x86_64-linux-android"
)

# Resolved once, and only when there is an Android lib to stage, so a
# desktop-only run needs no NDK. Assigned here rather than from a function: an
# `exit` inside `$(...)` would only leave the subshell, and the script would
# carry on and stage a lib whose C++ runtime is missing.
NDK_ROOT="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"

HAVE_ANDROID_LIBS=false
for entry in "${ANDROID_ABIS[@]}"; do
    IFS=':' read -r target abi sysroot_triple <<<"$entry"
    src="$LIB_DIR/$target/libmoq_ffi.so"
    if [[ ! -f "$src" ]]; then
        echo "  android $abi: skipped, $src missing"
        continue
    fi

    if [[ -z "$NDK_ROOT" ]]; then
        echo "Error: staging Android libs needs ANDROID_NDK_HOME / ANDROID_NDK_ROOT;" >&2
        echo "       libc++_shared.so must ship in the AAR or the binding won't load." >&2
        exit 1
    fi

    # Glob the host tag (darwin-x86_64 / linux-x86_64) rather than guessing it.
    cxx=$(echo "$NDK_ROOT"/toolchains/llvm/prebuilt/*/sysroot/usr/lib/"$sysroot_triple"/libc++_shared.so)
    if [[ ! -f "$cxx" ]]; then
        echo "Error: no libc++_shared.so for $sysroot_triple under $NDK_ROOT" >&2
        exit 1
    fi

    dest="$KT_DIR/moq-ffi/src/androidMain/jniLibs/$abi"
    mkdir -p "$dest"
    cp "$src" "$dest/"
    cp "$cxx" "$dest/"
    echo "  android $abi <- $target (+ libc++_shared.so)"
    HAVE_ANDROID_LIBS=true
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
        dest="$KT_DIR/moq-ffi/src/jvmMain/resources/$dir"
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
mkdir -p "$KT_DIR/moq-ffi/src/jvmAndAndroidMain/kotlin/uniffi/moq"
cp "$GENERATED_KT" "$KT_DIR/moq-ffi/src/jvmAndAndroidMain/kotlin/uniffi/moq/moq.kt"

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

"$GRADLE_CMD" -p "$KT_DIR" "${GRADLE_ARGS[@]}" :moq-ffi:assemble :moq-ffi:publishToMavenLocal
