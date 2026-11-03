# [XS] quest init keeps moq's nix-wrapped worktree hooks

## Goal

moq's Claude Code worktree hooks, which run the `quest` moq pins through nix,
survive quest pin bumps: `quest init` leaves them alone instead of refusing
them.

## Plan

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7674](https://github.com/moq-dev/moq/pull/7674)): ✅ the hooks
reach `quest` through nix (or a submodule) long-term, so they run moq's pin.
A host-wide `quest` on `PATH` is acceptable only as a temporary bridge.
Rejected: relying on an installed `quest`, `direnv exec`, adopting the bare
`quest worktree hook` now, and dropping the quest.

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7674](https://github.com/moq-dev/moq/pull/7674)): ✅ ask
moq-dev/quest to have `quest init` treat any command hook whose arguments end
in `worktree hook` as its own. Rejected: a configured hook command, and not
asking.

[moq-dev/moq#7674](https://github.com/moq-dev/moq/pull/7674) installed the four
hooks as `nix run "${CLAUDE_PROJECT_DIR}#quest" -- worktree hook`. That command
takes 0.1–0.5 s warm and 1.5–3 s with a cold nix cache, and builds a new pin in
about 6 s. It also bumped the pin past
[moq-dev/quest#425](https://github.com/moq-dev/quest/pull/425), so
`--worktree '#123'` starts at the PR head and workflow agents' checkouts get
released, and checked live that the hooks coexist with T3 Code's worktree
threads and isolated delegates. What remains:

- `quest init` matches only the literal `quest worktree hook`, so it refuses a
  wrapped `WorktreeCreate`/`WorktreeRemove` command (exit 1, nothing written)
  and would add duplicate subagent hooks. Until quest recognizes a wrapped
  command, a pin bump drops the four entries for its `quest init` run and
  restores `.claude/settings.json` afterwards, as AGENTS.md says. Once
  quest recognizes them, bump the pin, check that `quest init` is a
  no-op here, and drop that AGENTS.md sentence.

## Waiting

- moq-dev/quest: land `quest init` and `quest uninstall` treating a command
  hook whose arguments end in `worktree hook` as Quest's own (being planned
  there as of 2026-11-03). Check: with this repository's hooks in place, the
  pinned `quest init` exits 0 and leaves `.claude/settings.json` unchanged.
