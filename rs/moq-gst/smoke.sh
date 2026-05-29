#!/usr/bin/env bash
#
# Load the moq-gst plugin against the host's system GStreamer and
# assert moqsink + moqsrc are exposed. Used by build.sh after
# scrub.sh and by the moq-gst.yml workflow after extracting the
# release tarball, so the same check guards local dev and CI.
#
# Usage:
#   smoke.sh <plugin-dir>
#
# The plugin dir must contain libgstmoq.{so,dylib}. If
# gst-inspect-1.0 is not on PATH the script skips with a warning so
# contributors without GStreamer installed can still produce
# tarballs; CI installs it explicitly.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <plugin-dir>" >&2
    exit 2
fi

plugin_dir="$1"
if [[ ! -d "$plugin_dir" ]]; then
    echo "Error: $plugin_dir is not a directory" >&2
    exit 2
fi

if ! command -v gst-inspect-1.0 >/dev/null 2>&1; then
    echo "Warning: gst-inspect-1.0 not on PATH; skipping load test." >&2
    exit 0
fi

echo "Smoke testing against $(gst-inspect-1.0 --version | head -1)..."

# Isolate discovery to $plugin_dir:
#   - GST_PLUGIN_PATH_1_0 alone is additive, so a system-installed moq
#     plugin could shadow the one we want to validate.
#   - GST_PLUGIN_SYSTEM_PATH_1_0="" suppresses the default system scan.
#   - GST_REGISTRY_1_0 points at a temp file so we don't read or pollute
#     the user's cached registry.
tmp_registry="$(mktemp)"
trap 'rm -f "$tmp_registry"' EXIT

# gst-inspect-1.0 moq exits 0 even when the .so fails to load (it
# treats "no such plugin" as a non-error), so grep for the factory
# names. The plugin lists each factory as "  name: description".
out="$(
    GST_PLUGIN_PATH_1_0="$plugin_dir" \
        GST_PLUGIN_SYSTEM_PATH_1_0="" \
        GST_REGISTRY_1_0="$tmp_registry" \
        gst-inspect-1.0 moq
)"
if ! echo "$out" | grep -qE '^[[:space:]]+moqsink:' ||
    ! echo "$out" | grep -qE '^[[:space:]]+moqsrc:'; then
    echo "Error: gst-inspect-1.0 didn't find moqsink/moqsrc in the plugin." >&2
    echo "$out" >&2
    exit 1
fi

echo "Smoke test passed: moqsink + moqsrc loaded."
