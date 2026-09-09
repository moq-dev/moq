#!/usr/bin/env bash
# Load the project's direnv/nix dev shell into the Claude Code session so Bash
# tool commands resolve flake-pinned tools (bun, nixfmt, ...) instead of system
# ones. No-op for anyone without direnv or an .envrc, so non-nix setups are
# unaffected.
#
# direnv only *approves* the .envrc; the actual env is exported into
# $CLAUDE_ENV_FILE, which Claude Code inlines into every later Bash command.

set -u

# Because it is inlined, the file counts against MAX_ARG_STRLEN, the 128KB Linux
# cap on a single argv entry. Refuse to write more than half of that: past the
# cap every Bash command dies with E2BIG before the shell even starts.
MAX_BYTES=65536
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
# an unguarded append stacks another full copy each time.
grep -qF "$MARKER" "$CLAUDE_ENV_FILE" 2>/dev/null && exit 0

# Clear any inherited direnv state so the dev shell is recomputed in full.
# Without this, a stale DIRENV_DIFF from the parent process makes direnv assume
# the env is already loaded and emit nothing.
unset DIRENV_DIR DIRENV_DIFF DIRENV_WATCHES DIRENV_FILE DIRENV_LAYOUT

direnv allow . 2>/dev/null || exit 0

# Snapshot the environment from inside the dev shell rather than using `direnv
# export`, which also emits DIRENV_DIFF and DIRENV_WATCHES: base64 images of the
# entire environment, hundreds of KB under a flake, that only direnv reads.
scratch=$(mktemp) || exit 0
trap 'rm -f "$scratch"' EXIT

direnv exec . bash -c '
	for name in $(compgen -e); do
		case $name in
		DIRENV_* | PWD | OLDPWD | SHLVL | _) continue ;;
		esac
		printf "export %s=%q\n" "$name" "${!name}"
	done
' >"$scratch" 2>/dev/null

size=$(wc -c <"$scratch")
[ "$size" -gt 0 ] || exit 0

if [ "$size" -gt "$MAX_BYTES" ]; then
    echo "direnv hook: dev shell env is $size bytes, over the $MAX_BYTES cap. Refusing to export it, since Bash would then fail with E2BIG." >&2
    exit 1
fi

{
    echo "$MARKER"
    cat "$scratch"
} >>"$CLAUDE_ENV_FILE"
