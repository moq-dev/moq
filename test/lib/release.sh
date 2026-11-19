# shellcheck shell=bash
#
# Fetch a published binary: `release_binary <crate> <binary> <version> <root>` leaves it at
# <root>/bin/<binary> and prints its --version.
#
# The release's checksummed asset for this host when it has one; otherwise the exact
# published crate, built. A corrupt or mismatched asset is a hard failure, never a fallback.
# Needs WORKSPACE, the checkout, whose target directory caches that build.

release_binary() {
    local crate="$1" binary="$2" version="$3" root="$4"
    local tag="$crate-v$version" triple="" asset digest
    mkdir -p "$root/bin"
    case "$(uname -sm)" in
        "Linux x86_64") triple=x86_64-unknown-linux-gnu ;;
        "Linux aarch64") triple=aarch64-unknown-linux-gnu ;;
    esac
    digest=""
    if [[ -n "$triple" ]]; then
        asset="$tag-$triple"
        # A release without SHA256SUMS (404), or without this host's asset in it, ships no binary
        # to use. Any other answer, once retries run out, is a failure: building instead would
        # pass without testing the published asset.
        local code
        code=$(curl -sSL --retry 3 -o "$root/$crate-sums" -w '%{http_code}' \
            "https://github.com/moq-dev/moq/releases/download/$tag/SHA256SUMS") || code=000
        case "$code" in
            200) digest=$(awk -v name="$asset" '$2 == name { print $1 }' "$root/$crate-sums") ;;
            404) ;;
            *)
                echo "error: fetching $tag's SHA256SUMS answered HTTP $code" >&2
                return 1
                ;;
        esac
    fi
    if [[ -n "$digest" ]]; then
        [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || {
            echo "error: $tag lists a malformed checksum for $asset" >&2
            return 1
        }
        curl -fLsS --retry 3 "https://github.com/moq-dev/moq/releases/download/$tag/$asset" -o "$root/bin/$binary"
        printf '%s  %s\n' "$digest" "$root/bin/$binary" | sha256sum -c - >&2
        chmod +x "$root/bin/$binary"
    else
        cargo install --locked --version "$version" --root "$root" --target-dir "$WORKSPACE/target/compat-released" "$crate"
    fi
    "$root/bin/$binary" --version
}
