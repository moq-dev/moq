# [L] A TLA+ model shows receive credit never deadlocks

## Goal

A TLA+ model of one connection's flow control, run by TLC in CI like the
other `rs/moq-net/verify` models, checks every ordering of arrivals, reads,
pauses, and evictions for two properties:

- Safety: the credit every stream holds together (unread bytes, leased bytes
  not yet released, unfinished frames, paused allowances) never exceeds the
  connection window, and paused credit never exceeds its cap.
- Liveness: under weak fairness for the reader and the peer, every stream
  eventually progresses, finishes, or is evicted, and a sender waiting for
  stream credit eventually opens or gives up its group.

It models the design as [#9210](https://github.com/moq-dev/moq/pull/9210)
leaves it, and the code matches the model, or the PR fixes it.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of the pre-cut flow-control audit):

- ✅ The cut waits on a model (rejected: restoring the overcommitted benchmark
  cells as the evidence, which only samples orderings; cutting without it).
  At least four credit bugs landed in the two weeks before the audit, two of
  them hangs: [#8794](https://github.com/moq-dev/moq/pull/8794) (a paused
  stream granted credit until it held the window),
  [#8808](https://github.com/moq-dev/moq/pull/8808) (QMux leaked receive
  credit on `release`), [#8774](https://github.com/moq-dev/moq/pull/8774)
  (a dropped group never reached the reader), and
  [#8607](https://github.com/moq-dev/moq/pull/8607) (a group waiting for
  credit held a higher-ranked one behind it).
- ✅ The model comes before #9210, which merges once the model passes on its
  design (rejected: #9210 first, or both in parallel).
- ✅ Scope: receive credit, pausing, and the sender's credit wait (rejected:
  receive side only; a relay chain end to end, since pausing is per hop).

What to model:

- Receive, `rs/moq-quic/src/connection/streams/lease.rs`: the connection and
  stream windows, leases and their release at a segment seal, and
  `evict_if_stuck` with its two waits: an evicted reader that has not stopped
  its stream (`evicted_held`), and data a reader has not looked at
  (`trim_unlooked`). The second is the likeliest hang: it holds eviction off
  for as long as any stream has data no reader looked at, so liveness depends
  on moq-net always polling every stream it accepted, including ones not yet
  accepted.
- Pausing, `rs/moq-net/src/transport/boxed.rs` and `model/pace.rs`: the
  half-window cap, #9210's slot allowances (`cap / ((i + 1) * (i + 2))`) and
  `set_window`, the pause only at a segment seal, and the read-on-and-shed
  path past the cap.
- Send, `rs/moq-net/src/lite/publisher.rs` `TrackRun::opening` and `stuck`,
  and `transport/open.rs`: a group waiting for stream credit, by order and
  rank.

Abstract bytes into a few units, and keep frames, streams, and slots small
enough for TLC to run in the merge gate, as the other models do; a larger
configuration can run nightly. Add a `verify-credit` skill beside
`verify-subscribe` and the others, naming the code each action maps to.

A violation in main's code is fixed in this PR. A violation in #9210's
design goes to #9210 before it merges.

Public API: none. Wire: none.

## Related

- [Unsealed frame credit](/quest/m4/unsealed-frame-credit.md) - returns credit as bytes are read, which the model must then cover
- [Overcommitted receive-window cells](/quest/m3/receive-windows-evicted.md) - the benchmark side of the same mechanism
- [Send-credit stall counters](/quest/m3/send-credit-stalls.md) - what an operator sees when the sender waits on credit
