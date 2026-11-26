# [M] The room discovers participants without opening players

## Goal

An application with its own renderer uses `@moq/room` to discover a room's
participants, their kinds and paths, without the room opening a
`Watch.Player` for every member.

## Plan

Decided 2026-11-26 (maintainer, /quest-plan of moq-dev/hang.live's
follow-ups; approved during
[moq-dev/hang.live#43](https://github.com/moq-dev/hang.live/pull/43)).
Today `Remote` builds a `Member` for each announced camera or screen, and
`Member` always constructs a `Watch.Player` with a canvas
(`js/room/src/remote.ts`). Hang renders tiles through its own WebGL pipeline,
so it rewrote discovery instead of using `Room`.

- Recommended: split discovery out of `Room` into an exported roster (the
  announced identities, each with its live camera/screen paths and kinds)
  that `Room` builds its `Remote`s on. An application that renders on its
  own uses the roster alone (rejected: a headless `Member` flag, which would
  still construct the metadata consumer and player wiring per member).
- Keep `Room`'s current behavior and API for applications that use its
  players.
- Cover alias announce/withdraw and reconnect through the roster in tests.
  [Room keeps a participant while one of its aliases is
  announced](/quest/m3/room-alias-withdraw.md) and [room participants
  survive your own blip](/quest/m3/room-keep-through-blip.md) change the same
  attach and detach code; whichever lands second adapts.

Public API: one new export from `@moq/room`, not breaking. Wire: none.
