#!/usr/bin/env bash
#
# Shared release helpers for GitHub Actions workflows.
# Usage:
#   release.sh parse-version <prefix>          — extract SemVer from GITHUB_REF given a tag prefix
#   release.sh prev-tag <prefix>               — find the tag immediately before the current one
#   release.sh create <artifacts_dir>          — create or update a GitHub release with artifacts
#   release.sh git-tag-exists <repo> <tag>     — check whether a tag exists on a remote repo
#   release.sh read-version <pyproject.toml>   — read `version = "x.y.z"` from a manifest
#   release.sh pypi-exists <dist> <version>    — check whether <dist>==<version> is already on PyPI
#   release.sh maven-exists <group> <artifact> <version>
#   release.sh maven-version-in-range <group> <artifact> <range>
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

maven_path() {
    local group="$1"
    local artifact="$2"
    echo "${group//.//}/${artifact}"
}

maven_status() {
    local url="$1"
    local output="${2:-/dev/null}"

    curl -s -o "$output" -w '%{http_code}' --max-time 10 --retry 3 --retry-connrefused "$url" 2>/dev/null || true
}

maven_emit_exists() {
    local exists="$1"
    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        echo "exists=${exists}" >>"$GITHUB_OUTPUT"
    fi
}

# Check whether a Maven Central artifact version exists. Writes
# exists=true|false to $GITHUB_OUTPUT. A 404 means unpublished; any other
# non-200 status is fatal so an outage does not look like a fresh version.
maven_exists() {
    local group="$1"
    local artifact="$2"
    local version="$3"
    local path
    path=$(maven_path "$group" "$artifact")
    local url="https://repo1.maven.org/maven2/${path}/${version}/${artifact}-${version}.pom"

    local code
    code=$(maven_status "$url")

    local exists
    case "$code" in
        200) exists=true ;;
        404) exists=false ;;
        *)
            echo "Unexpected status $code querying $url" >&2
            exit 1
            ;;
    esac

    maven_emit_exists "$exists"
    echo "Maven ${group}:${artifact}:${version}: exists=${exists}"
}

semver_key() {
    local version="${1%%[-+]*}"
    local major minor patch
    IFS=. read -r major minor patch <<<"$version"
    printf '%06d%06d%06d\n' "${major:-0}" "${minor:-0}" "${patch:-0}"
}

semver_ge() {
    [[ "$(semver_key "$1")" > "$(semver_key "$2")" || "$(semver_key "$1")" == "$(semver_key "$2")" ]]
}

semver_gt() {
    [[ "$(semver_key "$1")" > "$(semver_key "$2")" ]]
}

semver_le() {
    [[ "$(semver_key "$1")" < "$(semver_key "$2")" || "$(semver_key "$1")" == "$(semver_key "$2")" ]]
}

semver_lt() {
    [[ "$(semver_key "$1")" < "$(semver_key "$2")" ]]
}

version_in_range() {
    local version="$1"
    local range="$2"

    if [[ ! "$range" =~ ^([\[\(])([^,]+),([^\]\)]+)([\]\)])$ ]]; then
        echo "Unsupported Maven version range: $range" >&2
        exit 1
    fi

    local lower_bound="${BASH_REMATCH[1]}"
    local lower="${BASH_REMATCH[2]}"
    local upper="${BASH_REMATCH[3]}"
    local upper_bound="${BASH_REMATCH[4]}"

    if [[ "$lower_bound" == "[" ]]; then
        semver_ge "$version" "$lower" || return 1
    else
        semver_gt "$version" "$lower" || return 1
    fi

    if [[ "$upper_bound" == "]" ]]; then
        semver_le "$version" "$upper" || return 1
    else
        semver_lt "$version" "$upper" || return 1
    fi
}

# Check whether Maven Central metadata contains any version in a Gradle-style
# half-open range such as [0.3,0.4). Writes exists=true|false to $GITHUB_OUTPUT.
maven_version_in_range() {
    local group="$1"
    local artifact="$2"
    local range="$3"
    local path
    path=$(maven_path "$group" "$artifact")
    local url="https://repo1.maven.org/maven2/${path}/maven-metadata.xml"

    local body
    body=$(mktemp)

    local code
    code=$(maven_status "$url" "$body")

    case "$code" in
        200) ;;
        404)
            maven_emit_exists false
            echo "Maven ${group}:${artifact} has version in ${range}: exists=false"
            rm -f "$body"
            return
            ;;
        *)
            echo "Unexpected status $code querying $url" >&2
            exit 1
            ;;
    esac

    local exists=false
    local version
    while IFS= read -r version; do
        if version_in_range "$version" "$range"; then
            exists=true
            break
        fi
    done < <(grep -oE '<version>[^<]+</version>' "$body" | sed 's#</\?version>##g')

    rm -f "$body"
    maven_emit_exists "$exists"
    echo "Maven ${group}:${artifact} has version in ${range}: exists=${exists}"
}

# Dispatch subcommands
case "${1:-}" in
    parse-version) parse_version "$2" ;;
    prev-tag) prev_tag "$2" ;;
    create) create_release "$2" ;;
    git-tag-exists) git_tag_exists "$2" "$3" ;;
    read-version) read_version "$2" ;;
    pypi-exists) pypi_exists "$2" "$3" ;;
    maven-exists) maven_exists "$2" "$3" "$4" ;;
    maven-version-in-range) maven_version_in_range "$2" "$3" "$4" ;;
    *)
        echo "Usage: $0 {parse-version|prev-tag|create|git-tag-exists|read-version|pypi-exists|maven-exists|maven-version-in-range} <args>" >&2
        exit 1
        ;;
esac
