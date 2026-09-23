# [M] An announce consumer knows when it has caught up

## Goal

A Rust consumer of `origin::Consumer::announced()` is told, once and in
order, that the routes live at subscribe time have all been delivered, so
"list what is live" needs no guess. Today the stream replays the current
routes and continues live with no boundary, and moq-cli's `--broadcast`
completion (`rs/moq-cli/src/complete.rs`) stops on a 30ms settle / 500ms
budget timer instead.

## Plan

- `AnnounceConsumer::next()` yields an enum: each route update as today,
  then a single `Live` marker once the initial set has been delivered, then
  live updates. `Live` comes after every replayed update, so a caller that
  stops at it has seen the whole set. This changes a published return type,
  so the quest lands on `dev`.
- The fact is per source, but consumers read a merged origin, which today
  has no source registry. Each remote announce subscription (a session's
  subscriber, a cluster peer) holds a pending guard from the origin until
  its initial set has landed; in-process routes are replayed synchronously
  and are never pending. A cursor yields `Live` once every guard pending at
  subscribe time has cleared. A source that closes before clearing drops
  its guard, so a dead session cannot stall the marker. No cost while no
  guard is pending.
- A source clears on the wire's boundary: `AnnounceOk.active` on lite-05+,
  `AnnounceInit` on lite-01/02. Versions without one (lite-03/04, IETF)
  clear on a settle timer owned by the session, the one place it lives.
- Move completion onto `Live`; the timer survives only in the session for
  marker-less versions. Look at the other settle and deadline loops
  (`moq-bench` startup, relay cluster discovery, test `settle()` helpers)
  and move any that want the initial set.
- Test: a relay with N announced broadcasts yields `Live` after exactly N
  updates on each lite version that carries a count, an empty relay yields
  it immediately, and a session that closes before its count arrives does
  not block it.

Public API: breaking on moq-net (`next()` returns an enum). Wire: none. JS
parity is [a separate quest](/quest/m1/js-announce-caught-up.md), and an
IETF count is [another](/quest/m1/ietf-announce-count.md).
