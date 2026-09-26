#!/usr/bin/env bash
# The shared and quest skills in .claude/skills are symlinks into the
# .claude/shared and .quest submodules. git leaves those unpopulated in a
# new worktree, and a dangling symlink is silently skipped. A checkout on the
# wrong commit stays stale too.

set -eu

# Require an explicit project dir so we never touch a stray checkout.
[ -n "${CLAUDE_PROJECT_DIR:-}" ] || exit 0
cd "$CLAUDE_PROJECT_DIR" || exit 0

for path in .claude/shared .quest; do
    # '-' is uninitialized. '+' is populated at a commit other than the gitlink.
    # A matching checkout has no prefix.
    case "$(git submodule status "$path" 2>/dev/null)" in
        -* | +*) ;;
        *) continue ;;
    esac

    echo "submodule hook: updating $path to the pinned commit." >&2
    git submodule update --init "$path" >&2
done
