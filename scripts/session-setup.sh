#!/usr/bin/env bash
# Load the project's direnv/Nix dev shell into the agent session so shell tool
# commands resolve flake-pinned tools (just, bun, nixfmt, ...) instead of system
# ones. No-op for anyone without direnv or an .envrc, so non-Nix setups are
# unaffected.
#
# direnv only approves the .envrc; the actual env is exported into the session
# env file, which the agent sources for later shell commands.
#
# Every exit reports what happened, in the session env as MOQ_SESSION_SETUP and
# on stdout for whoever is reading the hook. A hook that returns 0 having
# exported nothing is indistinguishable from one that worked, and the session it
# leaves behind fails later, in a build, as a missing tool nobody can explain.
# `just doctor` reads these two variables back.
#
# One implementation, invoked by every agent's own hook entry point, because the
# copy that drifts is the one whose sessions keep failing silently.

set -u

env_file="${CODEX_ENV_FILE:-${CLAUDE_ENV_FILE:-}}"
replace_env=${MOQ_SESSION_ENV_REPLACE:-}
output_file=$env_file
log=""

setup_error() {
    printf 'moq session setup: failed (%s)\n' "$1" >&2
    if [ -n "$replace_env" ] && [ -n "$output_file" ]; then
        trap - EXIT
        rm -f "$output_file" 2>/dev/null || true
    fi
    exit 1
}

# Quote a value for the env file, which is sourced. A checkout path with a space
# would otherwise become two words, and the second one a command.
shell_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

# Report the outcome and stop. Nothing here is fatal to the session: a session
# on the host toolchain still works, it just verifies less.
finish() {
    local result=$1 detail=$2
    if [ -n "$output_file" ]; then
        {
            printf 'export MOQ_SESSION_SETUP=%s\n' "$(shell_quote "$result")"
            printf 'export MOQ_SESSION_SETUP_LOG=%s\n' "$(shell_quote "${log:-none}")"
        } >>"$output_file" || setup_error "cannot write $output_file"
        if [ -n "$replace_env" ]; then
            mv "$output_file" "$env_file" || setup_error "cannot replace $env_file"
            trap - EXIT
        fi
    fi
    printf 'moq session setup: %s (%s)%s\n' "$result" "$detail" \
        "${log:+; log: $log}"
    exit 0
}

# Nothing to export into without this; older agent versions won't set it.
[ -n "$env_file" ] || exit 0

# Require an explicit project dir so we never approve/export a stray .envrc from
# some unrelated working directory.
project_dir="${CODEX_PROJECT_DIR:-${CLAUDE_PROJECT_DIR:-${PWD:-}}}"
[ -n "$project_dir" ] || finish host "no project directory"
cd "$project_dir" || finish host "cannot enter $project_dir"

if [ -n "$replace_env" ]; then
    mkdir -p "$PWD/.direnv" || setup_error "cannot create $PWD/.direnv"
    output_file="$env_file.tmp.$$"
    trap 'rm -f "$output_file"' EXIT
    : >"$output_file" || setup_error "cannot create $output_file"
fi

command -v direnv >/dev/null 2>&1 || finish host "direnv is not installed"
[ -f .envrc ] || finish host "no .envrc in $project_dir"

# Keep direnv's config and allow/deny state inside the worktree. Codex sessions
# usually cannot write to ~/.config/direnv or ~/.local/share/direnv while sandboxed.
export XDG_CONFIG_HOME="$PWD/.direnv/config"
export XDG_CACHE_HOME="$PWD/.direnv/cache"
export XDG_DATA_HOME="$PWD/.direnv/share"
export DIRENV_CONFIG="$XDG_CONFIG_HOME/direnv"
mkdir -p "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"

log="$PWD/.direnv/session-setup.log"
: >"$log"

# Codex only needs the flake dev shell environment, so avoid depending on
# nix-direnv's remote bootstrap when Nix can emit the shell exports directly.
if command -v nix >/dev/null 2>&1 && [ -f flake.nix ]; then
    nix_env="$(mktemp)"
    if nix --extra-experimental-features 'nix-command flakes' --accept-flake-config \
        print-dev-env --profile "$PWD/.direnv/codex-profile" .#default \
        >"$nix_env" 2>>"$log"; then
        cat "$nix_env" >>"$output_file" || setup_error "cannot write $output_file"
        rm -f "$nix_env"
        finish nix-dev-env "flake dev shell exported"
    fi
    rm -f "$nix_env"
    printf 'nix print-dev-env failed; falling back to direnv\n' >>"$log"
fi

# Clear any inherited direnv state so `export` recomputes the full diff. Without
# this, a stale DIRENV_DIFF from the parent process makes direnv assume the env
# is already loaded and emit nothing.
unset DIRENV_DIR DIRENV_DIFF DIRENV_WATCHES DIRENV_FILE DIRENV_LAYOUT

direnv allow . 2>>"$log" || finish direnv-failed "direnv could not approve .envrc"

exports="$(mktemp)"
if ! direnv export bash >"$exports" 2>>"$log"; then
    rm -f "$exports"
    finish direnv-failed "direnv export failed"
fi

# An export that produced nothing left the session on the host toolchain, which
# is a different outcome from one that loaded the shell, so say so.
if [ -s "$exports" ]; then
    cat "$exports" >>"$output_file" || setup_error "cannot write $output_file"
    rm -f "$exports"
    finish direnv "direnv exported the environment"
fi
rm -f "$exports"
finish direnv-empty "direnv exported nothing"
