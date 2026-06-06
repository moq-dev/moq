#!/usr/bin/env bash
# Cross-implementation token interop smoke test against the PUBLIC packages.
#
# moq-relay authenticates with JWTs minted by the moq-token tooling, which ships
# in several flavours from several registries:
#
#   - rust    : the moq-token-cli binary (cargo / brew / apt / nix), on PATH
#   - js-node : the @moq/token npm package's `moq-token` CLI, run under node
#   - js-bun  : the same published npm package, run under bun
#
# A token minted by any one of these must verify under every other one, or a
# relay keyed by implementation A would reject a perfectly valid token from a
# publisher using implementation B. This script proves that cross-verification
# holds for the *published* artifacts: it installs each, then for every
# (generator x verifier x algorithm) cell it has the generator mint a key and
# sign a token, and the verifier check it. A negative pass confirms each verifier
# actually rejects tampered tokens and the wrong key, so a green cell means
# "accepts the valid one AND refuses the bad ones", not "accepts everything".
#
# This is the install/packaging companion to moq's in-tree token unit tests:
# those run against workspace source with hardcoded fixtures; this runs the
# real CLIs a user installs, live on both sides.
set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
JS_DIR="$SMOKE_DIR/clients/token/js"

GENERATORS="rust"
VERIFIERS="rust"
# Cover both code paths in each impl: symmetric (HS256) and every asymmetric
# family they support — EdDSA (OKP/Ed25519), ES256 (EC), RS256 (RSA) — so the
# default matrix (and `just token-full` / CI, which don't pass --algorithms)
# can't silently stop exercising one. Override with --algorithms / TOKEN_ALGORITHMS.
ALGORITHMS="${TOKEN_ALGORITHMS:-HS256,EdDSA,ES256,RS256}"

# The Rust CLI under test. Whatever channel installed it (cargo/brew/apt/nix)
# just has to leave it on PATH; override here to point at a specific build.
TOKEN="${TOKEN_BIN:-moq-token-cli}"

# The published Docker image for the `rust-docker` cell. Untagged = :latest, the
# tag the release pipeline moves to the newest version; pulled fresh each run.
DOCKER_TOKEN_IMAGE="${DOCKER_TOKEN_IMAGE:-moqdev/moq-token-cli}"
# Container runtime for that cell. `docker` by default (what GitHub's Linux
# runners ship); set TOKEN_DOCKER=podman to use a drop-in-compatible one.
DOCKER="${TOKEN_DOCKER:-docker}"

# Canary claims. Distinctive values so a verifier's claim dump can be grepped for
# them regardless of output format (Rust prints a debug struct, JS prints JSON;
# both contain these literal strings if the claims survived the round trip).
ROOT="smoke-root"

require_value() {
    if [[ $# -lt 2 || -z "${2:-}" || "$2" == -* ]]; then
        echo "error: $1 requires a value" >&2
        exit 2
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --generators)
            require_value "$@"
            GENERATORS="$2"
            shift 2
            ;;
        --verifiers)
            require_value "$@"
            VERIFIERS="$2"
            shift 2
            ;;
        --algorithms)
            require_value "$@"
            ALGORITHMS="$2"
            shift 2
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

IFS=',' read -r -a GEN_LIST <<<"$GENERATORS"
IFS=',' read -r -a VER_LIST <<<"$VERIFIERS"
IFS=',' read -r -a ALGO_LIST <<<"$ALGORITHMS"

needs() {
    # needs <impl>: true if <impl> appears as a generator or verifier.
    local impl="$1" x
    for x in "${GEN_LIST[@]}" "${VER_LIST[@]}"; do [[ "$x" == "$impl" ]] && return 0; done
    return 1
}

TMP=$(mktemp -d)
CLI_NODE="" # node + @moq/token CLI path (set in prepare)
CLI_BUN=""  # bun  + @moq/token CLI path (set in prepare)
BROKEN_IMPLS=""

mark_broken() {
    # An implementation whose published package won't install fails only its own
    # matrix cells, instead of aborting the run: a broken registry artifact should
    # show up as a red cell, not hide every other result.
    BROKEN_IMPLS="$BROKEN_IMPLS $1"
    echo "  WARN  $1 unavailable: $2"
}

is_broken() {
    local impl="$1" x
    for x in $BROKEN_IMPLS; do [[ "$x" == "$impl" ]] && return 0; done
    return 1
}

# shellcheck disable=SC2329  # invoked indirectly via 'trap cleanup EXIT'
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

