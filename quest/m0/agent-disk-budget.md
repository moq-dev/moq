# [XS] Agent build output can't fill the desktop's disk

## Goal

The desktop host's agent checkouts reclaim their own build output, so parallel
agent builds don't fill the root disk and nobody has to run `mbx clean` by
hand. Done when the hourly cleaner from
[kixelated/dotfiles#41](https://github.com/kixelated/dotfiles/pull/41) runs on
the desktop and a PR records the root disk's free space over a busy day of
parallel agents.

## Plan

The cleaner is built: it reclaims a checkout's managed mbx target once its T3
thread, quest run, or Claude Code session has finished, and keeps anything live,
unknown, or held open by a process. Its design and the read-only classification
of 2026-11-05 (873 finished checkouts holding about 76.6 GiB of managed targets)
are in [moq-dev/moq#7960](https://github.com/moq-dev/moq/pull/7960).

Installed 2026-11-06: [kixelated/dotfiles#41](https://github.com/kixelated/dotfiles/pull/41)
merged at 16:35:00Z, and `worktree-clean.timer` last ran at 16:35:06Z
(checked 2026-11-06, /quest-unblock).

Now sample `df /` across a busy day, confirm
`worktree-clean.service`'s journal shows reclaimed targets and no live checkout
touched, and delete this quest in a PR recording both.
