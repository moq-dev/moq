#!/usr/bin/env bash
# Print the newest stable MoQ release with the requested tag prefix as JSON.
set -euo pipefail

case "${1:-}" in
    libmoq-v | moq-gst-v) prefix=$1 ;;
    *)
        echo "usage: $0 {libmoq-v|moq-gst-v}" >&2
        exit 2
        ;;
esac
[[ $# -eq 1 ]] || {
    echo "expected one release prefix" >&2
    exit 2
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
hdr=()
[[ -z "${GITHUB_TOKEN:-}" ]] || hdr=(-H "Authorization: Bearer $GITHUB_TOKEN")
page=1
while :; do
    : >"$work/headers"
    : >"$work/body"
    # Keep HTTP failures distinct from a valid list with no matching release.
    # The empty-array expansion also works with macOS's bash 3.2 and set -u.
    if status=$(curl -sS -L --fail-with-body --retry 3 --retry-all-errors \
        ${hdr[@]+"${hdr[@]}"} -D "$work/headers" -o "$work/body" -w '%{http_code}' \
        "https://api.github.com/repos/moq-dev/moq/releases?per_page=100&page=$page"); then
        fetched=1
    else
        fetched=0
    fi
    echo "GitHub releases page $page: HTTP $status" >&2
    # Response headers contain no credentials. Retain the request ID and rate
    # limit evidence so a failed lookup can be investigated from its CI log.
    grep -iE '^(x-github-request-id|x-ratelimit-[^:]+|link):' "$work/headers" >&2 || true
    if [[ $fetched -ne 1 || "$status" != 200 ]]; then
        echo "failed to query $prefix releases" >&2
        head -c 4096 "$work/body" >&2
        exit 1
    fi
    if ! jq -es 'length == 1 and (.[0] | type == "array" and all(.[];
        type == "object" and (.tag_name | type == "string") and
        (.draft | type == "boolean") and (.prerelease | type == "boolean")))' \
        "$work/body" >/dev/null; then
        echo "invalid GitHub releases response for $prefix" >&2
        head -c 4096 "$work/body" >&2
        exit 1
    fi
    release=$(jq -c --arg prefix "$prefix" \
        'first(.[] | select(.draft == false and .prerelease == false and
        (.tag_name | startswith($prefix)))) // empty' "$work/body")
    if [[ -n "$release" ]]; then
        printf '%s\n' "$release"
        exit 0
    fi
    echo "No $prefix match on page $page; received tags:" >&2
    jq -c '[.[].tag_name]' "$work/body" >&2
    # A short page can still have a next link. Follow GitHub's pagination
    # signal instead of assuming every response contains exactly 100 releases.
    if ! grep -qi '^link:.*rel="next"' "$work/headers"; then
        echo "no $prefix* release found after $page page(s)" >&2
        exit 1
    fi
    page=$((page + 1))
done
