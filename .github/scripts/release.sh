#!/usr/bin/env bash
#
# Shared release helpers for GitHub Actions workflows.
# Usage:
#   release.sh parse-version <prefix>     — extract SemVer from GITHUB_REF given a tag prefix
#   release.sh prev-tag <prefix>          — find the tag immediately before the current one
#   release.sh create <artifacts_dir>     — create or update a GitHub release with artifacts
#   release.sh git-tag-exists <repo> <tag> — check whether a tag exists on a remote repo
#
# Environment:
#   GITHUB_REF        — set by GitHub Actions (e.g. refs/tags/moq-relay-v1.2.3)
#   GITHUB_OUTPUT     — set by GitHub Actions (for writing step outputs)
#   GH_TOKEN          — required for `create` subcommand

set -euo pipefail

# Parse a SemVer version from GITHUB_REF given a tag prefix.
# Writes version=<ver> to $GITHUB_OUTPUT.
parse_version() {
    local prefix="$1"
    local ref="${GITHUB_REF#refs/tags/}"

    if [[ "$ref" =~ ^${prefix}-v([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?)$ ]]; then
        local version="${BASH_REMATCH[1]}"
        echo "version=${version}" >>"$GITHUB_OUTPUT"
        echo "Parsed version: ${version}"
    else
        echo "Tag format not recognized: $ref (expected ${prefix}-v<semver>)" >&2
        exit 1
    fi
}

# Find the tag immediately before the current one (by version sort order).
# Writes tag=<prev> to $GITHUB_OUTPUT.
prev_tag() {
    local prefix="$1"
    local current_tag="${GITHUB_REF#refs/tags/}"

    local prev
    prev=$(git tag --list "${prefix}-v*" --sort=v:refname |
        awk -v cur="$current_tag" '$0 == cur { print prev; found=1; exit } { prev=$0 } END { if (!found) print "" }')

    echo "tag=${prev}" >>"$GITHUB_OUTPUT"
    echo "Previous tag: ${prev:-none}"
}

# Create or update a GitHub release with artifacts.
# Args: <artifacts_dir>
# Reads tag/title/prev_tag from environment or step outputs.
create_release() {
    local artifacts_dir="$1"
    local tag="${RELEASE_TAG:?RELEASE_TAG must be set}"
    local title="${RELEASE_TITLE:?RELEASE_TITLE must be set}"
    local prev_tag="${RELEASE_PREV_TAG:-}"

    if gh release view "$tag" >/dev/null 2>&1; then
        echo "Release exists, updating assets and metadata..."
        gh release upload "$tag" "$artifacts_dir"/* --clobber
        if [ -n "$prev_tag" ]; then
            gh release edit "$tag" --title "$title" --notes-start-tag "$prev_tag"
        else
            gh release edit "$tag" --title "$title"
        fi
    else
        echo "Creating new release..."
        if [ -n "$prev_tag" ]; then
            gh release create "$tag" \
                --title "$title" \
                --generate-notes \
                --notes-start-tag "$prev_tag" \
                "$artifacts_dir"/*
        else
            gh release create "$tag" \
                --title "$title" \
                --generate-notes \
                "$artifacts_dir"/*
        fi
    fi
}

# Check whether a bare-semver tag already exists on a remote repo. This is the
# release gate for the independently-versioned Swift wrapper (like release-plz
# checking the registry): the mirror's git tag is the source of truth, so a
# version that's already published is a no-op. Writes exists=true|false to
# $GITHUB_OUTPUT. A connection failure is fatal rather than silently
# re-publishing.
git_tag_exists() {
    local repo="$1"
    local tag="$2"
    local url="https://github.com/${repo}"

    local out
    out=$(git ls-remote --tags "$url" "refs/tags/${tag}") || {
        echo "Failed to query tags on $url" >&2
        exit 1
    }

    local exists
    if [[ -n "$out" ]]; then exists=true; else exists=false; fi

    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        echo "exists=${exists}" >>"$GITHUB_OUTPUT"
    fi
    echo "Tag ${repo}@${tag}: exists=${exists}"
}

# Dispatch subcommands
case "${1:-}" in
    parse-version) parse_version "$2" ;;
    prev-tag) prev_tag "$2" ;;
    create) create_release "$2" ;;
    git-tag-exists) git_tag_exists "$2" "$3" ;;
    *)
        echo "Usage: $0 {parse-version|prev-tag|create|git-tag-exists} <args>" >&2
        exit 1
        ;;
esac
