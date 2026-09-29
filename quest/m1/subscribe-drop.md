# [L] SUBSCRIBE_DROP accounts for every group

## Goal

A lite subscriber can tell "not yet" from "never" for every stream group in
its subscription. Each sequence in range either arrives on a Group Stream, is
sent as a datagram, or is named by a SUBSCRIBE_DROP, including sequences the
publisher skipped. Only a lost datagram stays unaccounted. Rust and
`@moq/net` publishers send it on every lite version that has it, and moq-lite-07
brings it back in place of `Stream Count`.

## Plan

Today SUBSCRIBE_DROP is on the wire for lite-03 through lite-06 and the Rust
subscriber accounts for it, but no publisher sends it. lite-07 (still
`moq-lite-07-wip`, unpublished) removed it for a `Stream Count` on
SUBSCRIBE_END (#4224).

Decided:

- lite-07 restores SUBSCRIBE_DROP and removes `Stream Count`. With every
  sequence accounted for, the count is redundant; one mechanism instead of two.
  A reset whose header may be lost is covered by a DROP.
- A reliable reset ([Reliable stream reset](/quest/m1/quic/reliable-reset.md))
  that keeps the stream header acts as a one-group drop, an optimization over
  sending the DROP.
- Publishers send SUBSCRIBE_DROP on lite-03 through lite-06 too: for every group
  in range they won't deliver (expired, deprioritized, or reset without its
  header delivered) and for every explicit gap. Publishers that skip sequences
  (`cut` and group discontinuities in the media layers) must mark the gap so
  the net layer can drop it.
- Datagram groups stay best effort. A publisher counts a datagram as
  delivered, so a lost one leaves an uncovered hole that waits out the tail
  grace, as today.
- A resumed group ([Resumed groups](/quest/m1/resume-latest.md)) that is the
  new copy's latest ends with the DROP's error when the copy drops it.

Update `drafts/draft-lcurley-moq-lite.md` (SUBSCRIBE_DROP, SUBSCRIBE_END, the
lite-07 changelog), `doc/concept/moq-lite.md`, and the Rust and JS lite
publishers, subscribers, and tail accounting. Run `just drafts check` and
`just test interop --all`.

Regression tests: a publisher that expires a group, skips a sequence, and
resets a stream before its header; on each version the subscriber settles
without waiting out the grace.

## Related

- [Track tail interop](/quest/m1/track-tail-interop.md) - the Rust-JS proof of the lite-07 drop case
