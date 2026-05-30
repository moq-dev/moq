#!/usr/bin/env bash
#
# Shared release helpers for GitHub Actions workflows.
# Usage:
#   release.sh parse-version <prefix>          — extract SemVer from GITHUB_REF given a tag prefix
#   release.sh prev-tag <prefix>               — find the tag immediately before the current one
#   release.sh create <artifacts_dir>          — create or update a GitHub release with artifacts
#   release.sh read-version <pyproject.toml>   — read `version = "x.y.z"` from a manifest
#   release.sh pypi-exists <dist> <version>    — check whether <dist>==<version> is already on PyPI
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

# Read the static `version = "x.y.z"` from the [project] table of a
# pyproject.toml. Scoped to [project] so a `version` key in another table
# (e.g. a [tool.*] section) can't be picked up by mistake. Writes
# version=<ver> to $GITHUB_OUTPUT and stdout.
read_version() {
    local manifest="$1"
    local version
    version=$(sed -n '/^\[project\]/,/^\[/{
        s/^[[:space:]]*version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p
    }' "$manifest" | head -n1)
    if [[ -z "$version" ]]; then
        echo "Could not read version from [project] in $manifest" >&2
        exit 1
    fi
    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        echo "version=${version}" >>"$GITHUB_OUTPUT"
    fi
    echo "$version"
}

# Check whether a distribution+version is already published on PyPI. This is the
# release gate (like release-plz checking the registry): the git tag is just a
# record, the registry is the source of truth. Writes exists=true|false to
# $GITHUB_OUTPUT. A non-200/404 response is treated as fatal rather than
# silently re-publishing.
pypi_exists() {
    local dist="$1"
    local version="$2"
    local url="https://pypi.org/pypi/${dist}/${version}/json"

    # Retry transient failures so a network blip doesn't fail the release gate.
    local code
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 --retry 3 --retry-connrefused "$url" 2>/dev/null || true)

    local exists
    case "$code" in
        200) exists=true ;;
        404) exists=false ;;
        *)
            echo "Unexpected status $code querying $url" >&2
            exit 1
            ;;
    esac

    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        echo "exists=${exists}" >>"$GITHUB_OUTPUT"
    fi
    echo "PyPI ${dist}==${version}: exists=${exists}"
}

# Dispatch subcommands
case "${1:-}" in
    parse-version) parse_version "$2" ;;
    prev-tag) prev_tag "$2" ;;
    create) create_release "$2" ;;
    read-version) read_version "$2" ;;
    pypi-exists) pypi_exists "$2" "$3" ;;
    *)
        echo "Usage: $0 {parse-version|prev-tag|create|read-version|pypi-exists} <args>" >&2
        exit 1
        ;;
esac
