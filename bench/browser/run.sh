#!/usr/bin/env bash
# Real-browser transport and media benchmarks against a temporary relay.
#
#     bench/browser/run.sh [BASE] [--smoke] [--rounds N] [--scenarios transport,cmaf,media]
#
# Variants, each bundled from the same page sources:
#   current  this checkout, the reference
#   aa       this checkout again, so A/A spread shows the noise floor
#   copy     this checkout with one extra copy of every received stream chunk
#   base     BASE's checkout, when given
#
# The run directory is always kept: report.json, gzipped Chrome traces, and the
# relay log. See bench/README.md.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$HERE/../.." && pwd)

# shellcheck source-path=SCRIPTDIR source=../../test/lib/harness.sh
source "$WORKSPACE/test/lib/harness.sh"

RERUN="$(harness_env BENCH_PROFILE RELAY_BIN BROWSER_BIN)just bench-browser$(harness_argv "$@")"

BASE=
DRIVER=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --smoke)
            DRIVER+=(--smoke)
            shift
            ;;
        --rounds | --warmup | --scenarios | --timeout)
            if [[ $# -lt 2 || -z "${2:-}" || "$2" == -* ]]; then
                echo "error: $1 requires a value" >&2
                exit 2
            fi
            DRIVER+=("$1" "$2")
            shift 2
            ;;
        -*)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
        *)
            [[ -z $BASE ]] || {
                echo "error: one base revision at most" >&2
                exit 2
            }
            BASE="$1"
            shift
            ;;
    esac
done

# Evidence outlives the run, under the benchmark artifact root rather than $TMPDIR.
export HARNESS_RUNS="${HARNESS_RUNS:-$WORKSPACE/target/bench/browser}"
export HARNESS_KEEP=1
PROFILE="${BENCH_PROFILE:-release}"
RELAY="${RELAY_BIN:-}"

BASE_COMMIT=
if [[ -n $BASE ]]; then
    BASE_COMMIT=$(git -C "$WORKSPACE" rev-parse --verify "$BASE^{commit}")
fi

harness_begin bench-browser "$RERUN"

for tool in cargo bun git; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: $tool not found; run inside 'nix develop'" >&2
        exit 1
    }
done

# ── build ───────────────────────────────────────────────────────────────────
# One relay serves every variant: these benchmarks measure the browser, not the relay.
if [[ -z $RELAY ]]; then
    echo "building moq-relay ($PROFILE)..."
    flag=()
    [[ "$PROFILE" == "debug" ]] || flag=(--profile "$PROFILE")
    (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-relay)
    RELAY="${CARGO_TARGET_DIR:-$WORKSPACE/target}/$PROFILE/moq-relay"
fi

cd "$WORKSPACE"
bun install --frozen-lockfile
# BROWSER_BIN overrides the Chromium provided by the root devShell.

cd "$HERE"
# The in-memory paths share Chromium installation and artifact retention with this lane.
for benchmark in frames drift; do
    bun micro.ts --bench "$benchmark" --rounds 1 --out "$HARNESS_RUN/$benchmark.json" \
        current="$WORKSPACE" aa="$WORKSPACE"
done
bun test ./fixture.test.ts ./summary.test.ts
variants=(current="$WORKSPACE" aa="$WORKSPACE" copy="$WORKSPACE:copy")
names=current,aa,copy
if [[ -n $BASE_COMMIT ]]; then
    # Bundled then removed at once: the run directory keeps evidence, not a checkout.
    base="$HARNESS_RUN/base"
    (
        trap 'git -C "$WORKSPACE" worktree remove --force "$base" >/dev/null 2>&1 || true' EXIT
        git -C "$WORKSPACE" worktree add --detach "$base" "$BASE_COMMIT" >/dev/null
        (cd "$base" && bun install --frozen-lockfile >/dev/null)
        bun build.ts "$HARNESS_RUN/dist" base="$base"
    )
    names=base,current,aa,copy
fi
bun build.ts "$HARNESS_RUN/dist" "${variants[@]}"

dirty=$(git -C "$WORKSPACE" status --porcelain | wc -l)
BENCH_REVISIONS=$(printf '{"current":"%s","dirty_paths":%d,"base":"%s"}' \
    "$(git -C "$WORKSPACE" rev-parse HEAD)" "$dirty" "$BASE_COMMIT")
export BENCH_REVISIONS

# ── relay ───────────────────────────────────────────────────────────────────
harness_port relay "${BENCH_PORT:-}"
url="http://127.0.0.1:${HARNESS_PORT}"
if harness_probe "$url/certificate.sha256"; then
    echo "error: something is already listening on 127.0.0.1:${HARNESS_PORT}" >&2
    exit 1
fi
harness_spawn relay "$HARNESS_RUN/relay.log" "$RELAY" "$HERE/relay.toml" \
    --listen "127.0.0.1:${HARNESS_PORT}" --web-http-listen "127.0.0.1:${HARNESS_PORT}"
if ! harness_ready "$url/certificate.sha256" 30 "$HARNESS_PID"; then
    echo "relay never became ready" >&2
    sed 's/^/  relay: /' "$HARNESS_RUN/relay.log" >&2 || true
    exit 1
fi

# ── run ─────────────────────────────────────────────────────────────────────
# Spawned so a SIGTERM reaches teardown while the shell waits, instead of after the driver.
status=0
harness_spawn driver - bun driver.ts --relay "$url" --dist "$HARNESS_RUN/dist" --out "$HARNESS_RUN" \
    --variants "$names" ${DRIVER[@]+"${DRIVER[@]}"}
harness_wait "$HARNESS_PID" || status=$?
if [[ $status -ne 0 ]]; then
    echo "── relay log tail ($HARNESS_RUN/relay.log) ──" >&2
    tail -n 20 "$HARNESS_RUN/relay.log" | sed 's/^/  relay: /' >&2 || true
fi
exit $status
