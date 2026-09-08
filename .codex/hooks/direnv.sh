#!/usr/bin/env bash
# Capture the project's direnv/Nix dev shell for Codex shell commands. Codex
# runs each command in a fresh Bash process, which sources this snapshot through
# BASH_ENV as configured in .codex/config.toml.

set -eu

command -v direnv >/dev/null 2>&1 || exit 0

# Require an explicit project dir so we never approve/export a stray .envrc from
# some unrelated working directory.
project_dir="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
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

env_file="$PWD/.direnv/codex-env.sh"
env_tmp="$env_file.tmp"
trap 'rm -f "$env_tmp"' EXIT

# Codex only needs the flake dev shell environment, so avoid depending on
# nix-direnv's remote bootstrap when Nix can emit the shell exports directly.
if command -v nix >/dev/null 2>&1 && [ -f flake.nix ]; then
    if nix --extra-experimental-features 'nix-command flakes' --accept-flake-config \
        print-dev-env --profile "$PWD/.direnv/codex-profile" .#default \
        >"$env_tmp" 2>"$PWD/.direnv/codex-hook.log"; then
        mv "$env_tmp" "$env_file"
        exit 0
    fi
fi

# Clear any inherited direnv state so `export` recomputes the full diff. Without
# this, a stale DIRENV_DIFF from the parent process makes direnv assume the env
# is already loaded and emit nothing.
unset DIRENV_DIR DIRENV_DIFF DIRENV_WATCHES DIRENV_FILE DIRENV_LAYOUT

direnv allow . 2>>"$PWD/.direnv/codex-hook.log"
direnv export bash >"$env_tmp" 2>>"$PWD/.direnv/codex-hook.log"
mv "$env_tmp" "$env_file"

exit 0
