# [S] @moq/watch follows the route that serves its path

## Goal

When the exact route for a path ends while a covering prefix (such as
`pool`) still serves it, `@moq/watch` moves to the prefix instead of going
offline, as Rust's `moq play` does through the shared follow helper.

## Plan

Found in #5154 (broadcast-epoch apps), decided 2026-10-09:
`Broadcast.#runBroadcast` in `js/watch` ignores `End`, so the player goes
offline even though a covering prefix still announces the path. Start after
#5154 lands, since it adds the helper this quest uses.

- Decided: use the shared follow helper. #5154 moves it from
  `moq_mux::Source::follow` into moq-net (`origin::Consumer::follow`) and
  mirrors it in `@moq/net`, per the cross-package table; this quest wires
  `@moq/watch`'s `Broadcast` onto the JS one. The serving route is the most
  specific one covering the path; another route taking over is a `Restart`,
  or an `Update` when both carry the same epoch; routes beneath the path are
  ignored. Rejected: a private copy in `@moq/watch`, and keeping the Rust
  helper in moq-mux (maintainer, 2026-10-09: a net-layer concern that should
  mirror across languages).
- Test: a path served both exactly and by a `pool` prefix; ending the exact
  route moves playback to the prefix as a restart, and ending both goes
  offline.

Public API: none. Wire: none.
