#!/usr/bin/env bash
# Compare this checkout with registry releases using the existing interop drivers.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../../sh/local.sh" bash "$0" "$@"
fi
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
harness_begin wire-compat "just test wire-compat$(harness_argv "$@")"
# The CI report's records (see harness_result). The token and catalog lanes are known up
# front; the session cells depend on the resolved releases, so they are recorded as they run.
record() {
    harness_result "$1" suite=wire-compat "${@:2}"
}
for source in current released js-current js-released; do
    record "not run" case=token publisher="$source"
    record "not run" case=catalog/container publisher="$source"
done
# --remeasure runs the cells planned breaks cover, then rewrites planned-breaks.json
# from the result: re-pinned to these releases if a cell still fails, else deleted.
remeasure=
case "$*" in
    "") ;;
    --remeasure) remeasure=--remeasure ;;
    *)
        echo "usage: just test wire-compat [--remeasure]" >&2
        exit 2
        ;;
esac
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
bun "$DIR/compat/resolve.ts" matrix "$HARNESS_RUN/current-help" "$HARNESS_RUN/released-help" "$DIR/compat/planned-breaks.json" "$HARNESS_RUN/shared" "$HARNESS_RUN/versions.json" "$HARNESS_RUN/controls" ${remeasure:+"$remeasure"}

# A separate package root prevents bun workspaces from replacing released deps
# with the checkout. Copy exactly the same client into both environments; it uses
# only API both package sets have, so it never follows the checkout's interop client.
mkdir -p "$HARNESS_RUN/js"
cp "$DIR/compat/client.ts" "$DIR/compat/transport.ts" "$HARNESS_RUN/js/"
python3 - "$HARNESS_RUN/versions.json" "$HARNESS_RUN/js/package.json" <<'PY'
import json, sys
versions = json.load(open(sys.argv[1]))
deps = {name: version for name, version in versions.items() if name.startswith("@moq/")}
# tsx only loads the TypeScript clients and never touches the wire.
deps["tsx"] = "^4.23.15"
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
    for verifier in current released; do
        if [[ "$verifier" == current ]]; then bin="$CURRENT"; else bin="$RELEASED"; fi
        "$bin" auth verify --key "$HARNESS_RUN/key.jwk" --in "$HARNESS_RUN/$source.jwt" >"$HARNESS_RUN/$source.$verifier.claims"
        # `auth verify` prints the claims' Rust Debug form, the CLI's only output.
        bun "$DIR/compat/claims.ts" "$HARNESS_RUN/$source.$verifier.claims"
    done
    bun "$DIR/compat/client.ts" verify "$HARNESS_RUN/key.jwk" "$HARNESS_RUN/$source.jwt"
    bun "$HARNESS_RUN/js/client.ts" verify "$HARNESS_RUN/key.jwk" "$HARNESS_RUN/$source.jwt"
    echo "PASS token signed by $source"
    record pass case=token publisher="$source"
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
    record pass case=catalog/container publisher="$source"
done

# Every session cell runs, so one nightly names every break; the run fails at the
# end. A cell a planned break covers is logged with that break's name instead, or
# with --remeasure runs and is recorded against that break without failing the run.
failed=()
ran=0
skipped=0
: >"$HARNESS_RUN/measured"
cell() {
    local lane="$1" version="$2" relay_source="$3" source="$4" planned result reader started=$SECONDS
    shift 4
    local name="$version relay=$relay_source publisher=$source $lane"
    # A control reads its own source's output; every other cell reads the opposite one's.
    case "$lane:$source" in
        media-control:* | *:both) reader="$source" ;;
        *:current) reader=released ;;
        *) reader=current ;;
    esac
    local cell=(case="$lane relay=$relay_source" publisher="$source" subscriber="$reader" transport=auto version="$version")
    planned=$(bun "$DIR/compat/resolve.ts" skip "$DIR/compat/planned-breaks.json" "$lane" "$version" "$relay_source" "$source")
    if [[ -n "$planned" && -z "$remeasure" ]]; then
        echo "SKIP $name: planned break $planned"
        record skip "${cell[@]}" detail="planned break $planned"
        skipped=$((skipped + 1))
        return
    fi
    ran=$((ran + 1))
    echo "=== $name${planned:+ (planned break $planned)} ==="
    # The nested interop.sh runs are this cell, not cases of their own.
    if (unset HARNESS_RESULTS && "$@"); then
        echo "PASS $name"
        result=pass
    else
        echo "FAIL $name"
        result=fail
        [[ -n "$planned" ]] || failed+=("$name")
    fi
    cell+=(seconds=$((SECONDS - started)))
    [[ -z "$planned" ]] || cell+=(detail="planned break $planned")
    record "$result" "${cell[@]}"
    [[ -z "$planned" ]] || printf '%s\t%s\t%s\n' "$planned" "$result" "$name" >>"$HARNESS_RUN/measured"
}

fetch() {
    local relay="$1" pub="$2" sub="$3" js="$4" version="$5"
    RELAY_BIN="$relay" CLI_BIN="$pub" INTEROP_SUB_MOQ="$sub" INTEROP_VERSION="$version" \
        INTEROP_COMPAT_CLIENT="$js" INTEROP_FETCH_TRACK=1 \
        bash "$DIR/interop.sh" --publishers rust --subscribers rust
}

