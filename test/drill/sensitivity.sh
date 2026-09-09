#!/usr/bin/env bash
# Prove the failure drills are sensitive: remove the behavior each one grades and
# require it to fail.
#
# A green drill on its own says nothing. It might be green because the recovery
# it covers works, or because it never looked. This runs each drill twice: once
# against this checkout, where it must pass, and once against a DISPOSABLE COPY
# of the tree with one recovery behavior removed, where it must fail with the
# reason the mutation names.
#
# Nothing here writes to the checkout it runs from. The mutation is applied to a
# copy under a temporary directory, which is deleted afterwards unless --keep.
#
# Each mutation is a patch in mutations/ with two headers:
#
#   # drill:  the test that must fail once it is applied
#   # expect: a substring the failure output must contain
#
# A mutated tree that fails to COMPILE is not a pass here: a compile error proves
# the patch touched something, not that the drill was watching. Same for a drill
# that fails for a reason other than the one its mutation names.
set -euo pipefail

DRILL_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DRILL_DIR/../.." && pwd)
MUTATIONS="$DRILL_DIR/mutations"

# The dev shell puts mbx's Cargo shim ahead of the pinned toolchain, so the
# plain name is already the compile-caching wrapper that keeps the snapshot
# build from being a cold build of the world.
CARGO=cargo

KEEP=0
BASELINE=1
SELECTED=()
dir=
log=

cleanup() {
    if [[ $KEEP -eq 0 ]]; then
        [[ -z "$dir" ]] || rm -rf "$dir"
        [[ -z "$log" ]] || rm -f "$log"
    fi
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Read a `# key: value` header out of a patch. The headers sit above the diff,
# where `patch` ignores them.
header() {
    sed -n "s/^# $2: *//p" "$1" | head -1
}

# What each mutation claims to break.
list() {
    for patch in "$MUTATIONS"/*.patch; do
        printf '%-40s breaks %s\n' "$(basename "$patch" .patch)" "$(header "$patch" drill)"
    done
}

usage() {
    cat <<'EOF'
Usage: sensitivity.sh [options] [mutation...]

Options:
  --list           list the mutations and the drill each one must break
  --keep           keep the mutated snapshots (prints each path)
  --no-baseline    skip the unmutated run of each drill
  -h, --help       this

With no mutation named, every mutation runs.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --list)
            list
            exit 0
            ;;
        --keep)
            KEEP=1
            shift
            ;;
        --no-baseline)
            BASELINE=0
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            echo "error: unknown option $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            SELECTED+=("$1")
            shift
            ;;
    esac
done

for want in "${SELECTED[@]}"; do
    if [[ ! -f "$MUTATIONS/$want.patch" ]]; then
        echo "error: unknown mutation $want" >&2
        list >&2
        exit 2
    fi
done

# Copy the tree as it stands (tracked plus untracked-but-not-ignored) into a
# fresh directory. Listing the files rather than copying the directory is what
# keeps target/, node_modules, and the rest of the ignored bulk out of the copy.
snapshot() {
    local dest="$1"
    (cd "$WORKSPACE" && git ls-files -z --cached --others --exclude-standard | tar -cf - --null -T -) |
        tar -xf - -C "$dest"
}

# Run one drill under nextest's process-level timeout, writing its output to
# `log`, and print the exit status.
run_drill() {
    local dir="$1" drill="$2" log="$3"
    local status=0
    (
        cd "$dir"
        "$CARGO" nextest run --locked -p moq-relay --test drills -E "test(=$drill)" --test-threads 1
    ) >"$log" 2>&1 || status=$?
    echo "$status"
}

failed=0
checked=0

for patch in "$MUTATIONS"/*.patch; do
    name=$(basename "$patch" .patch)
    if [[ ${#SELECTED[@]} -gt 0 ]]; then
        found=0
        for want in "${SELECTED[@]}"; do
            [[ "$want" == "$name" ]] && found=1
        done
        [[ $found -eq 1 ]] || continue
    fi

    drill=$(header "$patch" drill)
    expect=$(header "$patch" expect)
    if [[ -z "$drill" || -z "$expect" ]]; then
        echo "$name: missing a '# drill:' or '# expect:' header" >&2
        failed=$((failed + 1))
        continue
    fi

    checked=$((checked + 1))
    echo "=== $name -> $drill"

    if [[ $BASELINE -eq 1 ]]; then
        log=$(mktemp "${TMPDIR:-/tmp}/drill-baseline.XXXXXX")
        status=$(run_drill "$WORKSPACE" "$drill" "$log")
        if [[ "$status" != 0 ]]; then
            echo "  FAIL: $drill does not pass unmutated, so it can prove nothing" >&2
            tail -20 "$log" >&2
            failed=$((failed + 1))
            rm -f "$log"
            continue
        fi
        echo "  baseline: $drill passes"
        rm -f "$log"
    fi

    dir=$(mktemp -d "${TMPDIR:-/tmp}/moq-drill.XXXXXX")
    log=
    snapshot "$dir"
    # The snapshot is a plain directory with no repository metadata of its own,
    # and it stays that way: nothing here can reach the developer's checkout.
    if ! patch -p1 -d "$dir" --batch --forward --silent <"$patch"; then
        echo "  FAIL: '$name' does not apply to this tree" >&2
        failed=$((failed + 1))
        cleanup
        dir=
        continue
    fi

    log=$(mktemp "${TMPDIR:-/tmp}/drill-mutated.XXXXXX")
    status=$(run_drill "$dir" "$drill" "$log")

    if [[ "$status" == 0 ]]; then
        echo "  FAIL: $drill still passed with '$name' applied; it is not watching this behavior" >&2
        failed=$((failed + 1))
    elif grep -qE 'error\[[A-Z0-9]+\]: |could not compile' "$log"; then
        echo "  FAIL: '$name' broke the build, which is not behavioral proof" >&2
        grep -E 'error\[[A-Z0-9]+\]: ' "$log" | head -5 >&2
        failed=$((failed + 1))
    elif ! grep -qE "(FAIL|TIMEOUT).*moq-relay::drills $drill" "$log"; then
        echo "  FAIL: '$name' never got as far as running $drill" >&2
        tail -20 "$log" >&2
        failed=$((failed + 1))
    elif ! grep -qF "$expect" "$log"; then
        echo "  FAIL: $drill failed for the wrong reason; expected: $expect" >&2
        grep -A 2 'panicked at' "$log" | head -10 >&2
        failed=$((failed + 1))
    else
        echo "  sensitive: $drill fails with '$expect'"
    fi

    if [[ $KEEP -eq 1 ]]; then
        echo "  snapshot: $dir"
        echo "  log:      $log"
    else
        cleanup
        dir=
        log=
    fi
done

if [[ $checked -eq 0 ]]; then
    echo "error: no mutations selected" >&2
    exit 2
fi

if [[ $failed -gt 0 ]]; then
    echo "$failed of $checked mutations did not prove sensitivity" >&2
    exit 1
fi

echo "$checked of $checked drills fail when their recovery behavior is removed"
