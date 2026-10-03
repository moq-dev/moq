#!/usr/bin/env bash
# Compare this checkout with registry releases using the existing interop drivers.
set -euo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
harness_begin wire-compat "just test wire-compat$(harness_argv "$@")"
[[ $# == 0 ]] || {
    echo "wire-compat takes no arguments" >&2
    exit 2
}
cd "$WORKSPACE"
bun install --frozen-lockfile
bun "$DIR/compat/resolve.ts" resolve "$HARNESS_RUN/versions.json"
value() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$HARNESS_RUN/versions.json" "$1"; }

# The binary asset and registry version must agree. A missing asset builds the
# exact published crate instead; a corrupt/mismatched asset is a hard failure.
release_binary() {
    local crate="$1" binary="$2" version asset digest url
    version=$(value "$crate")
    mkdir -p "$HARNESS_RUN/released/bin"
    if gh release view "$crate-v$version" --repo moq-dev/moq --json assets >"$HARNESS_RUN/$crate-assets.json" 2>"$HARNESS_RUN/$crate-assets.log"; then
        asset=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(next((a["url"] for a in d["assets"] if a["name"]==sys.argv[2]),""))' "$HARNESS_RUN/$crate-assets.json" "$crate-v$version-x86_64-unknown-linux-gnu")
    else
        asset=""
    fi
    if [[ -n "$asset" ]]; then
        # Release assets are an optimization for Linux x86_64, the nightly host.
        [[ "$(uname -sm)" == "Linux x86_64" ]] || asset=""
    fi
    if [[ -n "$asset" ]]; then
        url="https://github.com/moq-dev/moq/releases/download/$crate-v$version/SHA256SUMS"
        curl -fLS "$url" -o "$HARNESS_RUN/$crate-sums"
        digest=$(awk -v name="$crate-v$version-x86_64-unknown-linux-gnu" '$2 == name { print $1 }' "$HARNESS_RUN/$crate-sums")
        [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || {
            echo "release is missing its asset checksum" >&2
            exit 1
        }
        curl -fLS "$asset" -o "$HARNESS_RUN/released/bin/$binary"
        printf '%s  %s\n' "$digest" "$HARNESS_RUN/released/bin/$binary" | sha256sum -c -
        chmod +x "$HARNESS_RUN/released/bin/$binary"
    else
        cargo install --locked --version "$version" --root "$HARNESS_RUN/released" --target-dir "$WORKSPACE/target/compat-released" "$crate"
    fi
    "$HARNESS_RUN/released/bin/$binary" --version
}
release_binary moq-cli moq
release_binary moq-relay moq-relay
cargo build --locked -p moq-cli -p moq-relay
TARGET=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')
CURRENT="$TARGET/debug/moq"
RELEASED="$HARNESS_RUN/released/bin/moq"
"$CURRENT" --help >"$HARNESS_RUN/current-help"
"$RELEASED" --help >"$HARNESS_RUN/released-help"
bun "$DIR/compat/resolve.ts" matrix "$HARNESS_RUN/current-help" "$HARNESS_RUN/released-help" "$DIR/compat/planned-breaks.json" "$HARNESS_RUN/shared" "$HARNESS_RUN/versions.json"

# A separate package root prevents bun workspaces from replacing released deps
# with the checkout. Copy exactly the same clients into both environments.
mkdir -p "$HARNESS_RUN/js"
cp "$DIR/compat/client.ts" "$DIR/compat/transport.ts" "$DIR/clients/js-native/subscribe.ts" "$HARNESS_RUN/js/"
python3 - "$HARNESS_RUN/versions.json" "$HARNESS_RUN/js/package.json" <<'PY'
import json, sys
versions = json.load(open(sys.argv[1]))
deps = {name: versions[name] for name in ("@moq/net", "@moq/hang", "@moq/auth")}
deps.update({"@moq/web-transport": "^0.1.4", "@moq/json": "*", "tsx": "^4.23.15"})
json.dump({"private": True, "type": "module", "dependencies": deps}, open(sys.argv[2], "w"))
PY
(cd "$HARNESS_RUN/js" && bun install)
cp "$HARNESS_RUN/js/bun.lock" "$HARNESS_RUN/resolved-js.lock"

# JWT verification compares normalized permissions, not just signature validity.
"$CURRENT" auth generate --out "$HARNESS_RUN/key.jwk"
for source in current released js-current js-released; do
    case "$source" in
        current) "$CURRENT" auth sign --key "$HARNESS_RUN/key.jwk" --root compat --publish 'video/**' --subscribe '**' ;;
        released) "$RELEASED" auth sign --key "$HARNESS_RUN/key.jwk" --root compat --publish 'video/**' --subscribe '**' ;;
        js-current) bun "$DIR/compat/client.ts" sign "$HARNESS_RUN/key.jwk" ;;
        js-released) bun "$HARNESS_RUN/js/client.ts" sign "$HARNESS_RUN/key.jwk" ;;
    esac >"$HARNESS_RUN/$source.jwt"
    for verifier in "$CURRENT" "$RELEASED"; do
        "$verifier" auth verify --key "$HARNESS_RUN/key.jwk" --in "$HARNESS_RUN/$source.jwt" >"$HARNESS_RUN/claims.json"
    done
    bun "$DIR/compat/client.ts" verify "$HARNESS_RUN/key.jwk" "$HARNESS_RUN/$source.jwt"
    bun "$HARNESS_RUN/js/client.ts" verify "$HARNESS_RUN/key.jwk" "$HARNESS_RUN/$source.jwt"
    echo "PASS token signed by $source"
