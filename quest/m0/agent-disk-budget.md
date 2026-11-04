# [S] Agent build output can't fill the desktop's disk

## Goal

The desktop host's agent checkouts reclaim their own build output, so parallel
agent builds don't fill the root disk and nobody has to run `mbx clean` by
hand. Done when a finished T3 or Claude Code checkout's managed target is
reclaimed automatically, without touching a checkout whose owner is still
using its outputs, and the PR records the free space over a busy day of
parallel agents.

## Plan

Planned 2026-11-04 with the maintainer, after the desktop's root disk filled
three times on 2026-11-04: to 244 MB, then twice to 5-14 GB while ten iterate
agents ran. Each time a human-run cleanup (`mbx gc`, `mbx clean`, worktree
removal, nix gc) freed the space.

What holds the space (measured 2026-11-04):

- `~/.cache/mbx/targets` held 430 GiB of managed targets for about 420
  checkouts, with single checkouts at 10-43 GiB. `mbx cache projects` attributes
  nearly all of it to `~/worktrees/moq/t3-*` T3 thread worktrees, about 740 of
  which existed. Cleaning the targets of those idle over six hours freed about
  150 GiB.
- The dotfiles `mbx/config.toml` sets `target.keep = ["/"]` and
  `gc.min_free_size = "none"` on purpose (see dotfiles `docs/mbx.md`): a target
  is kept while its checkout exists, because directly launched binaries hold no
  mbx lease. So a target is only reclaimed when its checkout is removed.
- Dotfiles' `worktree-clean.service` removes finished checkouts, but it runs
  only when started by hand and scans `~/work`, `~/.codex/worktrees`,
  `~/.worktrees` and `/tmp`, not `~/worktrees`, where T3 creates its checkouts.
  Most T3 worktrees there sit at a base commit or an unmerged branch, so even
  the merged-PR rule finds few: on 2026-11-04 only 2 of 742 were at a merged PR
  head.

The work lands mostly in [kixelated/dotfiles](https://github.com/kixelated/dotfiles)
(cleaner scope, a schedule, mbx retention), and possibly in how T3 retires
thread worktrees. Keep the retention guarantee `docs/mbx.md` gives: never
reclaim a target whose owner is still using it. Find the signal that a T3 or
Claude Code checkout is finished (its thread or task ended, its PR merged or
closed, no process using it) and reclaim on that, rather than on idle time
alone. Also check `/tmp` test scratch, which filled the disk on 2026-11-04
before the reboot.

Public API: none. Wire: none.