have() { command -v "$1" >/dev/null 2>&1; }

# ── per-implementation adapters ──────────────────────────────────────────────
# Each implementation's CLI differs (flag names, key encoding, verify output),
# so every operation is funnelled through an adapter that normalises it. The
# contract:
#   gen <impl> <algo> <dir>   -> writes <dir>/sign.jwk (key that signs) and
#                                <dir>/verify.jwk (key that verifies)
#   sign <impl> <signkey> <algo>  -> prints a token to stdout
#   verify <impl> <verifykey> <token-file>  -> exit 0 + claim dump on stdout if
#                                accepted, non-zero if rejected
#
# Symmetric (HS256): sign.jwk == verify.jwk (shared secret).
# Asymmetric (EdDSA/ES256/RS256): verify.jwk is the public half.
# Key encodings cross over fine: the Rust CLI writes base64url-JSON and reads
# either; @moq/token writes plain JSON and reads either.

cli_for() {
    # The command prefix that runs each implementation's moq-token CLI. The word
    # split is deliberate (runtime + path, or a whole `docker run ...` line), so
    # callers expand it unquoted.
    case "$1" in
        rust) echo "$TOKEN" ;;
        # Mount TMP at its real path so the in-container CLI reads/writes the same
        # key/token files token.sh hands it. The image bundles the nix store, so
        # the binary's libiconv deps resolve (the brew bottle's bug doesn't apply).
        rust-docker) echo "$DOCKER run --rm -v $TMP:$TMP -w $TMP $DOCKER_TOKEN_IMAGE" ;;
        js-node) echo "node $CLI_NODE" ;;
        js-bun) echo "bun $CLI_BUN" ;;
        *) return 1 ;;
    esac
}

gen() {
    local impl="$1" algo="$2" dir="$3"
    mkdir -p "$dir"
    local cli
    cli=$(cli_for "$impl") || {
        echo "unknown generator: $impl" >&2
        return 1
    }
    case "$impl" in
        rust | rust-docker)
            if [[ "$algo" == HS* ]]; then
                # shellcheck disable=SC2086  # cli is a deliberate multi-word prefix
                $cli generate --algorithm "$algo" --out "$dir/sign.jwk"
                cp "$dir/sign.jwk" "$dir/verify.jwk"
            else
                # shellcheck disable=SC2086
                $cli generate --algorithm "$algo" --out "$dir/sign.jwk" --public "$dir/verify.jwk"
            fi
            ;;
        js-node | js-bun)
            if [[ "$algo" == HS* ]]; then
                # shellcheck disable=SC2086
                $cli generate --key "$dir/sign.jwk" --algorithm "$algo" >/dev/null
                cp "$dir/sign.jwk" "$dir/verify.jwk"
            else
                # shellcheck disable=SC2086
                $cli generate --key "$dir/sign.jwk" --algorithm "$algo" --public "$dir/verify.jwk" >/dev/null
            fi
            ;;
    esac
}

sign() {
    local impl="$1" signkey="$2" algo="$3" cli
    cli=$(cli_for "$impl") || {
        echo "unknown signer: $impl" >&2
        return 1
    }
    # Same flags for the PATH binary and the Docker image; JS uses the same ones too.
    # shellcheck disable=SC2086
    $cli sign --key "$signkey" --root "$ROOT" \
        --publish "pub-canary-$algo" --subscribe "sub-canary-$algo"
}

verify() {
    local impl="$1" verifykey="$2" tokenfile="$3" cli
    cli=$(cli_for "$impl") || {
        echo "unknown verifier: $impl" >&2
        return 1
    }
    case "$impl" in
        rust | rust-docker)
            # Rust verify reads the token from --in and ignores root (it just
            # decodes); it prints a debug dump of the claims on success.
            # shellcheck disable=SC2086
            $cli verify --key "$verifykey" --in "$tokenfile"
            ;;
        js-node | js-bun)
            # JS verify reads the token from stdin and enforces --root, so pass
            # the same root the token was signed with; it prints claims as JSON.
            # shellcheck disable=SC2086
            $cli verify --key "$verifykey" --root "$ROOT" <"$tokenfile"
            ;;
    esac
}

# ── setup ────────────────────────────────────────────────────────────────────
"$SMOKE_DIR/freshness.sh" || echo "WARN: freshness check failed (see above); continuing" >&2

