#!/usr/bin/env bash
# Exercise release discovery without registry access or client toolchains.
set -euo pipefail
script=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/release.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export RELEASE_FIXTURES="$work/responses"
mkdir -p "$work/bin" "$RELEASE_FIXTURES"
cat >"$work/bin/curl" <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
while [[ $# -gt 0 ]]; do
    case "$1" in
        -D) headers=$2; shift ;;
        -o) body=$2; shift ;;
        https://api.github.com/repos/moq-dev/moq/releases\?*) page=${1##*&page=} ;;
    esac
    shift
done
cp "$RELEASE_FIXTURES/$page.headers" "$headers"
cp "$RELEASE_FIXTURES/$page.json" "$body"
printf '%s' "${RELEASE_STATUS:-200}"
exit "${RELEASE_CURL_EXIT:-0}"
CURL
chmod +x "$work/bin/curl"
export PATH="$work/bin:$PATH"
# Do not send a developer's token to the fixture command.
unset GITHUB_TOKEN RELEASE_STATUS RELEASE_CURL_EXIT

reset_pages() {
    rm -f "$RELEASE_FIXTURES"/*
    printf 'X-GitHub-Request-Id: fixture-id\r\n' >"$RELEASE_FIXTURES/1.headers"
}
expect_tag() {
    local prefix=$1 expected=$2
    "$script" "$prefix" >"$work/out" 2>"$work/err" || {
        cat "$work/err" >&2
        exit 1
    }
    [[ $(jq -r '.tag_name' "$work/out") == "$expected" ]]
    jq -e '.assets[0].browser_download_url == "https://example.com/asset.tar.gz"' "$work/out" >/dev/null
}
expect_error() {
    if "$script" libmoq-v >"$work/out" 2>"$work/err"; then
        echo "expected release lookup to fail: $1" >&2
        exit 1
    fi
    [[ ! -s "$work/out" ]]
    grep -qF "$1" "$work/err"
    grep -qF 'fixture-id' "$work/err"
}

reset_pages
jq -n '[{tag_name:"libmoq-v9",draft:true,prerelease:false},
    {tag_name:"libmoq-v8-rc1",draft:false,prerelease:true}] +
    ["libmoq-v7", "moq-gst-v6", "libmoq-v5" | {tag_name:.,draft:false,prerelease:false,
    assets:[{browser_download_url:"https://example.com/asset.tar.gz"}]}]' >"$RELEASE_FIXTURES/1.json"
expect_tag libmoq-v libmoq-v7
expect_tag moq-gst-v moq-gst-v6
echo 'ok: selects the first stable matching release and preserves its assets'

cp "$RELEASE_FIXTURES/1.json" "$RELEASE_FIXTURES/2.json"
cp "$RELEASE_FIXTURES/1.headers" "$RELEASE_FIXTURES/2.headers"
printf 'link: <https://api.github.com/repositories/508763694/releases?per_page=100&page=2>; rel="next"\r\n' >>"$RELEASE_FIXTURES/1.headers"
jq -n '[range(100) | {tag_name:("other-v" + tostring),draft:false,prerelease:false}]' >"$RELEASE_FIXTURES/1.json"
expect_tag libmoq-v libmoq-v7
expect_tag moq-gst-v moq-gst-v6
echo 'ok: finds both clients beyond the first 100 releases'
printf '[]' >"$RELEASE_FIXTURES/1.json"
expect_tag libmoq-v libmoq-v7
echo 'ok: follows a next link even on a short page'
printf '[]' >"$RELEASE_FIXTURES/2.json"
expect_error 'no libmoq-v* release found after 2 page(s)'
echo 'ok: reports absence only after exhausting the pages'

reset_pages
: >"$RELEASE_FIXTURES/1.json"
expect_error 'invalid GitHub releases response'
printf '[][]' >"$RELEASE_FIXTURES/1.json"
expect_error 'invalid GitHub releases response'
printf '{' >"$RELEASE_FIXTURES/1.json"
expect_error 'invalid GitHub releases response'
printf '{"message":"not an array"}' >"$RELEASE_FIXTURES/1.json"
expect_error 'invalid GitHub releases response'
printf '[{"tag_name":123}]' >"$RELEASE_FIXTURES/1.json"
expect_error 'invalid GitHub releases response'
echo 'ok: rejects empty, malformed, and unsupported responses'

printf '{"message":"API rate limit exceeded"}' >"$RELEASE_FIXTURES/1.json"
export RELEASE_STATUS=403 RELEASE_CURL_EXIT=22
expect_error 'failed to query libmoq-v releases'
grep -qF 'HTTP 403' "$work/err"
grep -qF 'API rate limit exceeded' "$work/err"
export RELEASE_STATUS=000 RELEASE_CURL_EXIT=6
expect_error 'failed to query libmoq-v releases'
export RELEASE_STATUS=302 RELEASE_CURL_EXIT=0
expect_error 'failed to query libmoq-v releases'
echo 'ok: distinguishes HTTP and transport failures from absent releases'
