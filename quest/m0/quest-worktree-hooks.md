# [S] Agents use quest's worktree hooks

## Goal

moq's `.claude/settings.json` carries the `WorktreeCreate`, `WorktreeRemove`,
`SubagentStart` and `SubagentStop` hooks that `quest init` installs (running
`quest worktree hook`), so agent worktrees are created and cleaned up the way
quest expects, without breaking T3 Code's own worktree management.

## Plan

Planned 2026-11-02 (maintainer, /quest-unblock), from
[moq-dev/moq#7644](https://github.com/moq-dev/moq/pull/7644): the quest pin bump
left the hooks out because they change how agent worktrees are made here, and
moq never adopted them after moq-dev/quest#350.

- First confirm the hooks coexist with T3 Code's worktree threads and its
  isolated delegate checkouts (create, hand off, and clean up one of each with
  the hooks on). If they conflict, stop and report rather than adopting.
- Then add them with `quest init` and document the behavior in AGENTS.md.

Findings, 2026-11-02 ([moq-dev/moq#7674](https://github.com/moq-dev/moq/pull/7674)):

- The hook itself leaves T3 Code's checkouts alone. T3 makes its own
  (`~/worktrees/moq/t3-*` on `t3/*` branches) rather than going through
  Claude's `WorktreeCreate`. `SubagentStart`/`SubagentStop` act only on a
  registered checkout named `agent-<agent_id>`, and `WorktreeRemove` refuses a
  path quest doesn't own. A scratch run against a T3-style linked worktree
  confirmed each case.
- They still conflict today: hooks run with the `PATH` Claude Code started
  with, not the direnv dev shell, and T3 Code's `PATH` has no `quest`. With the
  hooks on, every isolated subagent, `--worktree`, and `EnterWorktree` fails
  with `quest: not found`. moq-dev/quest's own adoption,
  [moq-dev/quest#423](https://github.com/moq-dev/quest/pull/423), waits on the
  same install.
- Without
  [moq-dev/quest#425](https://github.com/moq-dev/quest/pull/425) in moq's pin,
  `--worktree '#123'` starts at `origin/HEAD` instead of the PR head, and
  workflow agents' checkouts are never released.
- Once both clear, run `quest init`, document the behavior and the `quest`
  install in AGENTS.md, and do the live T3 check above, plus a native
  `isolation: "worktree"` subagent inside a T3 session.

## Waiting

- Maintainer: install a reviewed `quest` on the `PATH` Claude Code and T3 Code
  start with on every host that runs agents here, for example
  `nix profile add github:moq-dev/quest` (`~/.nix-profile/bin` is already on
  T3 Code's `PATH`). Check from a shell outside the dev shell:
  `echo '{}' | quest worktree hook` prints `expected a Claude Code hook's JSON
  input`, not `command not found`.
- moq-dev/quest: land
  [moq-dev/quest#425](https://github.com/moq-dev/quest/pull/425), then bump
  moq's `quest` flake input past it.
