#!/usr/bin/env bash
#
# Shared release helpers for GitHub Actions workflows.
# Usage:
#   release.sh parse-version <prefix>     — extract SemVer from GITHUB_REF given a tag prefix
#   release.sh prev-tag <prefix>          — find the tag immediately before the current one
#   release.sh create <artifacts_dir>     — create or update a GitHub release with artifacts
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
        echo "version=${version}" >> "$GITHUB_OUTPUT"
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
    prev=$(git tag --list "${prefix}-v*" --sort=v:refname \
        | awk -v cur="$current_tag" '$0 == cur { print prev; found=1; exit } { prev=$0 } END { if (!found) print "" }')

    echo "tag=${prev}" >> "$GITHUB_OUTPUT"
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

    if gh release view "$tag" > /dev/null 2>&1; then
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

# Dispatch subcommands
case "${1:-}" in
    parse-version) parse_version "$2" ;;
    prev-tag)      prev_tag "$2" ;;
    create)        create_release "$2" ;;
    *)
        echo "Usage: $0 {parse-version|prev-tag|create} <args>" >&2
        exit 1
        ;;
esac
