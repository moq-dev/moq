#!/usr/bin/env bash
# Render drafts to <name>.txt and <name>.html: sh/drafts/build.sh [NAME...]
#
# The local editor's copy (gitignored), with the -latest docname as-is; use
# `just drafts publish` for a version. No NAME renders every draft.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)/drafts"

names=("$@")
if ((${#names[@]} == 0)); then
    for f in draft-*.md; do names+=("${f%.md}"); done
fi

for name in "${names[@]}"; do
    kramdown-rfc --v3 <"$name.md" >"$name.xml"
    xml2rfc -q --allow-local-file-access --text "$name.xml" -o "$name.txt"
    xml2rfc -q --allow-local-file-access --html "$name.xml" -o "$name.html"
    echo "built $name.txt and $name.html"
done
