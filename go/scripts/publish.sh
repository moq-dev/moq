#!/usr/bin/env bash
set -euo pipefail

# Push the staged Go module contents to the moq-dev/moq-go mirror repo
# on a bare-semver tag (e.g. v0.2.11). Go modules resolve from VCS tags,
# and the Go module proxy only recognizes `v<semver>` tags, not the
# prefixed `moq-ffi-v*` tags used in this monorepo.
#
# Required environment:
#   BUILD_VERSION    - version string (e.g. 0.2.11)
#   GO_MIRROR_TOKEN  - PAT or GitHub App token with contents:write
#                      on $MIRROR_REPO
#
# Optional environment:
#   GO_MIRROR_REPO   - defaults to moq-dev/moq-go
#   GIT_AUTHOR_NAME  - defaults to "moq-go-release"
#   GIT_AUTHOR_EMAIL - defaults to "release@moq.dev"
#
# Flags:
#   --dry-run        Stage and diff against the mirror but skip the
#                    commit, tag, and push. Useful for verifying the
#                    pipeline locally without touching the mirror.
#
# Expects the staged Go module tarball under `go-out/`, produced by
# package.sh as `moq-ffi-${BUILD_VERSION}-go.tar.gz`.

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

: "${BUILD_VERSION:?BUILD_VERSION is required}"
if [[ "$DRY_RUN" != true ]]; then
    : "${GO_MIRROR_TOKEN:?GO_MIRROR_TOKEN is required (or pass --dry-run)}"
fi

MIRROR_REPO="${GO_MIRROR_REPO:-moq-dev/moq-go}"
# Go modules resolve bare v<semver> tags; the moq-ffi-v* prefix used in
# this monorepo would be invisible to the proxy.
MIRROR_TAG="v${BUILD_VERSION}"
SOURCE_TAG="moq-ffi-v${BUILD_VERSION}"

TARBALL="go-out/moq-ffi-${BUILD_VERSION}-go.tar.gz"
[[ -f "$TARBALL" ]] || {
    echo "Error: missing $TARBALL" >&2
    exit 1
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# --- 1. Clone the mirror ---
if [[ -n "${GO_MIRROR_TOKEN:-}" ]]; then
    CLONE_URL="https://x-access-token:${GO_MIRROR_TOKEN}@github.com/${MIRROR_REPO}"
else
    CLONE_URL="https://github.com/${MIRROR_REPO}"
fi
git clone --depth 1 "$CLONE_URL" "$WORK/mirror" 2>&1 | sed "s|${GO_MIRROR_TOKEN:-__no_token__}|***|g"

# --- 2. Idempotency: skip if the mirror tag already exists ---
if [[ -n "$(git -C "$WORK/mirror" ls-remote --tags origin "refs/tags/${MIRROR_TAG}")" ]]; then
    echo "Mirror tag ${MIRROR_TAG} already exists on ${MIRROR_REPO}. Nothing to publish."
    exit 0
fi

# --- 3. Extract staged package ---
tar -xzf "$TARBALL" -C "$WORK"
STAGED="$WORK/moq-ffi-${BUILD_VERSION}-go"
[[ -d "$STAGED" ]] || {
    echo "Error: tarball did not contain $STAGED" >&2
    exit 1
}

# --- 4. Replace mirror tree with staged contents (preserving .git) ---
rsync --archive --delete --exclude='.git' "$STAGED/" "$WORK/mirror/"

# --- 5. Summary diff (always shown, helpful for dry-runs and audit logs) ---
echo "--- diff against ${MIRROR_REPO} HEAD ---"
git -C "$WORK/mirror" add -A
git -C "$WORK/mirror" diff --cached --stat
echo "---"

# --- 6. Commit / tag / push (skipped in dry-run) ---
if [[ "$DRY_RUN" == true ]]; then
    echo "Dry-run: not committing or pushing."
    exit 0
fi

if git -C "$WORK/mirror" diff --cached --quiet; then
    echo "No changes to publish to ${MIRROR_REPO}. (Tag ${MIRROR_TAG} would be a no-op commit.)"
    git -C "$WORK/mirror" tag "${MIRROR_TAG}"
    git -C "$WORK/mirror" push origin "refs/tags/${MIRROR_TAG}"
    exit 0
fi

git -C "$WORK/mirror" config user.name "${GIT_AUTHOR_NAME:-moq-go-release}"
git -C "$WORK/mirror" config user.email "${GIT_AUTHOR_EMAIL:-release@moq.dev}"

git -C "$WORK/mirror" commit -m "Release ${MIRROR_TAG} (mirrors ${SOURCE_TAG})"
git -C "$WORK/mirror" tag "${MIRROR_TAG}"
git -C "$WORK/mirror" push origin "HEAD:refs/heads/main"
git -C "$WORK/mirror" push origin "refs/tags/${MIRROR_TAG}"

echo "Published ${MIRROR_REPO}@${MIRROR_TAG}"
