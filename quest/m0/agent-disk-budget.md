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

Once it is installed, sample `df /` across a busy day, confirm
`worktree-clean.service`'s journal shows reclaimed targets and no live checkout
touched, and delete this quest in a PR recording both.

## Waiting

- Maintainer: merge and install [kixelated/dotfiles#41](https://github.com/kixelated/dotfiles/pull/41) on the desktop, since an agent session must not install over the live dotfiles or mass-delete targets; check that `systemctl --user list-timers` lists `worktree-clean.timer` with a last run
