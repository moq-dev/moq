# [XS] Agent build output can't fill the desktop's disk

## Goal

The desktop host's agent checkouts reclaim their own build output, so parallel
agent builds don't fill the root disk and nobody has to run `mbx clean` by
hand. Done when the hourly cleaner from
[kixelated/dotfiles#41](https://github.com/kixelated/dotfiles/pull/41) runs
cleanly on the desktop and a PR records the root disk's free space over a busy
day of parallel agents.

## Plan

The cleaner's design and the 2026-11-05 classification are in
[moq-dev/moq#7960](https://github.com/moq-dev/moq/pull/7960).

Evidence from `worktree-clean.service`'s journal, 2026-11-06 08:32 to
2026-11-07 08:10 PST (26 hourly runs, checked 2026-11-07). Read it with
`journalctl --user-unit worktree-clean.service`; `journalctl --user -u` reads a
stale journal file and prints nothing.

- 53 managed targets reclaimed. Two moq.pro checkouts were reclaimed twice
  because their threads resumed after finishing and rebuilt their targets.
- Live checkouts were kept: 869 "agent checkout is not finished" and 56
  "active process uses" keeps, and no reclaim of a live checkout.
- `mbx gc --max-size 25GiB` evicted 68.2, 36.0, 0.7 and 18.2 GiB in the runs
  that had something over budget.
- `df /` at 2026-11-07 08:14 PST, with about 20 agents just started: 1.9T,
  1.3T used, 604G free (68%). No earlier sample exists: neither the cleaner nor
  anything else on the host logs free space.

Every run exits 1 ("some cleanup operations failed") on the same four
checkouts, so the unit shows as failed and a real failure would be hidden:

- `git worktree remove` on `~/worktrees/moq/t3-522b4ead` fails with "working
  trees containing submodules cannot be moved or removed". The cleaner needs
  `--force` after it has verified the checkout, or to deinit submodules first.
- `git rev-list --all --reflog HEAD --not --remotes` exits 128 in the
  `.claude/shared` submodule of three moq.pro checkouts (`t3-46ce7785`,
  `t3-01aae7e2`, `.claude/worktrees/agent-a01261628f9e3b674`). The cleaner
  should keep such a checkout and report it as a keep, not a failure.

Fix both in kixelated/dotfiles `clean.sh`, and have each run log `df /` so the
journal holds the free-space record. Then record a busy day's samples here and
delete this quest.

## Waiting

- desktop: a busy day of parallel agents passes after the cleaner logs `df /`
  each run; check with `journalctl --user-unit worktree-clean.service | grep -A1 'df /'`
