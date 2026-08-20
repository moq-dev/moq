#!/usr/bin/env bash
set -euo pipefail

# Exercises publish-wrapper.sh against a scratch bare repo standing in for the
# mirror, via GO_MIRROR_URL. No network, no cargo, no bindings: this is about
# the publish state machine, which the rest of `just go check` never reaches.
#
# The case worth having a test for is recovery. Publishing lands a branch and a
# tag, and the patch number is derived only when the staged tree differs from
# the mirror, so a tag that failed to land used to strand that content: the
# retry matched HEAD, said "nothing to publish", and never asked which tag was
# missing.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

if ! command -v go >/dev/null 2>&1; then
    echo "publish-wrapper test: no go on PATH, skipping" >&2
    exit 0
fi
if ! command -v rsync >/dev/null 2>&1; then
    echo "publish-wrapper test: no rsync on PATH, skipping" >&2
    exit 0
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

FAILED=0
ok() { echo "  ok: $1"; }
fail() {
    echo "  FAIL: $1" >&2
    FAILED=1
}

MIRROR="$WORK/mirror.git"
git init --quiet --bare --initial-branch=main "$MIRROR"

# A mirror has to start somewhere: one empty commit so `git clone` has a HEAD.
SEED="$WORK/seed"
git init --quiet --initial-branch=main "$SEED"
git -C "$SEED" -c user.email=t@example.com -c user.name=t commit --quiet --allow-empty -m "seed"
git -C "$SEED" push --quiet "$MIRROR" HEAD:refs/heads/main

LINE=$(tr -d '[:space:]' <"$GO_DIR/wrapper/VERSION")

# Stage a real wrapper tarball. --skip-tidy keeps it offline (no go.sum), which
# is fine: publish-wrapper.sh only reads VERSION out of the staged tree.
bash "$GO_DIR/scripts/package-wrapper.sh" \
    --line "$LINE" \
    --ffi-version "0.0.0-test" \
    --source-dir "$GO_DIR/wrapper" \
    --output "$WORK/staged" \
    --skip-tidy >"$WORK/package.log" 2>&1 || {
    cat "$WORK/package.log" >&2
    echo "publish-wrapper test: packaging failed" >&2
    exit 1
}

mkdir -p "$WORK/run/go-out"
cp "$WORK/staged/moq-go-${LINE}-wrapper.tar.gz" "$WORK/run/go-out/"

publish() {
    (
        cd "$WORK/run"
        GO_MIRROR_URL="$MIRROR" GO_MIRROR_TOKEN=unused \
            bash "$GO_DIR/scripts/publish-wrapper.sh"
    )
}

tags() { git -C "$MIRROR" tag --list "v${LINE}.*" | sort; }
head_tags() { git -C "$MIRROR" tag --points-at "refs/heads/main" | sort; }

echo "publish-wrapper test: first release"
publish >"$WORK/run1.log" 2>&1 || {
    cat "$WORK/run1.log" >&2
    fail "first publish exited non-zero"
}
[[ "$(tags)" == "v${LINE}.0" ]] && ok "cut v${LINE}.0" || fail "expected v${LINE}.0, got: $(tags | tr '\n' ' ')"
[[ -n "$(head_tags)" ]] && ok "tagged the pushed HEAD" || fail "HEAD carries no tag"

echo "publish-wrapper test: unchanged tree is a no-op"
publish >"$WORK/run2.log" 2>&1 || {
    cat "$WORK/run2.log" >&2
    fail "second publish exited non-zero"
}
grep -q "Nothing to publish" "$WORK/run2.log" && ok "reported nothing to publish" || fail "expected a no-op"
[[ "$(tags)" == "v${LINE}.0" ]] && ok "burned no patch number" || fail "unexpected tags: $(tags | tr '\n' ' ')"

echo "publish-wrapper test: recovers a release whose tag never landed"
# Exactly what a failed tag push leaves behind: the tree is on main, untagged.
git -C "$MIRROR" tag --delete "v${LINE}.0" >/dev/null
publish >"$WORK/run3.log" 2>&1 || {
    cat "$WORK/run3.log" >&2
    fail "recovery publish exited non-zero"
}
[[ "$(tags)" == "v${LINE}.0" ]] && ok "re-cut v${LINE}.0 for the existing HEAD" || fail "expected v${LINE}.0 restored, got: $(tags | tr '\n' ' ')"
[[ -n "$(head_tags)" ]] && ok "the tag points at the published HEAD" || fail "restored tag is not on HEAD"

if ((FAILED)); then
    echo "publish-wrapper test: FAILED" >&2
    exit 1
fi
echo "publish-wrapper test: ok"
