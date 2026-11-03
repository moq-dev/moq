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
