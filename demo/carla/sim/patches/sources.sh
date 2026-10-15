#!/usr/bin/env bash
# Put a checkout (CARLA or the engine) into the build's source state: its
# pinned commit plus the given patches, overlays and scripted edits, and
# nothing else. A cached checkout can carry other edits at the same commit
# (an experiment, a hand edit), and those would ship inside an
# image labelled as this build, so they are refused rather than compiled.
#
#   sources.sh <checkout> [--scripted <file>]... [--overlay <dir> <dest>]... <patch>...
#
# --scripted: a file the build rewrites itself later, reset here first.
# --overlay:  a directory of this repository kept identical at <dest>, which the
#             checkout does not track.
set -euo pipefail

usage="usage: sources.sh <checkout> [--scripted <file>]... [--overlay <dir> <dest>]... <patch>..."
checkout="${1:?$usage}"
shift
scripted=()
overlay_dirs=()
overlay_dests=()
patches=()
while (($#)); do
    case "$1" in
        --scripted)
            scripted+=("${2:?$usage}")
            shift 2
            ;;
        --overlay)
            overlay_dirs+=("$(cd "${2:?$usage}" && pwd)")
            overlay_dests+=("${3:?$usage}")
            shift 3
            ;;
        *)
            patches+=("$(cd "$(dirname "$1")" && pwd)/$(basename "$1")")
            shift
            ;;
    esac
done
if ((${#patches[@]} == 0)); then
    echo "$usage" >&2
    exit 1
fi

patched=()
for patch in "${patches[@]}"; do
    while read -r _ _ file; do
        patched+=("$file")
    done < <(git -C "$checkout" apply --numstat "$patch")
done

# Unreal Build Tool compiles every source file under a module, tracked or not,
# so untracked sources count as edits too, except inside an overlay.
under_overlay() {
    local dest
    for dest in "${overlay_dests[@]}"; do
        [[ "$1" == "$dest"/* ]] && return 0
    done
    return 1
}
unexpected=$({
    git -C "$checkout" diff --name-only HEAD |
        grep -vxF -f <(printf '%s\n' "${patched[@]}" "${scripted[@]}") || true
    git -C "$checkout" ls-files --others --exclude-standard |
        { grep -E '\.(c|cc|cpp|cxx|h|hh|hpp|hxx|inl|ipp|cs)$' || true; } |
        while read -r file; do under_overlay "$file" || echo "$file"; done
})
if [[ -n "$unexpected" ]]; then
    printf 'sources: %s has edits this build does not make:\n%s\n' "$checkout" "$unexpected" >&2
    exit 1
fi

if ((${#scripted[@]})); then
    git -C "$checkout" checkout -q HEAD -- "${scripted[@]}"
fi

# Each patch applies once, onto files it finds pristine, so a checkout cached
# by an earlier build with fewer patches takes the new ones.
for patch in "${patches[@]}"; do
    mapfile -t files < <(git -C "$checkout" apply --numstat "$patch" | cut -f3)
    if ((${#files[@]} == 0)); then
        echo "sources: $patch changes no file" >&2
        exit 1
    fi
    if git -C "$checkout" diff --quiet HEAD -- "${files[@]}"; then
        git -C "$checkout" apply "$patch"
    fi
done
# The patched files must be exactly the commit plus the patches: rebuild that
# from the commit and compare, which also refuses a hand edit on top.
expected=$(mktemp -d)
trap 'rm -rf "$expected"' EXIT
# Its own repository, so `git apply` works relative to it even when the
# temporary directory sits inside another checkout.
git init -q "$expected"
for file in "${patched[@]}"; do
    mkdir -p "$expected/$(dirname "$file")"
    git -C "$checkout" show "HEAD:$file" >"$expected/$file"
done
(cd "$expected" && for patch in "${patches[@]}"; do git apply "$patch"; done)
for file in "${patched[@]}"; do
    if ! cmp -s "$expected/$file" "$checkout/$file"; then
        echo "sources: $checkout/$file differs from its patch" >&2
        exit 1
    fi
done

# Overlays are synced by content, so an unchanged file keeps its timestamp and
# Unreal does not rebuild it.
for index in "${!overlay_dirs[@]}"; do
    dir="${overlay_dirs[$index]}"
    dest="$checkout/${overlay_dests[$index]}"
    if git -C "$checkout" ls-files --error-unmatch "${overlay_dests[$index]}" >/dev/null 2>&1; then
        echo "sources: overlay destination ${overlay_dests[$index]} is tracked by $checkout" >&2
        exit 1
    fi
    mkdir -p "$dest"
    while IFS= read -r -d '' file; do
        mkdir -p "$dest/$(dirname "$file")"
        cmp -s "$dir/$file" "$dest/$file" || cp "$dir/$file" "$dest/$file"
    done < <(cd "$dir" && find . -type f -print0)
    while IFS= read -r -d '' file; do
        [[ -e "$dir/$file" ]] || rm "$dest/$file"
    done < <(cd "$dest" && find . -type f -print0)
done
