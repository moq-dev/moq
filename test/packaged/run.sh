#!/usr/bin/env bash
# Consume this checkout's release candidates from outside the workspace.
#
# Every gate the repo already has looks at the packages from the inside: cargo
# builds them as workspace members with path dependencies, and bun links them
# into one node_modules. Both hide the same class of defect -- a file the
# archive does not ship, a dependency nobody declared, a version a path
# dependency does not carry -- because the workspace supplies what the archive
# omits. moq-dev/smoke catches those, but only after a release, from a public
# registry. This is the bridge: build the archives a release would upload, then
# consume them from a directory that has never heard of this repo.
#
#     just test packaged                # lanes selected from the branch diff
#     just test packaged --rust --js    # both lanes, whatever the diff says
#     just test packaged --self-test    # the negative controls
#
# See README.md for what each lane checks and how to read the report.
set -euo pipefail

PACKAGED_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$PACKAGED_DIR/../.." && pwd)

# shellcheck source=test/packaged/common.sh
source "$PACKAGED_DIR/common.sh"

RUST=0
JS=0
SELF_TEST=0
KEEP=0
BASE=""
FILES=""
FEATURES=""

usage() {
    cat <<'EOF'
usage: run.sh [--rust] [--js] [--self-test] [--base REF] [--features LIST] [--keep]

  --rust           run the Rust lane regardless of what changed
  --js             run the JS lane regardless of what changed
  --self-test      run the negative controls instead of the lanes
  --base REF       diff against REF when selecting lanes (default: the branch base)
  --features LIST  cargo features for the Rust consumer, e.g. moq-native/quiche
  --keep           leave the staging directory behind and print its path
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --rust) RUST=1 ;;
        --js) JS=1 ;;
        --self-test) SELF_TEST=1 ;;
        --keep) KEEP=1 ;;
        --base)
            [[ $# -ge 2 && -n "${2:-}" && "$2" != -* ]] || die "--base requires a value"
            BASE="$2"
            shift
            ;;
        --features)
            [[ $# -ge 2 && -n "${2:-}" && "$2" != -* ]] || die "--features requires a value"
            FEATURES="$2"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown arg: $1" ;;
    esac
    shift
done

for tool in cargo bun node npm jq tar; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool not found; run inside 'nix develop'"
done

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/moq-packaged.XXXXXX")
# The consumers have to sit outside the repo: bun and npm walk up looking for a
# workspace root, and cargo walks up looking for a workspace and a config, so a
# consumer staged anywhere below the checkout would silently rejoin it.
case "$STAGE" in
    "$WORKSPACE"/*) die "staging directory $STAGE is inside the workspace" ;;
esac

cleanup() {
    if ((KEEP)); then
        echo "packaged: staging kept at $STAGE"
    else
        rm -rf "$STAGE"
    fi
}
trap cleanup EXIT

# Called, not exec'd: exec replaces this process and the cleanup trap above dies
# with it, leaking the staging directory and losing --keep's message.
if ((SELF_TEST)); then
    "$PACKAGED_DIR/self-test.sh" "$STAGE"
    exit
fi

# ── lane selection ──────────────────────────────────────────────────────────
# Only the changed packages are candidates, for the same reason `just check` is
# scoped: the archive of a crate nothing touched is the archive the last run
# already consumed. The Rust lane reuses `just rs _select`, which expands a
# changed file to its crate plus every workspace crate that depends on it.

# Every publishable crate a plain `just rs check` compiles. The default members
# are the right unscoped set for the same reason they are the right one there:
# libmoq, moq-ffi, and moq-gst need a C toolchain, maturin, and GStreamer, so
# they are released by their own workflows rather than by a casual build.
all_rust() {
    cargo metadata --no-deps --format-version 1 --manifest-path "$WORKSPACE/Cargo.toml" |
        jq -r '
			.workspace_default_members as $default
			| .packages[]
			| select(.publish == null)
			| select(.id as $id | $default | index($id))
			| .name
		'
}

# Every JS workspace that publishes, which is the ones with a release script:
# the same predicate js/common/package.ts uses to decide whether to emit a
# jsr.json.
all_js() {
    jq -r '.workspaces[]' "$WORKSPACE/package.json" |
        while read -r dir; do
            [[ -f "$WORKSPACE/$dir/package.json" ]] || continue
            jq -r 'select(.scripts.release != null) | .name' "$WORKSPACE/$dir/package.json"
        done
}

selection() {
    local files="$1"
    if [[ "$files" == ALL ]]; then
        rust_names=$(all_rust)
        js_names=$(all_js)
        return
    fi

    local packages
    packages=$(cd "$WORKSPACE" && just rs _select "$files")
    if [[ "$packages" == ALL ]]; then
        rust_names=$(all_rust)
    elif [[ -n "$packages" ]]; then
        rust_names=$(cd "$WORKSPACE" && just rs _names "$packages" | tr ' ' '\n' | grep -v '^$' || true)
    fi

    # A JS package is selected by a change under its directory, and its
    # dependents come along because js.sh stages the whole `workspace:` closure
    # below whatever it is handed.
    #
    # Everything selects everything when the change is one no single package
    # owns: the root manifests are the workspace list and the lockfile, and
    # js/common/ is the shared build (package.ts writes every published
    # package.json, and the vite plugins inline every worklet), so a defect there
    # lands in every tarball at once rather than in the one directory that
    # changed.
    if grep -qE '^(package\.json|bun\.lock)$|^js/common/' <<<"$files"; then
        js_names=$(all_js)
        return
    fi
    js_names=$(
        while read -r dir; do
            [[ -n "$dir" && -f "$WORKSPACE/$dir/package.json" ]] || continue
            grep -q "^$dir/" <<<"$files" || continue
            jq -r 'select(.scripts.release != null) | .name' "$WORKSPACE/$dir/package.json"
        done <<<"$(jq -r '.workspaces[]' "$WORKSPACE/package.json")"
    )
}

rust_names=""
js_names=""
if ((RUST == 0 && JS == 0)); then
    FILES=$(cd "$WORKSPACE" && just _changed "$BASE")
    selection "$FILES"
    [[ -n "$rust_names" ]] && RUST=1
    [[ -n "$js_names" ]] && JS=1
    if ((RUST == 0 && JS == 0)); then
        echo "packaged: nothing publishable changed."
        exit 0
    fi
else
    FILES=ALL
    selection ALL
fi

# ── lanes ───────────────────────────────────────────────────────────────────
status=0
if ((RUST)); then
    echo "── rust ─────────────────────────────────────────────────────────────"
    # shellcheck disable=SC2086  # deliberately word-split into one arg per crate
    PACKAGED_FEATURES="$FEATURES" "$PACKAGED_DIR/rust.sh" "$STAGE" $rust_names || status=1
fi

if ((JS)); then
    echo "── js ───────────────────────────────────────────────────────────────"
    # shellcheck disable=SC2086  # deliberately word-split into one arg per package
    "$PACKAGED_DIR/js.sh" "$STAGE" $js_names || status=1
fi

((status == 0)) || die "a consumer rejected a candidate; see above"
echo
echo "packaged: every candidate built and ran outside the workspace."
