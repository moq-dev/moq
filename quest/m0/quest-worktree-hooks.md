# [XS] quest init keeps moq's nix-wrapped worktree hooks

## Goal

moq's Claude Code worktree hooks run the `quest` moq pins, and they survive
quest pin bumps: `quest init` leaves them alone instead of refusing them, and
`--worktree '#123'` and workflow agents behave under them as without them.

## Plan

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7674](https://github.com/moq-dev/moq/pull/7674)): ✅ the hooks
reach `quest` through nix (or a submodule) long-term, so they run moq's pin.
A host-wide `quest` on `PATH` is acceptable only as a temporary bridge.
Rejected: relying on an installed `quest`, `direnv exec`, adopting the bare
`quest worktree hook` now, and dropping the quest.

[moq-dev/moq#7674](https://github.com/moq-dev/moq/pull/7674) installed the four
hooks as `nix run "${CLAUDE_PROJECT_DIR}#quest" -- worktree hook`. That command
takes 0.1–0.5 s warm and 1.5–3 s with a cold nix cache, and builds a new pin in
about 6 s. The PR checked live that it coexists with T3 Code's worktree threads
and isolated delegates. What remains:

- `quest init` matches only the literal `quest worktree hook`, so it refuses a
  wrapped `WorktreeCreate`/`WorktreeRemove` command (exit 1, nothing written)
  and would add duplicate subagent hooks. Until quest recognizes a wrapped
  command, a pin bump drops the four entries for its `quest init` run and
  restores `.claude/settings.json` afterwards, as AGENTS.md says. Once a
  quest release recognizes them, bump the pin, check that `quest init` is a
  no-op here, and drop that AGENTS.md sentence.
- Bump moq's `quest` input past
  [moq-dev/quest#425](https://github.com/moq-dev/quest/pull/425) (merged
  2026-11-03 as `53f78b9`), so `--worktree '#123'` starts at the PR head
  instead of `origin/HEAD` and workflow agents' checkouts get released.
  Refresh the skill stubs with the AGENTS.md drop-and-restore step, then
  check both routes live.

## Waiting

- Maintainer: ask moq-dev/quest to let `quest init` and `quest uninstall`
  recognize a hook command that wraps `quest worktree hook` in a launcher, such
  as `nix run <flake>#quest -- worktree hook`. Options: (a) recognize any
  command hook whose arguments end in `worktree hook` (recommended: no config,
  and `quest init` stays idempotent); (b) a configured hook command `quest init`
  writes and matches; (c) don't ask, and keep the drop-and-restore step on
  every pin bump.
