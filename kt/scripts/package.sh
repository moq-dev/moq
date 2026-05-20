#!/usr/bin/env bash
set -euo pipefail

# Assemble the moq-ffi Kotlin packages (Android AAR + JVM JAR) and stage
# them for publication. Designed to run after rs/moq-ffi/build.sh has
# produced per-target binaries in $LIB_DIR.
#
# Usage:
#   kt/scripts/package.sh --version 0.0.0-dev --lib-dir dist --output dist
#
#   --version       Version string baked into the artifact metadata.
#   --lib-dir       Directory containing per-target moq-ffi outputs from
#                   rs/moq-ffi/build.sh (each subdir holds lib/libmoq_ffi.{so,a,dll,dylib}).
#   --output        Destination directory for the .tar.gz archive and
#                   the maven-local staging tree.
#   --bindings-dir  Directory containing uniffi-bindgen output (a
#                   `kotlin/uniffi/moq/moq.kt` will be picked up from
#                   here). Defaults to "$LIB_DIR/bindings".

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$KT_DIR/.." && pwd)"

VERSION=""
LIB_DIR=""
OUTPUT_DIR=""
BINDINGS_DIR=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --version) VERSION="$2"; shift 2;;
        --lib-dir) LIB_DIR="$2"; shift 2;;
        --output) OUTPUT_DIR="$2"; shift 2;;
        --bindings-dir) BINDINGS_DIR="$2"; shift 2;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown option: $1" >&2; exit 1;;
    esac
done

[[ -z "$VERSION" ]] && { echo "Error: --version is required" >&2; exit 1; }
[[ -z "$LIB_DIR" ]] && { echo "Error: --lib-dir is required" >&2; exit 1; }
[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="dist"
[[ -z "$BINDINGS_DIR" ]] && BINDINGS_DIR="$LIB_DIR/bindings"

mkdir -p "$OUTPUT_DIR"

# Clean staging dirs.
rm -rf "$KT_DIR/moq-android/src/main/jniLibs"
rm -rf "$KT_DIR/moq-jvm/src/main/resources"
mkdir -p "$KT_DIR/moq-android/src/main/jniLibs"
mkdir -p "$KT_DIR/moq-jvm/src/main/resources"

# --- Android JNI libs ---
# Map cargo target -> Android ABI.
declare -A ANDROID_ABIS=(
    [aarch64-linux-android]=arm64-v8a
    [armv7-linux-androideabi]=armeabi-v7a
    [x86_64-linux-android]=x86_64
)

for target in "${!ANDROID_ABIS[@]}"; do
    abi="${ANDROID_ABIS[$target]}"
    src="$LIB_DIR/moq-ffi-${VERSION}-${target}/lib/libmoq_ffi.so"
    if [[ -f "$src" ]]; then
        dest="$KT_DIR/moq-android/src/main/jniLibs/$abi"
        mkdir -p "$dest"
        cp "$src" "$dest/"
        echo "  android $abi <- $target"
    else
        echo "  android $abi: skipped, $src missing"
    fi
done

# --- JVM desktop resources (JNA classpath layout) ---
# JNA looks for libraries at:
#   <os>-<arch>/<libname>   in classpath resources
# See com.sun.jna.Platform.RESOURCE_PREFIX.
declare -A JVM_LIBS=(
    [x86_64-unknown-linux-gnu]="linux-x86-64:libmoq_ffi.so"
    [aarch64-unknown-linux-gnu]="linux-aarch64:libmoq_ffi.so"
    [universal-apple-darwin]="darwin:libmoq_ffi.dylib"
    [aarch64-apple-darwin]="darwin-aarch64:libmoq_ffi.dylib"
    [x86_64-apple-darwin]="darwin-x86-64:libmoq_ffi.dylib"
    [x86_64-pc-windows-msvc]="win32-x86-64:moq_ffi.dll"
)

for target in "${!JVM_LIBS[@]}"; do
    spec="${JVM_LIBS[$target]}"
    dir="${spec%%:*}"
    libname="${spec##*:}"
    src="$LIB_DIR/moq-ffi-${VERSION}-${target}/lib/$libname"
    if [[ -f "$src" ]]; then
        dest="$KT_DIR/moq-jvm/src/main/resources/$dir"
        mkdir -p "$dest"
        cp "$src" "$dest/"
        echo "  jvm $dir <- $target"
    else
        echo "  jvm $dir: skipped, $src missing"
    fi
done

# --- Uniffi-generated Kotlin source ---
GENERATED_KT="$BINDINGS_DIR/kotlin/uniffi/moq/moq.kt"
if [[ ! -f "$GENERATED_KT" ]]; then
    echo "Error: uniffi-bindgen output not found at $GENERATED_KT" >&2
    echo "       Run rs/moq-ffi/build.sh --bindings-only first." >&2
    exit 1
fi
mkdir -p "$KT_DIR/common/src/uniffi/moq"
cp "$GENERATED_KT" "$KT_DIR/common/src/uniffi/moq/moq.kt"

# --- Maven-local publish ---
MAVEN_LOCAL="$OUTPUT_DIR/maven-local"
mkdir -p "$MAVEN_LOCAL"

GRADLE_TASKS=":moq-jvm:assemble :moq-jvm:publishToMavenLocal"

# Only include the Android module if jniLibs were populated above AND
# the SDK is available. Either condition missing means the assembleRelease
# task would fail.
if find "$KT_DIR/moq-android/src/main/jniLibs" -name 'libmoq_ffi.so' -print -quit | grep -q .; then
    if [[ -n "${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}" ]] || [[ -f "$KT_DIR/local.properties" ]]; then
        GRADLE_TASKS="$GRADLE_TASKS :moq-android:assembleRelease :moq-android:publishToMavenLocal"
    fi
fi

GRADLE_CMD="${GRADLE_CMD:-$(command -v gradle || echo "$KT_DIR/gradlew")}"
"$GRADLE_CMD" -p "$KT_DIR" \
    -Pmoqffi.version="$VERSION" \
    -Dmaven.repo.local="$(cd "$MAVEN_LOCAL" && pwd)" \
    $GRADLE_TASKS

# --- Archive ---
ARCHIVE="$OUTPUT_DIR/moq-ffi-${VERSION}-kotlin.tar.gz"
tar -czf "$ARCHIVE" \
    -C "$OUTPUT_DIR" \
    "$(basename "$MAVEN_LOCAL")"
echo "Created: $ARCHIVE"
