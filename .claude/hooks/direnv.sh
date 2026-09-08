#!/usr/bin/env bash
# Claude Code's entry point into the shared session setup. The implementation is
# in scripts/session-setup.sh so Claude Code and Codex cannot drift apart; a copy
# that fell behind would be the one whose sessions keep failing silently.
exec "$(cd "$(dirname "$0")/../.." && pwd)/scripts/session-setup.sh" "$@"