# The released CLI's fMP4 import restamps media on its arrival clock, so its tone
# does not start at timestamp zero and its decodes cannot be held to the tone's
# phase (see phase_checked in interop.sh). Its readers' phase is still checked.
live_publisher() {
    [[ "$1" == "$RELEASED" ]] && echo 1 || echo 0
}

# Same-source cells keep exercising the compat reader while version-pairing
# exceptions skip cross-source media. A control always uses its own binary,
# relay, and package set, and can never be covered by a planned break.
while read -r source version <&3; do
    case "$source" in
        current) relay="$TARGET/debug/moq-relay" pub="$CURRENT" js="$DIR/compat" ;;
        released) relay="$HARNESS_RUN/released/bin/moq-relay" pub="$RELEASED" js="$HARNESS_RUN/js" ;;
        *)
            echo "unknown media control source: $source" >&2
            exit 1
            ;;
    esac
    cell media-control "$version" "$source" "$source" env RELAY_BIN="$relay" CLI_BIN="$pub" INTEROP_SUB_MOQ="$pub" INTEROP_VERSION="$version" \
        INTEROP_COMPAT_CLIENT="$js" INTEROP_DECODE=1 INTEROP_PUB_LIVE="$(live_publisher "$pub")" bash "$DIR/interop.sh" --subscribers rust,js-native-node
done 3<"$HARNESS_RUN/controls"

# Existing media drivers: current publisher -> released clients and the reverse,
# through either relay. The relay offers only one version, so JS cannot silently
# negotiate its preferred draft instead of the draft this cell names. Only moq-lite
# drafts are listed: our clients and relays always prefer lite with each other, so
# IETF between them is not compared here (`just test interop` covers IETF).
# Fd 3, so a child reading stdin (ffmpeg does) cannot consume the version list.
# Dropping a released FETCH is a cell, so a planned break on its version covers it too.
fetch_kept() {
    [[ "$1" != true || "$2" == true ]] || {
        echo "the checkout removed published FETCH"
        return 1
    }
}
while read -r version <&3; do
    released_relay="$HARNESS_RUN/released/bin/moq-relay"
    # Each moq-net library states whether the draft has FETCH, so a FETCH cell that
    # fails is a failure, never a skip. Dropping a released capability fails too.
    current_fetch=$("$TARGET/compat-hang-current/debug/compat-container" fetch-supported "$version")
    released_fetch=$("$TARGET/compat-hang-released/debug/compat-container" fetch-supported "$version")
    cell fetch-capability "$version" both both fetch_kept "$released_fetch" "$current_fetch"
    if [[ "$released_fetch" != true || "$current_fetch" != true ]]; then
        echo "SKIP $version fetch and js-publish: no FETCH on this draft in both libraries"
        skipped=$((skipped + 8))
    fi
    for relay_source in current released; do
        if [[ "$relay_source" == current ]]; then relay="$TARGET/debug/moq-relay"; else relay="$released_relay"; fi
        for source in current released; do
            # Each side's compat client, with its version-pinning transport beside it.
            if [[ "$source" == current ]]; then
                pub="$CURRENT" sub="$RELEASED" own_js="$DIR/compat" other_js="$HARNESS_RUN/js"
            else
                pub="$RELEASED" sub="$CURRENT" own_js="$HARNESS_RUN/js" other_js="$DIR/compat"
            fi
            # One cell per reader, so a planned break in one reader's release keeps the other running.
            for reader in rust js-native-node; do
                cell "media-$reader" "$version" "$relay_source" "$source" env RELAY_BIN="$relay" CLI_BIN="$pub" INTEROP_SUB_MOQ="$sub" INTEROP_VERSION="$version" \
                    INTEROP_COMPAT_CLIENT="$other_js" INTEROP_DECODE=1 INTEROP_PUB_LIVE="$(live_publisher "$pub")" bash "$DIR/interop.sh" --subscribers "$reader"
            done
            [[ "$released_fetch" == true && "$current_fetch" == true ]] || continue
            # Drive JS publication from actual subscriber demand: the publisher's own JS
            # subscribes and observes the group, which the opposite Rust CLI then
            # FETCHes by ID. Reading the newest group races its live delivery.
            cell js-publish "$version" "$relay_source" "$source" env RELAY_BIN="$relay" CLI_BIN="$pub" INTEROP_SUB_MOQ="$sub" INTEROP_VERSION="$version" \
                INTEROP_COMPAT_CLIENT="$own_js" INTEROP_FETCH_DATA=1 bash "$DIR/interop.sh" --publishers js-native --subscribers rust
            # Both Rust sources fetch the same group and compare exact payloads. The
            # publisher's own JS finds the group, so a catalog break fails only the
            # media cell.
            cell fetch "$version" "$relay_source" "$source" fetch "$relay" "$pub" "$sub" "$own_js" "$version"
        done
    done
done 3<"$HARNESS_RUN/shared"
echo "session cells: $ran ran, $skipped skipped"
if ((${#failed[@]})); then
    printf 'FAIL %s\n' "${failed[@]}" >&2
    exit 1
fi
if [[ -n "$remeasure" ]]; then
    bun "$DIR/compat/resolve.ts" remeasure "$DIR/compat/planned-breaks.json" "$HARNESS_RUN/current-help" "$HARNESS_RUN/released-help" "$HARNESS_RUN/versions.json" "$HARNESS_RUN/measured"
    bun x biome format --write "$DIR/compat/planned-breaks.json"
    echo "rewrote compat/planned-breaks.json: review the diff and refresh each reason"
fi
echo "wire compatibility: token, session, catalog, and container lanes passed"
