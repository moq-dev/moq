#!/usr/bin/env bash
# moq-quic, built from this checkout, through the pinned QUIC Interop Runner
# against the peers in cases.toml, as server and as client. Fails on any cell
# whose result differs from what cases.toml expects.
#
#   run.sh [--peers quic-go,ngtcp2] [--cases handshake,transfer] [--roles server,client]
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../../sh/local.sh" bash "$0" "$@"
fi

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"

# `moq-quic` among the peers is the self pair.
peers="moq-quic,$(python3 -I "$DIR/matrix.py" peers)"
cases=""
roles="server,client"
argv=("$@")
while [[ $# -gt 0 ]]; do
    case "$1" in
        --peers)
            peers="$2"
            shift 2
            ;;
        --cases)
            cases="$2"
            shift 2
            ;;
        --roles)
            roles="$2"
            shift 2
            ;;
        *)
            echo "usage: run.sh [--peers A,B] [--cases A,B] [--roles server,client]" >&2
            exit 2
            ;;
    esac
done
for role in ${roles//,/ }; do
    [[ "$role" == server || "$role" == client ]] || {
        echo "unknown role: $role" >&2
        exit 2
    }
done

# The self pair runs in the server pass only.
others=$(tr , '\n' <<<"$peers" | grep -vx moq-quic | paste -sd , || true)
passes=()
for role in ${roles//,/ }; do
    if [[ "$role" == server || -n "$others" ]]; then
        passes+=("$role")
    fi
done
if [[ ${#passes[@]} -eq 0 ]]; then
    echo "nothing to run: the self pair runs as server only, so --roles client needs another peer" >&2
    exit 2
fi

# The runner names its containers and networks, so one Docker daemon runs one matrix at a
# time. The lock sits in /tmp whatever TMPDIR says, since every run shares the daemon.
exec {lock}>"/tmp/moq-quic-interop-$(id -u).lock"
if ! flock -n "$lock"; then
    echo "waiting for another quic-interop run on this host" >&2
    flock "$lock"
fi

system=$(nix eval --impure --raw --expr builtins.currentSystem)
tools=$(nix build --cores 4 --no-link --print-out-paths "$WORKSPACE#legacyPackages.$system.quic-interop")
commit=$(python3 -I -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["runner"]["commit"])' "$DIR/cases.toml")

# Cargo names the binary it built, wherever its target directory resolves.
binary=$(cargo build --locked --release -p moq-tokio --example quic-interop \
    --manifest-path "$WORKSPACE/Cargo.toml" --message-format=json-render-diagnostics |
    python3 -I -c '
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "quic-interop":
        print(message["executable"])
')
[[ -x "$binary" ]] || {
    echo "cargo built no quic-interop example" >&2
    exit 1
}

harness_begin quic-interop "just test quic-interop$(harness_argv "${argv[@]}")"

# The image carries the binary and the exact loader and libraries it links, at the
# store paths it names, since the endpoint's Ubuntu cannot run a newer glibc's build.
context="$HARNESS_RUN/image"
harness_stage image/bin/quic-interop "$binary"
cp "$DIR/Dockerfile" "$DIR/run_endpoint.sh" "$context/"
mkdir -p "$context/nix"
while read -r lib; do
    mkdir -p "$context$(dirname "$lib")"
    cp -L "$lib" "$context$lib"
done < <(ldd "$binary" | grep -o '/nix/store/[^ ]*')
image=localhost/moq-quic-interop:latest
docker build --quiet --tag "$image" "$context" >"$HARNESS_RUN/image.log"

runner="$HARNESS_RUN/runner"
git init --quiet "$runner"
git -C "$runner" fetch --quiet --depth 1 https://github.com/quic-interop/quic-interop-runner.git "$commit"
git -C "$runner" checkout --quiet FETCH_HEAD
podman=()
if docker --version | grep -qi podman; then
    podman=(--podman)
fi
python3 -I "$DIR/matrix.py" prepare "$runner" "$image" ${podman[@]+"${podman[@]}"}

uv venv --quiet --python 3.12 "$HARNESS_RUN/venv"
uv pip install --quiet --python "$HARNESS_RUN/venv" -r "$DIR/requirements.txt"
cp "$DIR/sitecustomize.py" "$("$HARNESS_RUN/venv/bin/python" -c 'import sysconfig; print(sysconfig.get_path("purelib"))')/"

tests=()
[[ -z "$cases" ]] || tests=(-t "$cases")
results=()
status=0
for role in "${passes[@]}"; do
    if [[ "$role" == server ]]; then
        pair=(-s moq-quic -c "$peers")
    else
        pair=(-s "$others" -c moq-quic)
    fi
    echo "moq-quic as $role: run.py ${pair[*]}"
    # The runner calls a client that fails every server unsupported unless told not
    # to, which would hide a moq-quic regression that breaks against every peer.
    (cd "$runner" && env -u PYTHONPATH PATH="$tools/bin:$PATH" "$HARNESS_RUN/venv/bin/python" run.py \
        "${pair[@]}" ${tests[@]+"${tests[@]}"} -n "moq-quic${others:+,$others}" \
        -l "$HARNESS_RUN/logs/$role" -j "$HARNESS_RUN/$role.json" -m) || true
    if [[ ! -s "$HARNESS_RUN/$role.json" ]]; then
        echo "the runner wrote no results for moq-quic as $role" >&2
        status=1
        continue
    fi
    results+=("$HARNESS_RUN/$role.json")
done
if [[ ${#results[@]} -gt 0 ]]; then
    python3 -I "$DIR/matrix.py" check "${results[@]}" || status=1
fi
exit "$status"