if needs rust; then
    if ! have "$TOKEN"; then
        mark_broken rust "$TOKEN not found (cargo/brew/apt/nix install moq-token-cli)"
    # `have` only checks the file exists; actually run it once, since a broken
    # published binary (e.g. a Homebrew bottle that baked in a /nix/store rpath
    # and aborts on launch) is exactly the packaging failure this test exists to
    # catch. A broken CLI marks the whole rust row unavailable instead of crashing
    # mid-matrix.
    elif "$TOKEN" generate --algorithm HS256 --out /dev/null >"$TMP/rust-probe.log" 2>&1; then
        echo "rust:    $(command -v "$TOKEN")"
    else
        mark_broken rust "$TOKEN on PATH but won't run (see below)"
        sed 's/^/        /' "$TMP/rust-probe.log" >&2 || true
    fi
fi

if needs rust-docker; then
    if ! have "$DOCKER"; then
        mark_broken rust-docker "$DOCKER not found"
    elif ! "$DOCKER" info >"$TMP/docker-info.log" 2>&1; then
        mark_broken rust-docker "$DOCKER daemon not running"
    # Pull :latest fresh (the published-package equivalent of the other channels'
    # always-latest install), then run it once to confirm the image works.
    elif "$DOCKER" pull "$DOCKER_TOKEN_IMAGE" >"$TMP/docker-pull.log" 2>&1 &&
        "$DOCKER" run --rm "$DOCKER_TOKEN_IMAGE" generate --algorithm HS256 --out /dev/null >"$TMP/docker-probe.log" 2>&1; then
        echo "rust-docker: $DOCKER_TOKEN_IMAGE (latest, via $DOCKER)"
    else
        mark_broken rust-docker "$DOCKER pull/run $DOCKER_TOKEN_IMAGE failed (see below)"
        cat "$TMP/docker-pull.log" "$TMP/docker-probe.log" 2>/dev/null | sed 's/^/        /' >&2 || true
    fi
fi

if needs js-node || needs js-bun; then
    echo "installing js token client (@moq/token from npm)..."
    if ! have bun; then
        for v in js-node js-bun; do needs "$v" && mark_broken "$v" "bun not found (needed to install)"; done
    elif (cd "$JS_DIR" && bun install) >"$TMP/js-install.log" 2>&1; then
        # Resolve the published CLI path under each runtime we actually need.
        if needs js-bun; then
            if CLI_BUN=$(cd "$JS_DIR" && bun resolve-bin.mjs 2>"$TMP/js-bun-resolve.log"); then :; else
                mark_broken js-bun "could not resolve @moq/token CLI under bun"
                sed 's/^/        /' "$TMP/js-bun-resolve.log" >&2 || true
            fi
        fi
        if needs js-node; then
            if ! have node; then
                mark_broken js-node "node not found"
            elif CLI_NODE=$(cd "$JS_DIR" && node resolve-bin.mjs 2>"$TMP/js-node-resolve.log"); then :; else
                mark_broken js-node "could not resolve @moq/token CLI under node"
                sed 's/^/        /' "$TMP/js-node-resolve.log" >&2 || true
            fi
        fi
    else
        for v in js-node js-bun; do needs "$v" && mark_broken "$v" "bun install failed"; done
        sed 's/^/        /' "$TMP/js-install.log" >&2 || true
    fi
fi

# ── matrix ───────────────────────────────────────────────────────────────────
overall=0

# claims_ok <file> <algo>: the claim dump must carry every canary value, so a
# verifier that accepts the signature but mangles the payload still fails.
claims_ok() {
    local file="$1" algo="$2"
    grep -q "$ROOT" "$file" && grep -q "pub-canary-$algo" "$file" && grep -q "sub-canary-$algo" "$file"
}

