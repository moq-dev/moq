# [M] Track-state reads skip when nothing changed

## Goal

`Cursor::sync`, `Cursor::probe` and `GroupExpiry::is_expired` skip their
track-state reads when the track's version is unchanged, and fanout's mutex wait
falls with them. `Subscriber::poll_recv_group` and `poll_recv_datagram` then
move onto a `kio::Watch`. The watch resets when the tail-grace deadline arms
or the floor moves.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Found by [moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532). With
`poll_finished` on the watch, `Cursor::sync`'s track-state read is the largest
fanout wait (52% at 16 workers), followed by `probe`.

- First make sure each read depends only on state that bumps the version.
  `poll_recv_group` reads the wall clock (a copy's tail grace), and its floor
  changes without a `Ready`, so it moves only with explicit resets.
- Measure fanout and chat with `just bench-profile lock` against main, at 4
  and 16 workers, on a quiet host. #7532 couldn't capture 16-worker chat.
