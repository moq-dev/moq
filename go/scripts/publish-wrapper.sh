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
# One exception, since "identical tree" and "already released" are not the same
# thing: a mirror HEAD carrying no tag on this line is a half-finished release,
# and it gets tagged rather than skipped. See the recovery below.
#
# Required environment:
#   GO_MIRROR_TOKEN  - PAT or GitHub App token with contents:write on $MIRROR_REPO
#
# Optional environment:
#   GO_MIRROR_REPO   - defaults to moq-dev/moq-go
#   GO_MIRROR_URL    - clone/push remote, defaults to the GitHub URL for
#                      $MIRROR_REPO. Tests point it at a local bare repo.
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
if [[ -n "${GO_MIRROR_URL:-}" ]]; then
    CLONE_URL="$GO_MIRROR_URL"
elif [[ -n "${GO_MIRROR_TOKEN:-}" ]]; then
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

# --- 4. Derive the next patch on this line from the mirror's tags ---
# The registry, not the staged tree, decides the patch number, so the release
# path and the recovery path below ask the same question the same way.
next_release_tag() {
    local max=-1 ref patch
    while read -r ref; do
        [[ -z "$ref" ]] && continue
        patch="${ref##*.}"
        if [[ "$patch" =~ ^[0-9]+$ ]] && ((patch > max)); then
            max="$patch"
        fi
    done < <(git -C "$WORK/mirror" ls-remote --tags origin "refs/tags/v${LINE}.*" |
        sed -n "s#.*refs/tags/\(v${LINE}\.[0-9][0-9]*\)\$#\1#p")

    echo "v${LINE}.$((max + 1))"
}

# --- 5. Identical tree usually means there is nothing to release ---
if git -C "$WORK/mirror" diff --cached --quiet; then
    # Usually, but not always: an untagged HEAD is a half-finished release, not a
    # finished one. Publishing pushes the branch and the tag, and the patch
    # number is only derived when there is a diff, so a tag that never landed
    # used to strand that content forever -- every retry matched HEAD and
    # stopped here without ever asking which tag was missing. Tag what is
    # already there instead.
    HEAD_SHA=$(git -C "$WORK/mirror" rev-parse HEAD)
    if git -C "$WORK/mirror" ls-remote --tags origin "refs/tags/v${LINE}.*" |
        grep -q "^${HEAD_SHA}[[:space:]]"; then
        echo "Staged wrapper tree is identical to ${MIRROR_REPO} HEAD. Nothing to publish."
        exit 0
    fi

    MIRROR_TAG=$(next_release_tag)
    echo "::warning::${MIRROR_REPO} HEAD matches the staged tree but carries no v${LINE}.* tag;"
    echo "::warning::a previous run pushed the tree without its tag. Tagging HEAD as ${MIRROR_TAG}."

    if [[ "$DRY_RUN" == true ]]; then
        echo "Dry-run: not tagging or pushing."
        exit 0
    fi

    git -C "$WORK/mirror" tag "${MIRROR_TAG}"
    git -C "$WORK/mirror" push origin "refs/tags/${MIRROR_TAG}"
    echo "Published ${MIRROR_REPO}@${MIRROR_TAG}"
    exit 0
fi

MIRROR_TAG=$(next_release_tag)
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

# One push, all or nothing. Landing the branch without its tag publishes content
# that no version resolves to, and the recovery above only exists because that
# window used to be two separate pushes wide.
git -C "$WORK/mirror" push --atomic origin "HEAD:refs/heads/main" "refs/tags/${MIRROR_TAG}"

echo "Published ${MIRROR_REPO}@${MIRROR_TAG}"
