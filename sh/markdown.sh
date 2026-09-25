#!/usr/bin/env bash
# Lint or format every Markdown file: sh/markdown.sh check|fix
#
# remark-cli has no check mode: `--frail` raises the exit code on lint
# messages, and only `--output` formats, so a file that is merely misformatted
# passes both ways. `check` formats a scratch mirror and diffs it, so it stays
# read-only while `fix` writes in place.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

case "${1:-}" in
    check) ;;
    fix)
        bun remark . --quiet --output
        exit 0
        ;;
    *)
        echo "usage: sh/markdown.sh check|fix" >&2
        exit 2
        ;;
esac

mirror=$(mktemp -d)
trap 'rm -rf "$mirror"' EXIT

extension_list=$(bun -e \
    'import extensions from "markdown-extensions"; console.log(extensions.join("\n"))')
patterns=()
while IFS= read -r extension; do
    [[ -n "$extension" ]] && patterns+=("*.$extension")
done <<<"$extension_list"
((${#patterns[@]})) || {
    echo "error: remark reported no Markdown extensions" >&2
    exit 1
}

# Untracked files are in scope so a new doc is linted before it is staged;
# --exclude-standard keeps build output out.
files=()
while IFS= read -r -d '' file; do
    [[ -f "$file" ]] || continue
    files+=("$file")
    mkdir -p "$mirror/$(dirname "$file")"
    cp "$file" "$mirror/$file"
done < <(git ls-files -z --cached --others --exclude-standard -- "${patterns[@]}")
((${#files[@]})) || exit 0

# The config rides along so every .remarkignore pattern resolves against the
# mirror root exactly as it does here, and node_modules is where the plugins
# named by .remarkrc.mjs come from.
cp .remarkrc.mjs .remarkignore "$mirror/"
ln -s "$PWD/node_modules" "$mirror/node_modules"

status=0
(cd "$mirror" && bun remark . --quiet --frail --output) || status=$?

stale=()
for file in "${files[@]}"; do
    cmp -s "$file" "$mirror/$file" || stale+=("$file")
done

if ((${#stale[@]})); then
    echo "error: these files are not formatted, run 'just fix':" >&2
    printf '       %s\n' "${stale[@]}" >&2
    status=1
fi

exit "$status"
