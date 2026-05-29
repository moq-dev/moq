#!/usr/bin/env bash
# Load the project's direnv/nix dev shell into the Claude Code session so Bash
# tool commands resolve flake-pinned tools (bun, nixfmt, ...) instead of system
# ones. No-op for anyone without direnv or an .envrc, so non-nix setups are
# unaffected.
#
# direnv only *approves* the .envrc; the actual env is exported into
# $CLAUDE_ENV_FILE, which Claude Code sources for every later Bash command.

set -u

# Nothing to export into without this; older Claude Code versions won't set it.
[ -n "${CLAUDE_ENV_FILE:-}" ] || exit 0

command -v direnv >/dev/null 2>&1 || exit 0

# Require an explicit project dir so we never approve/export a stray .envrc from
# some unrelated working directory.
[ -n "${CLAUDE_PROJECT_DIR:-}" ] || exit 0
cd "$CLAUDE_PROJECT_DIR" || exit 0
[ -f .envrc ] || exit 0

# Clear any inherited direnv state so `export` recomputes the full diff. Without
# this, a stale DIRENV_DIFF from the parent process makes direnv assume the env
# is already loaded and emit nothing.
unset DIRENV_DIR DIRENV_DIFF DIRENV_WATCHES DIRENV_FILE DIRENV_LAYOUT

direnv allow . 2>/dev/null
direnv export bash >>"$CLAUDE_ENV_FILE" 2>/dev/null

exit 0
