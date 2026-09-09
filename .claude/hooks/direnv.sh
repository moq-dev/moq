#!/usr/bin/env bash
# Load the project's direnv/nix dev shell into the Claude Code session so Bash
# tool commands resolve flake-pinned tools (bun, nixfmt, ...) instead of system
# ones. No-op for anyone without direnv or an .envrc, so non-nix setups are
# unaffected.
#
# Claude Code inlines $CLAUDE_ENV_FILE into the command string of every Bash
# call, and Linux caps a single argv entry at MAX_ARG_STRLEN, 128KB. This dev
# shell exports around 190KB, so the environment goes in a sidecar file and
# $CLAUDE_ENV_FILE gets only a line sourcing it. macOS has no per-argument cap,
# which is why inlining the whole environment appeared to work there.

set -u

MARKER='# moq dev shell'

# Nothing to export into without this; older Claude Code versions won't set it.
[ -n "${CLAUDE_ENV_FILE:-}" ] || exit 0

command -v direnv >/dev/null 2>&1 || exit 0

# Require an explicit project dir so we never approve/export a stray .envrc from
# some unrelated working directory.
[ -n "${CLAUDE_PROJECT_DIR:-}" ] || exit 0
cd "$CLAUDE_PROJECT_DIR" || exit 0
[ -f .envrc ] || exit 0

# SessionStart fires again on resume and clear, and the env file outlives it, so
# an unguarded append would stack another copy each time.
grep -qF "$MARKER" "$CLAUDE_ENV_FILE" 2>/dev/null && exit 0

# Clear any inherited direnv state so the dev shell is recomputed in full.
# Without this, a stale DIRENV_DIFF from the parent process makes direnv assume
# the env is already loaded and emit nothing.
unset DIRENV_DIR DIRENV_DIFF DIRENV_WATCHES DIRENV_FILE DIRENV_LAYOUT

direnv allow . 2>/dev/null || exit 0

raw=$(mktemp) || exit 1
trap 'rm -f "$raw"' EXIT

# Read the environment with `env -0` rather than a shell builtin: nixpkgs builds
# the non-interactive `bash` without programmable completion, so `compgen` does
# not exist inside the very dev shell this hook is here to load.
if ! direnv exec . env -0 >"$raw"; then
	echo "direnv hook: could not load the dev shell, see the direnv output above." >&2
	exit 1
fi

sidecar=$CLAUDE_ENV_FILE.direnv

while IFS= read -r -d '' entry; do
	name=${entry%%=*}

	# Skip exported functions (BASH_FUNC_foo%%) and anything else that is not a
	# plain identifier, since `export` cannot restore those.
	[[ $name =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue

	# direnv's bookkeeping, and the shell's own state, are not ours to restore.
	case $name in
	DIRENV_* | PWD | OLDPWD | SHLVL | _) continue ;;
	esac

	printf 'export %s=%q\n' "$name" "${entry#*=}"
done <"$raw" >"$sidecar"

if [ ! -s "$sidecar" ]; then
	echo "direnv hook: the dev shell exported nothing, so no tools would resolve." >&2
	exit 1
fi

printf '%s\n. %q\n' "$MARKER" "$sidecar" >>"$CLAUDE_ENV_FILE"
