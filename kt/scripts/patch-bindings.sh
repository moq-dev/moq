#!/usr/bin/env bash
set -euo pipefail

# UniFFI does not propagate Rust deprecation metadata to generated Kotlin.
# Patch both the interface and implementation declarations so every call site
# receives the language-native warning.
BINDINGS="${1:?usage: patch-bindings.sh <moq.kt>}"
OUTPUT=$(mktemp)
trap 'rm -f "$OUTPUT"' EXIT

awk '
    /fun dynamic\(/ {
        indent = $0
        sub(/[^ ].*$/, "", indent)
        print indent "@Deprecated(\"dynamic routing is not currently supported by clients\")"
        count++
    }
    { print }
    END {
        if (count != 2) {
            print "expected two generated dynamic declarations, found " count > "/dev/stderr"
            exit 1
        }
    }
' "$BINDINGS" >"$OUTPUT"

mv "$OUTPUT" "$BINDINGS"
trap - EXIT
