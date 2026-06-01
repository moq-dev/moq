#!/usr/bin/env bash
set -euo pipefail

# Push the staged moq-go wrapper module to the moq-dev/moq-go mirror repo on a
# bare-semver tag (e.g. v0.3.4). The wrapper is versioned independently of the
# ffi crate:
#
#   * MAJOR.MINOR comes from the staged VERSION file (human-owned API line).
#   * PATCH is derived here from the mirror's existing v<line>.* tags
#     (highest + 1, or .0 for a fresh line). The registry is the gate, like
#     release-plz and the PyPI check in release.sh.
#
# Idempotency: the staged tree is patch-independent (see package-wrapper.sh), so
# if it matches the mirror HEAD we publish NOTHING: no commit, no tag, no push.
# That keeps an ffi tag that didn't actually move the ffi version (or any other
# no-op trigger) from minting an empty patch release. The patch is computed only
# after a real diff is confirmed, so no-ops never consume a patch number.
#
# Required environment:
#   GO_MIRROR_TOKEN  - PAT or GitHub App token with contents:write on $MIRROR_REPO
#
# Optional environment:
#   GO_MIRROR_REPO   - defaults to moq-dev/moq-go
#   GIT_AUTHOR_NAME  - defaults to "moq-go-release"
#   GIT_AUTHOR_EMAIL - defaults to "release@moq.dev"
#
# Flags:
#   --dry-run        Stage and diff against the mirror but skip the commit, tag,
#                    and push.
#
# Expects the staged wrapper tarball under `go-out/` as
# `moq-go-<line>-wrapper.tar.gz`, produced by package-wrapper.sh.

DRY_RUN=false
while [[ $# -gt 0 ]]; do
    case $1 in
        --dry-run)
            DRY_RUN=true
            shift
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

if [[ "$DRY_RUN" != true ]]; then
    : "${GO_MIRROR_TOKEN:?GO_MIRROR_TOKEN is required (or pass --dry-run)}"
fi

MIRROR_REPO="${GO_MIRROR_REPO:-moq-dev/moq-go}"

# Locate the staged tarball (one per line).
shopt -s nullglob
TARBALLS=(go-out/moq-go-*-wrapper.tar.gz)
shopt -u nullglob
[[ ${#TARBALLS[@]} -eq 1 ]] || {
    echo "Error: expected exactly one go-out/moq-go-*-wrapper.tar.gz, found ${#TARBALLS[@]}" >&2
    exit 1
}
TARBALL="${TARBALLS[0]}"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# --- 1. Extract staged package and read the MAJOR.MINOR line ---
tar -xzf "$TARBALL" -C "$WORK"
STAGED=$(echo "$WORK"/moq-go-*-wrapper)
[[ -d "$STAGED" ]] || {
    echo "Error: tarball did not contain a staged wrapper dir" >&2
    exit 1
}
LINE=$(tr -d '[:space:]' <"$STAGED/VERSION")
[[ "$LINE" =~ ^[0-9]+\.[0-9]+$ ]] || {
    echo "Error: VERSION must be MAJOR.MINOR, got '$LINE'" >&2
    exit 1
}

# --- 2. Clone the mirror ---
if [[ -n "${GO_MIRROR_TOKEN:-}" ]]; then
    CLONE_URL="https://x-access-token:${GO_MIRROR_TOKEN}@github.com/${MIRROR_REPO}"
else
    CLONE_URL="https://github.com/${MIRROR_REPO}"
fi
git clone --depth 1 "$CLONE_URL" "$WORK/mirror" 2>&1 | sed "s|${GO_MIRROR_TOKEN:-__no_token__}|***|g"

# --- 3. Replace mirror tree with staged contents (preserving .git) ---
rsync --archive --delete --exclude='.git' "$STAGED/" "$WORK/mirror/"
git -C "$WORK/mirror" add -A

echo "--- diff against ${MIRROR_REPO} HEAD ---"
git -C "$WORK/mirror" diff --cached --stat
echo "---"

# --- 4. Idempotency: identical tree means there is nothing to release ---
if git -C "$WORK/mirror" diff --cached --quiet; then
    echo "Staged wrapper tree is identical to ${MIRROR_REPO} HEAD. Nothing to publish."
    exit 0
fi

# --- 5. Derive the next patch on this line from the mirror's tags ---
# (Only now that we know the tree actually changed, so no-ops don't burn a patch.)
MAX_PATCH=-1
while read -r ref; do
    [[ -z "$ref" ]] && continue
    patch="${ref##*.}"
    if [[ "$patch" =~ ^[0-9]+$ ]] && ((patch > MAX_PATCH)); then
        MAX_PATCH="$patch"
    fi
done < <(git -C "$WORK/mirror" ls-remote --tags origin "refs/tags/v${LINE}.*" |
    sed -n "s#.*refs/tags/\(v${LINE}\.[0-9][0-9]*\)\$#\1#p")

VERSION="${LINE}.$((MAX_PATCH + 1))"
MIRROR_TAG="v${VERSION}"
echo "Next ${MIRROR_REPO} release on line ${LINE}: ${MIRROR_TAG}"

# --- 6. Commit / tag / push (skipped in dry-run) ---
if [[ "$DRY_RUN" == true ]]; then
    echo "Dry-run: not committing or pushing."
    exit 0
fi

git -C "$WORK/mirror" config user.name "${GIT_AUTHOR_NAME:-moq-go-release}"
git -C "$WORK/mirror" config user.email "${GIT_AUTHOR_EMAIL:-release@moq.dev}"

git -C "$WORK/mirror" commit -m "Release ${MIRROR_TAG}"
git -C "$WORK/mirror" tag "${MIRROR_TAG}"
git -C "$WORK/mirror" push origin "HEAD:refs/heads/main"
git -C "$WORK/mirror" push origin "refs/tags/${MIRROR_TAG}"

echo "Published ${MIRROR_REPO}@${MIRROR_TAG}"
