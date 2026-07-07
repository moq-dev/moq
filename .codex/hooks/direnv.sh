#!/usr/bin/env bash
# Load the project's direnv/Nix dev shell into the agent session so shell tool
# commands resolve flake-pinned tools (just, bun, nixfmt, ...) instead of system
# ones. No-op for anyone without direnv or an .envrc, so non-Nix setups are
# unaffected.
#
# direnv only approves the .envrc; the actual env is exported into the session
# env file, which the agent sources for later shell commands.

set -u

env_file="${CODEX_ENV_FILE:-${CLAUDE_ENV_FILE:-}}"

# Nothing to export into without this; older agent versions won't set it.
[ -n "$env_file" ] || exit 0

command -v direnv >/dev/null 2>&1 || exit 0

# Require an explicit project dir so we never approve/export a stray .envrc from
# some unrelated working directory.
project_dir="${CODEX_PROJECT_DIR:-${CLAUDE_PROJECT_DIR:-${PWD:-}}}"
[ -n "$project_dir" ] || exit 0
cd "$project_dir" || exit 0
[ -f .envrc ] || exit 0

# Keep direnv's config and allow/deny state inside the worktree. Codex sessions
# usually cannot write to ~/.config/direnv or ~/.local/share/direnv while sandboxed.
export XDG_CONFIG_HOME="$PWD/.direnv/config"
export XDG_CACHE_HOME="$PWD/.direnv/cache"
export XDG_DATA_HOME="$PWD/.direnv/share"
export DIRENV_CONFIG="$XDG_CONFIG_HOME/direnv"
mkdir -p "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"

# Codex only needs the flake dev shell environment, so avoid depending on
# nix-direnv's remote bootstrap when Nix can emit the shell exports directly.
if command -v nix >/dev/null 2>&1 && [ -f flake.nix ]; then
    nix_env="$(mktemp)"
    if nix print-dev-env --profile "$PWD/.direnv/codex-profile" .#default >"$nix_env" 2>"$PWD/.direnv/codex-hook.log"; then
        cat "$nix_env" >>"$env_file"
        rm -f "$nix_env"
        exit 0
    fi
    rm -f "$nix_env"
fi

# Clear any inherited direnv state so `export` recomputes the full diff. Without
# this, a stale DIRENV_DIFF from the parent process makes direnv assume the env
# is already loaded and emit nothing.
unset DIRENV_DIR DIRENV_DIFF DIRENV_WATCHES DIRENV_FILE DIRENV_LAYOUT

direnv allow . 2>>"$PWD/.direnv/codex-hook.log"
direnv export bash >>"$env_file" 2>>"$PWD/.direnv/codex-hook.log"

exit 0
