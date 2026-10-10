# [S] @moq/watch follows the route that serves its path

## Goal

When the exact route for a path ends while a covering prefix (such as
`pool`) still serves it, `@moq/watch` moves to the prefix instead of going
offline, as Rust's `moq play` does through `moq_mux::Source::follow`.

## Plan

Found in #5154 (broadcast-epoch apps), decided 2026-10-09:
`Broadcast.#runBroadcast` in `js/watch` ignores `End`, so the player goes
offline even though a covering prefix still announces the path.

- Decided: mirror `Source::follow`'s reduction inside `@moq/watch`'s
  `Broadcast`. The serving route is the most specific one covering the path;
  another route taking over is a `Restart`, or an `Update` when both carry
  the same epoch; routes beneath the path are ignored. Rejected: a shared
  js/net helper, until a second JS consumer needs it.
- Test: a path served both exactly and by a `pool` prefix; ending the exact
  route moves playback to the prefix as a restart, and ending both goes
  offline.

Public API: none. Wire: none.
