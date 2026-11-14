#!/usr/bin/env bash
set -euo pipefail

# Upload GOPROXY files (<module>/@v/<version>.{info,mod,zip}) from a local
# directory to the go-moq-dev bucket, then rewrite each module's `list`.
#
# Usage: infra/go/upload.sh DIR
#
# Environment: see r2.sh.
#
# sum.golang.org pins the first hash it sees for a version, so an existing
# object is never replaced: an identical one is skipped and a different one
# fails the upload. A version is listed only once its .info, .mod, and .zip
# are all served, and the go command asks for .info first, so that goes last.

DIR="${1:?usage: upload.sh DIR}"
# shellcheck source=infra/go/r2.sh
source "$(dirname "${BASH_SOURCE[0]}")/r2.sh"

modules=$(cd "$DIR" && find . -type d -name '@v' | sed 's|^\./||; s|/@v$||' | sort)
[[ -n "$modules" ]] || {
    echo "Error: no <module>/@v directory under $DIR" >&2
    exit 1
}

for module in $modules; do
    src="$DIR/$module/@v"
    dst="$BUCKET/$module/@v"
    stray=$(find "$src" -mindepth 1 \( -type d -o \( -not -name '*.info' -not -name '*.mod' -not -name '*.zip' \) \) -print)
    if [[ -n "$stray" ]]; then
        echo "Error: unexpected files under $src:" >&2
        printf '  %s\n' "$stray" >&2
        exit 1
    fi

    rclone copy "$src" "$dst" --include '*.{mod,zip}' --immutable --checksum
    rclone copy "$src" "$dst" --include '*.info' --immutable --checksum

    # Every version whose three files are all in the bucket, this upload's and
    # earlier ones. Built locally first, so a failed listing never replaces it.
    names=$(rclone lsf --files-only "$dst")
    list=$(awk '
        /\.(info|mod|zip)$/ { v = $0; sub(/\.[a-z]+$/, "", v); n[v]++ }
        END { for (v in n) if (n[v] == 3) print v }
    ' <<<"$names" | sort -V)
    [[ -n "$list" ]] || {
        echo "Error: no complete version in $dst" >&2
        exit 1
    }
    printf '%s\n' "$list" | rclone rcat "$dst/list"
    echo "$module: $(rclone cat "$dst/list" | wc -l) versions listed"
done
