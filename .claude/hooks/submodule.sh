#!/usr/bin/env bash
# The shared skills in .claude/skills are symlinks into the .claude/shared
# submodule. git leaves that unpopulated in a new worktree, and a dangling
# symlink is silently skipped. A checkout on the wrong commit stays stale too.

set -eu

# Require an explicit project dir so we never touch a stray checkout.
[ -n "${CLAUDE_PROJECT_DIR:-}" ] || exit 0
cd "$CLAUDE_PROJECT_DIR" || exit 0

# '-' is uninitialized. '+' is populated at a commit other than the gitlink.
# A matching checkout has no prefix.
case "$(git submodule status .claude/shared 2>/dev/null)" in
    -*|+*) ;;
    *) exit 0 ;;
esac

echo "submodule hook: updating .claude/shared to the pinned skills." >&2
git submodule update --init .claude/shared >&2
