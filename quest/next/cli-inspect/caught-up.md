# [M] An announce consumer knows when it has caught up

## Goal

A Rust consumer of `origin::Consumer::announced()` learns, once, that the
routes live at subscribe time have all been delivered, so "list what is live"
needs no guess. Today the stream replays the current routes and continues live
with no boundary, and moq-cli's `--broadcast` completion
(`rs/moq-cli/src/complete.rs`) stops on a 30ms settle / 500ms budget timer
instead.

## Plan

- The fact is per announce producer, but consumers read a merged origin. Each
  producer (a session's subscriber, a cluster peer) reports when its initial
  set has landed; an in-process producer is caught up immediately. The
  consumer's caught-up signal fires once every producer present at subscribe
  time has reported. A producer that closes before reporting drops out of
  the pending set, so a dead session cannot stall the signal. The API shape (an event kind, an awaitable, or a flag) is
  the implementer's call; propose it in the PR.
- The wire already carries the boundary: `AnnounceOk.active` on lite-05+ and
  `AnnounceInit` on lite-01/02. Versions without one (lite-03/04, IETF) fall
  back to the settle timer completion uses today, owned in one place rather
  than by each caller.
- Move completion onto the signal; the timer survives only for marker-less
  versions.
- Test: a relay with N announced broadcasts signals caught up after exactly N
  updates on each lite version that carries a count, and an empty relay
  signals immediately, and a session that closes before its count arrives
  does not block the signal.

Public API: additive on moq-net. Wire: none. JS parity is
[a separate quest](/quest/next/js-announce-caught-up.md).
