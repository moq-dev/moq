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

# expect <description> <actual> <expected>
expect() {
    if [[ "$2" == "$3" ]]; then
        echo "  ok: $1"
    else
        echo "  FAIL: $1 (expected '$3', got '$2')" >&2
        FAILED=1
    fi
}

# expect_tagged <description> <tags pointing at the mirror HEAD>
expect_tagged() {
    if [[ -n "$2" ]]; then
        echo "  ok: $1"
    else
        echo "  FAIL: $1 (HEAD carries no tag)" >&2
        FAILED=1
    fi
}

fail() {
    echo "  FAIL: $1" >&2
    FAILED=1
}

# A CI runner has no global git identity, and both a commit and an annotated tag
# refuse to be written without one, so give each scratch repo its own.
identify() {
    git -C "$1" config user.email test@example.com
    git -C "$1" config user.name "publish-wrapper test"
}

MIRROR="$WORK/mirror.git"
git init --quiet --bare --initial-branch=main "$MIRROR"
identify "$MIRROR"

# A mirror has to start somewhere: one empty commit so `git clone` has a HEAD.
SEED="$WORK/seed"
git init --quiet --initial-branch=main "$SEED"
identify "$SEED"
git -C "$SEED" commit --quiet --allow-empty -m "seed"
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

tags() { git -C "$MIRROR" tag --list "v${LINE}.*" | sort | tr '\n' ' '; }
head_tags() { git -C "$MIRROR" tag --points-at "refs/heads/main" | sort | tr '\n' ' '; }

echo "publish-wrapper test: first release"
publish >"$WORK/run1.log" 2>&1 || {
    cat "$WORK/run1.log" >&2
    fail "first publish exited non-zero"
}
expect "cut v${LINE}.0" "$(tags)" "v${LINE}.0 "
expect_tagged "tagged the pushed HEAD" "$(head_tags)"

echo "publish-wrapper test: unchanged tree is a no-op"
publish >"$WORK/run2.log" 2>&1 || {
    cat "$WORK/run2.log" >&2
    fail "second publish exited non-zero"
}
if grep -q "Nothing to publish" "$WORK/run2.log"; then
    echo "  ok: reported nothing to publish"
else
    fail "expected a no-op"
fi
expect "burned no patch number" "$(tags)" "v${LINE}.0 "

echo "publish-wrapper test: recovers a release whose tag never landed"
# Exactly what a failed tag push leaves behind: the tree is on main, untagged.
git -C "$MIRROR" tag --delete "v${LINE}.0" >/dev/null
publish >"$WORK/run3.log" 2>&1 || {
    cat "$WORK/run3.log" >&2
    fail "recovery publish exited non-zero"
}
expect "re-cut v${LINE}.0 for the existing HEAD" "$(tags)" "v${LINE}.0 "
expect_tagged "the tag points at the published HEAD" "$(head_tags)"

echo "publish-wrapper test: recognises an annotated tag"
# The publisher writes lightweight tags, but a hand-cut one is annotated, and
# ls-remote then lists it twice: the tag object, and the peeled commit.
git -C "$MIRROR" tag --delete "v${LINE}.0" >/dev/null
git -C "$MIRROR" tag --annotate --message "by hand" "v${LINE}.0" refs/heads/main
publish >"$WORK/run4.log" 2>&1 || {
    cat "$WORK/run4.log" >&2
    fail "publish against an annotated tag exited non-zero"
}
if grep -q "Nothing to publish" "$WORK/run4.log"; then
    echo "  ok: read the peeled tag as released"
else
    fail "did not recognise the annotated tag; would have minted a duplicate"
fi
expect "left the annotated tag alone" "$(tags)" "v${LINE}.0 "

echo "publish-wrapper test: leaves a HEAD it did not write alone"
# Only a stranded publisher commit is ours to tag. Anything else with a
# matching tree gets a warning, not a version.
git -C "$MIRROR" tag --delete "v${LINE}.0" >/dev/null
git -C "$SEED" fetch --quiet "$MIRROR" main
git -C "$SEED" checkout --quiet FETCH_HEAD
git -C "$SEED" commit --quiet --allow-empty -m "a human was here"
git -C "$SEED" push --quiet --force "$MIRROR" HEAD:refs/heads/main
publish >"$WORK/run5.log" 2>&1 || {
    cat "$WORK/run5.log" >&2
    fail "publish against an unrelated HEAD exited non-zero"
}
expect "minted no version for it" "$(tags)" ""
if grep -q "is not a release commit" "$WORK/run5.log"; then
    echo "  ok: said why it stopped"
else
    fail "no warning explaining the skip"
fi

if ((FAILED)); then
    echo "publish-wrapper test: FAILED" >&2
    exit 1
fi
echo "publish-wrapper test: ok"