done

# Exercise the actual hang library release, independently of the version bundled
# into moq-cli. Compile one small, identical adapter against either source.
for source in current released; do
    mkdir -p "$HARNESS_RUN/hang-$source/src"
    cp "$DIR/compat/container.rs" "$HARNESS_RUN/hang-$source/src/main.rs"
    if [[ "$source" == current ]]; then dependency="{ path = \"$WORKSPACE/rs/hang\" }"; else dependency="\"=$(value hang)\""; fi
    cat >"$HARNESS_RUN/hang-$source/Cargo.toml" <<TOML
[package]
name = "compat-container"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
hang = $dependency
bytes = "1"
TOML
    cargo build --manifest-path "$HARNESS_RUN/hang-$source/Cargo.toml" --target-dir "$TARGET/compat-hang-$source"
    "$TARGET/compat-hang-$source/debug/compat-container" encode "$HARNESS_RUN/$source.catalog" "$HARNESS_RUN/$source.frame"
done
bun "$DIR/compat/client.ts" encode "$HARNESS_RUN/js-current.catalog" "$HARNESS_RUN/js-current.frame"
bun "$HARNESS_RUN/js/client.ts" encode "$HARNESS_RUN/js-released.catalog" "$HARNESS_RUN/js-released.frame"
for source in current released js-current js-released; do
    for consumer in current released; do
        "$TARGET/compat-hang-$consumer/debug/compat-container" decode "$HARNESS_RUN/$source.catalog" "$HARNESS_RUN/$source.frame"
    done
    bun "$DIR/compat/client.ts" decode "$HARNESS_RUN/$source.catalog" "$HARNESS_RUN/$source.frame"
    bun "$HARNESS_RUN/js/client.ts" decode "$HARNESS_RUN/$source.catalog" "$HARNESS_RUN/$source.frame"
    echo "PASS catalog/container written by $source"
done

# Existing media drivers: current publisher -> released clients and the reverse,
# through either relay. The relay offers only one version, so JS cannot silently
# negotiate its preferred draft instead of the draft this cell names.
while read -r version; do
    current_fetch=$("$TARGET/compat-hang-current/debug/compat-container" fetch-supported "$version")
    released_fetch=$("$TARGET/compat-hang-released/debug/compat-container" fetch-supported "$version")
    if [[ "$released_fetch" == true && "$current_fetch" != true ]]; then
        echo "checkout removed published FETCH capability for $version" >&2
        exit 1
    fi
    for relay_source in current released; do
        if [[ "$relay_source" == current ]]; then relay="$TARGET/debug/moq-relay"; else relay="$HARNESS_RUN/released/bin/moq-relay"; fi
        for source in current released; do
            if [[ "$source" == current ]]; then
                pub="$CURRENT"
                sub="$RELEASED"
                js="$HARNESS_RUN/js"
                transport="$js/transport.ts"
            else
                pub="$RELEASED"
                sub="$CURRENT"
                js="$DIR/clients/js-native"
                transport="$DIR/compat/transport.ts"
            fi
            echo "=== $version relay=$relay_source publisher=$source consumers=opposite ==="
            RELAY_BIN="$relay" MOQ_BIN="$pub" INTEROP_SUB_MOQ="$sub" INTEROP_VERSION="$version" \
                INTEROP_NATIVE_CLIENT="$js" INTEROP_COMPAT_TRANSPORT="$transport" INTEROP_DECODE=1 bash "$DIR/interop.sh" --subscribers rust,js-native-node
            if [[ "$source" == current ]]; then publisher_js="$DIR/compat"; else publisher_js="$HARNESS_RUN/js"; fi
            # Drive JS publication from actual subscriber demand. The CLI's newest-group
            # read uses SUBSCRIBE here, independently of one-shot FETCH support.
            RELAY_BIN="$relay" MOQ_BIN="$pub" INTEROP_SUB_MOQ="$sub" INTEROP_VERSION="$version" \
                INTEROP_JS_PUBLISH_CLIENT="$publisher_js" INTEROP_READ_CURRENT=1 bash "$DIR/interop.sh" --publishers js-native --subscribers rust
            if [[ "$current_fetch" == true && "$released_fetch" == true ]]; then
                # JS does not implement IETF FETCH in either direction. Both Rust
                # sources fetch the same completed group and compare exact payloads.
                echo "INFO JS IETF FETCH unsupported; exercising both Rust implementations"
                RELAY_BIN="$relay" MOQ_BIN="$pub" INTEROP_SUB_MOQ="$sub" INTEROP_VERSION="$version" \
                    INTEROP_COMPAT_TRANSPORT="$DIR/compat/transport.ts" INTEROP_FETCH_TRACK=1 bash "$DIR/interop.sh" --publishers rust --subscribers rust
            else
                echo "SKIP $version FETCH: not offered by both protocol implementations"
            fi
        done
    done
done <"$HARNESS_RUN/shared"
echo "wire compatibility: token, session, catalog, and container lanes passed"
