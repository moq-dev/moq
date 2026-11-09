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

The two causes of the hourly exit 1 are fixed in
[kixelated/dotfiles#54](https://github.com/kixelated/dotfiles/pull/54), a draft:
orphaned submodule metadata forces a single removal only after every repository
under it is checked, unreadable history keeps its checkout, and each run logs
`df /`. Once it merges and the desktop pulls it, the next run should log
`df /` and exit 0.

Evidence from `worktree-clean.service`'s journal, 2026-11-06 08:32 to
2026-11-08 22:15 PST (49 hourly runs). Read it with
`journalctl --user-unit worktree-clean.service`; `journalctl --user -u` reads a
stale journal file and prints nothing.

- 2026-11-06 to 11-07 (26 runs): 53 managed targets reclaimed. Two moq.pro
  checkouts were reclaimed twice because their threads resumed after finishing
  and rebuilt their targets.
- Live checkouts were kept: 869 "agent checkout is not finished" and 56
  "active process uses" keeps, and no reclaim of a live checkout.
- `mbx gc --max-size 25GiB` evicted 68.2, 36.0, 0.7 and 18.2 GiB in the runs
  that had something over budget.
- `df /` at 2026-11-07 08:14 PST, with about 20 agents just started: 1.9T,
  1.3T used, 604G free (68%). No earlier sample exists: neither the cleaner nor
  anything else on the host logs free space.
- 2026-11-08 (23 runs, 00:09 to 22:15): every run exits 1 on the same four
  checkouts, and no run logs `df /`. A live `df /` at about 2026-11-08 23:00 PST read
  190G free (90% used). This is not a busy-day sample under the protocol below.

The four failing checkouts, every run:

- `t3-522b4ead`: `git worktree remove` refused with "working trees containing
  submodules cannot be moved or removed", because orphaned submodule metadata
  remained in the worktree's git dir.
- `.claude/shared` in `t3-01aae7e2`, `t3-46ce7785` and
  `agent-a01261628f9e3b674`: `git rev-list` exits 128 because the submodule's
  HEAD is unborn.

Remaining after the dotfiles PR merges: record a busy day's samples here, then
delete this quest.

## Waiting

- Maintainer: review and merge [kixelated/dotfiles#54](https://github.com/kixelated/dotfiles/pull/54), then pull `~/work/dotfiles` on the desktop; the hourly cleaner runs `~/.local/bin/clean.sh`, which links there. Check: `journalctl --user-unit worktree-clean.service --since today | grep -A1 'df /'` shows a sample each run, and `grep failed:` finds nothing.
- desktop: a busy day of parallel agents passes after the cleaner logs `df /` each run; check with `journalctl --user-unit worktree-clean.service | grep -A1 'df /'`.
