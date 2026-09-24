#!/usr/bin/env bash
# Validate that every draft parses and that its generated XML passes validation.
#
# Both halves are needed. kramdown-rfc catches markdown and frontmatter errors,
# but happily emits XML for a cross-reference that points at no such section;
# xml2rfc is what rejects the dangling IDREF. Checking only the first half lets
# a broken `[text](#anchor)` pass here and then fail at `publish` time.
#
# --preptool stops after validation instead of rendering txt/html, and -N keeps
# it off the network (kramdown-rfc already inlined the references), which is
# what makes this a couple of seconds per draft rather than a minute.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
bun install --frozen-lockfile
cd drafts

for f in draft-*.md; do
    echo "checking $f"
    kramdown-rfc --v3 <"$f" >"${f%.md}.xml"
    xml2rfc -q --allow-local-file-access --preptool -N "${f%.md}.xml" -o "${f%.md}.prepped.xml" || {
        rm -f "${f%.md}.prepped.xml"
        exit 1
    }
    rm -f "${f%.md}.prepped.xml"
done

for f in moq-e2ee-*.ts; do
    bun tsc --noEmit --skipLibCheck --target esnext --module esnext --moduleResolution bundler --types bun "$f"
    bun "$f"
done