echo "=== positive: generator mints, every verifier must ACCEPT ==="
for algo in "${ALGO_LIST[@]}"; do
    for g in "${GEN_LIST[@]}"; do
        if is_broken "$g"; then
            for v in "${VER_LIST[@]}"; do
                echo "  FAIL  $g -> $v ($algo, generator unavailable)"
                overall=1
            done
            continue
        fi
        keydir="$TMP/$g-$algo"
        token="$keydir/token.jwt"
        if ! gen "$g" "$algo" "$keydir" >"$keydir.gen.log" 2>&1; then
            for v in "${VER_LIST[@]}"; do echo "  FAIL  $g -> $v ($algo, key generation failed)"; done
            sed 's/^/        /' "$keydir.gen.log" >&2 || true
            overall=1
            continue
        fi
        if ! sign "$g" "$keydir/sign.jwk" "$algo" >"$token" 2>"$keydir.sign.log"; then
            for v in "${VER_LIST[@]}"; do echo "  FAIL  $g -> $v ($algo, signing failed)"; done
            sed 's/^/        /' "$keydir.sign.log" >&2 || true
            overall=1
            continue
        fi
        for v in "${VER_LIST[@]}"; do
            if is_broken "$v"; then
                echo "  FAIL  $g -> $v ($algo, verifier unavailable)"
                overall=1
                continue
            fi
            out="$TMP/verify-$g-$v-$algo.log"
            if verify "$v" "$keydir/verify.jwk" "$token" >"$out" 2>&1 && claims_ok "$out" "$algo"; then
                echo "  PASS  $g -> $v ($algo)"
            else
                echo "  FAIL  $g -> $v ($algo)"
                sed 's/^/        /' "$out" >&2 || true
                overall=1
            fi
        done
    done
done

echo "=== negative: every verifier must REJECT a tampered token and the wrong key ==="
# Use the first non-broken generator as the canonical minter for the bad tokens.
canon=""
for g in "${GEN_LIST[@]}"; do is_broken "$g" || {
    canon="$g"
    break
}; done
if [[ -z "$canon" ]]; then
    echo "  WARN  no working generator; skipping negative pass"
else
    for algo in "${ALGO_LIST[@]}"; do
        # Mint the canonical key + token here rather than reusing the positive
        # pass's artifacts: `canon` is only "not marked broken", and its positive
        # gen/sign for this algo may have failed (missing token.jwt), which would
        # abort the whole negative pass under `set -e` and hide every later cell.
        keydir="$TMP/$canon-neg-$algo"
        token="$keydir/token.jwt"
        if ! gen "$canon" "$algo" "$keydir" >"$keydir.gen.log" 2>&1; then
            echo "  FAIL  reject(*, $algo): canonical key generation ($canon) failed"
            sed 's/^/        /' "$keydir.gen.log" >&2 || true
            overall=1
            continue
        fi
        if ! sign "$canon" "$keydir/sign.jwk" "$algo" >"$token" 2>"$keydir.sign.log"; then
            echo "  FAIL  reject(*, $algo): canonical signing ($canon) failed"
            sed 's/^/        /' "$keydir.sign.log" >&2 || true
            overall=1
            continue
        fi
        # Tampered token: flip the FIRST character of the signature segment.
        # The first base64url char of a segment carries 6 significant bits, so
        # flipping it always changes the decoded signature bytes; appending or
        # touching the last char would not (a short HS256 sig has slack bits at
        # the tail that some decoders ignore, leaving the signature still valid).
        tampered="$keydir/tampered.jwt"
        trimmed=$(tr -d '[:space:]' <"$token")
        head=${trimmed%.*} # header.payload
        sig=${trimmed##*.} # signature
        first=${sig:0:1}
        flip=$([[ "$first" == "A" ]] && echo B || echo A)
        printf '%s.%s%s\n' "$head" "$flip" "${sig:1}" >"$tampered"
        for v in "${VER_LIST[@]}"; do
            if is_broken "$v"; then
                echo "  FAIL  reject($v, $algo): verifier unavailable"
                overall=1
                continue
            fi
            # (a) tampered token must be refused.
            if verify "$v" "$keydir/verify.jwk" "$tampered" >"$TMP/neg-tamper-$v-$algo.log" 2>&1; then
                echo "  FAIL  reject($v, $algo): accepted a tampered token"
                overall=1
            else
                echo "  PASS  reject($v, $algo): tampered token refused"
            fi
            # (b) a valid token verified against an unrelated key must be refused.
            wrongdir="$TMP/wrong-$v-$algo"
            if ! gen "$v" "$algo" "$wrongdir" >"$wrongdir.gen.log" 2>&1; then
                echo "  WARN  reject($v, $algo): could not mint a wrong key; skipping key check"
                continue
            fi
            if verify "$v" "$wrongdir/verify.jwk" "$token" >"$TMP/neg-wrongkey-$v-$algo.log" 2>&1; then
                echo "  FAIL  reject($v, $algo): accepted a token signed by a different key"
                overall=1
            else
                echo "  PASS  reject($v, $algo): wrong key refused"
            fi
        done
    done
fi

if [[ "$overall" -eq 0 ]]; then
    echo "token: all checks passed"
else
    echo "token: FAILURES detected" >&2
fi
exit "$overall"
