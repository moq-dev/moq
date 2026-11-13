# [M] Track-state reads skip when nothing changed

## Goal

`Cursor::probe` and `GroupExpiry::is_expired` skip their track-state reads
when the track's version is unchanged, and fanout's mutex wait falls with
them. `Subscriber::poll_recv_group` then moves onto a `kio::Watch`. The watch
resets when the tail-grace deadline arms or the floor moves. Each part lands
only if a fresh lock profile still shows its wait.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Found by [moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532). With
`poll_finished` on the watch, `Cursor::sync`'s track-state read was the largest
fanout wait (52% at 16 workers), followed by `probe`.

Since then, [moq-dev/moq#7493](https://github.com/moq-dev/moq/pull/7493)
made `Cursor::sync` (`rs/moq-net/src/model/track.rs`) return before locking
the track once its window is settled, and
[moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605) made a live
reader's `probe` skip the fills and the subscription when it misses nothing.
`probe` still takes the track lock once for its judgement.

- Re-profile first: `just bench-profile lock` against main, fanout and chat at
  4 and 16 workers, on a quiet host. #7532 couldn't capture 16-worker chat.
  Drop any part below whose wait no longer shows.
- Make sure each read depends only on state that bumps the version.
  `poll_recv_group` reads the wall clock (a copy's tail grace), and its floor
  changes without a `Ready`, so it moves only with explicit resets.
