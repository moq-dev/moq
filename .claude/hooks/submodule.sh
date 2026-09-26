#!/usr/bin/env bash
# The shared skills in .claude/skills are symlinks into the .claude/shared
# submodule. git leaves submodules unpopulated in a new worktree, and a dangling
# skill symlink is silently skipped rather than reported, so populate it here.

set -eu

# Require an explicit project dir so we never touch a stray checkout.
[ -n "${CLAUDE_PROJECT_DIR:-}" ] || exit 0
cd "$CLAUDE_PROJECT_DIR" || exit 0

# `git submodule status` prefixes an uninitialized entry with '-'. Any other
# output means it is already populated, which is every session after the first.
case "$(git submodule status .claude/shared 2>/dev/null)" in
    -*) ;;
    *) exit 0 ;;
esac

echo "submodule hook: populating .claude/shared, the shared skills live there." >&2
git submodule update --init .claude/shared >&2
