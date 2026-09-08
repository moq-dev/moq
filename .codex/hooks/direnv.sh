#!/usr/bin/env bash
# Codex's entry point into the shared session setup. Codex shell commands run in
# fresh processes, so the hook atomically replaces the worktree snapshot that
# BASH_ENV sources rather than appending to a one-process session environment.

set -u

project_dir="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
export CODEX_PROJECT_DIR="$project_dir"
export CODEX_ENV_FILE="$project_dir/.direnv/codex-env.sh"
export MOQ_SESSION_ENV_REPLACE=1

exec "$project_dir/scripts/session-setup.sh" "$@"
