#!/usr/bin/env bash
set -euo pipefail

# UniFFI does not propagate Rust deprecation metadata to generated Kotlin.
# Patch both OriginProducer declarations so every call site receives the
# language-native warning.
BINDINGS="${1:?usage: patch-bindings.sh <moq.kt>}"
OUTPUT=$(mktemp)
trap 'rm -f "$OUTPUT"' EXIT

awk '
    /^public interface MoqOriginProducerInterface \{/ {
        origin = 1
    }
    /^open class MoqOriginProducer:/ {
        origin = 1
    }
    /^(public interface MoqOriginDynamicInterface \{|open class MoqOriginDynamic:|public interface MoqBroadcastRequestInterface \{|open class MoqBroadcastRequest:)/ {
        print "/** @suppress */"
        hidden_count++
    }
    origin && /fun `dynamic`\(/ {
        indent = $0
        sub(/[^ ].*$/, "", indent)
        print indent "/** @suppress */"
        print indent "@Deprecated(\"dynamic routing is not currently supported by clients\")"
        count++
        origin = 0
    }
    { print }
    END {
        if (count != 2) {
            print "expected two generated OriginProducer.dynamic declarations, found " count > "/dev/stderr"
            exit 1
        }
        if (hidden_count != 4) {
            print "expected four generated dynamic compatibility types, found " hidden_count > "/dev/stderr"
            exit 1
        }
    }
' "$BINDINGS" >"$OUTPUT"

mv "$OUTPUT" "$BINDINGS"
trap - EXIT
